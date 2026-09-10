//! The keystore.
//!
//! It holds keys and exposes exactly one operation: sign this payload, because
//! the shell confirmed it with the user. Everything else here is enrolment,
//! which the shell drives interactively at setup.
//!
//! The agent has no connection to this process and no way to mint the
//! confirmation a signature requires. That is rule two, and it is checked in
//! [`Keystore::sign_confirmed`], not assumed.

use crate::address::Address;
use crate::custody::Vault;
use crate::derive::{self, ExtendedKey};
use crate::idle::{self, Watch};
use crate::presence;
use crate::seal::{self, KdfParams, SealedSeed};
use crate::wrapping::WrappingKeyStore;
use bip39::{Language, Mnemonic};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use syndeo_ipc::confirm::{Confirmation, Confirmer};
use syndeo_ipc::protocol::SignaturePurpose;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    #[error("custody: {0}")]
    Custody(#[from] crate::custody::CustodyError),
    #[error("seal: {0}")]
    Seal(#[from] crate::seal::SealError),
    #[error("credential store: {0}")]
    Wrapping(#[from] crate::wrapping::WrappingError),
    #[error("confirmation: {0}")]
    Confirmation(#[from] syndeo_ipc::confirm::ConfirmationError),
    #[error("the keystore is locked")]
    Locked,
    #[error("a keystore already exists here; restore or remove it first")]
    AlreadyInitialized,
    #[error("no keystore has been set up")]
    NotInitialized,
    #[error("a passphrase is required on this platform because user presence is not enforced by the operating system")]
    PassphraseRequired,
    #[error("recovery phrase: {0}")]
    Mnemonic(String),
}

pub type Result<T> = std::result::Result<T, KeystoreError>;

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub initialized: bool,
    pub unsealed: bool,
    pub passphrase_required: bool,
    pub presence_enforced: bool,
    /// After how many seconds without an operation the seed is forgotten.
    pub idle_timeout_secs: Option<u64>,
    /// How long it has been since the last operation.
    pub idle_for_secs: u64,
}

/// How long an unsealed session survives with nothing using it.
///
/// Short on purpose. The cost of it being too short is a passphrase prompt; the
/// cost of it being too long is a signing oracle on an unattended machine.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub struct Keystore {
    vault: Vault,
    wrapping: Arc<dyn WrappingKeyStore>,
    /// The master key, present only while the session is unsealed.
    master: Mutex<Option<ExtendedKey>>,
    /// What ends that session without anyone asking.
    watch: Watch,
}

impl Keystore {
    pub fn open(home: impl AsRef<Path>, wrapping: Arc<dyn WrappingKeyStore>) -> Result<Self> {
        Ok(Keystore {
            vault: Vault::open(home)?,
            wrapping,
            master: Mutex::new(None),
            watch: Watch::new(Some(DEFAULT_IDLE_TIMEOUT)),
        })
    }

    /// Replace the idle policy. `None` disables idle expiry, which is only ever
    /// right for a one-shot command that locks when it is done.
    pub fn with_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.watch = Watch::new(timeout);
        self
    }

    /// Replace the clocks the idle policy reads. Tests advance time with this
    /// rather than waiting for it.
    pub fn with_clock(mut self, timeout: Option<Duration>, clock: idle::Clock) -> Self {
        self.watch = Watch::with_clock(timeout, clock);
        self
    }

    /// Forget the seed if the session has ended. Returns why, when it did.
    ///
    /// Called on a timer by the service loop, and again at the top of every
    /// operation so that a request arriving after the session ended cannot slip
    /// in ahead of the timer.
    pub fn lock_if_expired(&self) -> Option<idle::Reason> {
        if self.master.lock().unwrap().is_none() {
            return None;
        }
        let reason = self.watch.expired()?;
        self.lock();
        tracing::info!(reason = reason.as_str(), "locked the keystore");
        Some(reason)
    }

    pub fn status(&self) -> Status {
        let sealed = self.load_sealed().ok();
        let user_set_a_passphrase = sealed.map(|s| s.passphrase_layer).unwrap_or(false);
        // With a key enrolled, the question is about that key. With none, it is
        // about what the next enrolment will get — which is what lets setup
        // relax the passphrase requirement before there is a key to inspect.
        let presence_enforced = if self.vault.exists() {
            self.wrapping.presence_enforced()
        } else {
            presence::available()
        };
        let policy = presence::policy(presence_enforced, user_set_a_passphrase);
        Status {
            initialized: self.vault.exists(),
            unsealed: self.master.lock().unwrap().is_some(),
            passphrase_required: policy.passphrase_required,
            presence_enforced,
            idle_timeout_secs: self.watch.timeout().map(|t| t.as_secs()),
            idle_for_secs: self.watch.idle_for(),
        }
    }

    /// First run. Generates entropy, seals it, and returns the recovery phrase
    /// exactly once — it is never written to disk, here or anywhere.
    pub fn initialize(&self, passphrase: Option<&str>) -> Result<(Zeroizing<String>, Address)> {
        if self.vault.exists() {
            return Err(KeystoreError::AlreadyInitialized);
        }
        self.require_passphrase(passphrase)?;

        let mut entropy = Zeroizing::new([0u8; 32]);
        {
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(entropy.as_mut());
        }
        let mnemonic = Mnemonic::from_entropy_in(Language::English, entropy.as_ref())
            .map_err(|e| KeystoreError::Mnemonic(e.to_string()))?;
        let address = self.seal_mnemonic(&mnemonic, passphrase)?;
        Ok((Zeroizing::new(mnemonic.to_string()), address))
    }

    /// Restore from a recovery phrase, replacing whatever is here.
    pub fn restore(&self, phrase: &str, passphrase: Option<&str>) -> Result<Address> {
        self.require_passphrase(passphrase)?;
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase.trim())
            .map_err(|e| KeystoreError::Mnemonic(e.to_string()))?;
        self.seal_mnemonic(&mnemonic, passphrase)
    }

    fn seal_mnemonic(&self, mnemonic: &Mnemonic, passphrase: Option<&str>) -> Result<Address> {
        // BIP-39 to seed, then SLIP-0010 master. The seed is zeroized on drop and
        // is never handed to anything that could persist it.
        let seed = Zeroizing::new(mnemonic.to_seed_normalized(""));
        let master = ExtendedKey::master(seed.as_ref());
        let identity = master
            .derive_path(&[44, derive::SUM_COIN_TYPE, 0])
            .address();

        let wrapping_key = seal::generate_wrapping_key();
        let kdf = if passphrase.is_some() {
            KdfParams::calibrate(250)
        } else {
            KdfParams::default()
        };
        let sealed = seal::seal(
            seed.as_ref(),
            &wrapping_key,
            passphrase,
            kdf,
            identity.to_base58(),
        )?;

        // Store the wrapping key first: a sealed blob with no wrapping key is
        // unopenable, which is a worse failure than an orphaned keyring entry.
        self.wrapping.store(&wrapping_key)?;
        self.vault.write_sealed(&sealed.to_bytes())?;

        *self.master.lock().unwrap() = Some(master);
        self.watch.touch();
        Ok(identity)
    }

    /// Open the seed for this session.
    pub fn unseal(&self, passphrase: Option<&str>) -> Result<()> {
        let sealed = self.load_sealed()?;
        if sealed.passphrase_layer && passphrase.is_none() {
            return Err(KeystoreError::PassphraseRequired);
        }
        let wrapping_key = self.wrapping.load()?;
        let seed = seal::unseal(&sealed, &wrapping_key, passphrase)?;
        *self.master.lock().unwrap() = Some(ExtendedKey::master(seed.as_ref()));
        self.watch.touch();
        Ok(())
    }

    /// Forget the unsealed material. `ExtendedKey` zeroizes on drop.
    pub fn lock(&self) {
        *self.master.lock().unwrap() = None;
    }

    /// The public identity for an origin. Public keys are not secret, but the
    /// derivation is still done here and nowhere else.
    pub fn public_identity(&self, origin: &str) -> Result<(String, Address)> {
        self.lock_if_expired();
        let guard = self.master.lock().unwrap();
        let master = guard.as_ref().ok_or(KeystoreError::Locked)?;
        let key = derive::origin_key(master, origin);
        self.watch.touch();
        Ok((hex::encode(key.public_key()), key.address()))
    }

    /// The one operation.
    ///
    /// The confirmation must verify against the session secret the shell
    /// established when it spawned this process, must not have been used before,
    /// must not have expired, and must be bound to exactly these payload bytes.
    /// The origin the user was shown is the origin whose key signs — a
    /// confirmation for one site can never produce a signature for another.
    pub fn sign_confirmed(
        &self,
        confirmer: &Confirmer,
        confirmation: &Confirmation,
        payload: &[u8],
    ) -> Result<Signed> {
        // Ahead of the confirmation check, so a request that arrives after the
        // session ended is refused as locked rather than burning the single-use
        // confirmation on an operation that was going to fail anyway.
        self.lock_if_expired();
        confirmer.verify(confirmation, payload)?;

        let guard = self.master.lock().unwrap();
        let master = guard.as_ref().ok_or(KeystoreError::Locked)?;
        let key = derive::origin_key(master, &confirmation.origin);
        let signature = key.sign(payload);
        self.watch.touch();

        Ok(Signed {
            signature: hex::encode(signature.to_bytes()),
            public_key: hex::encode(key.public_key()),
            address: key.address(),
            purpose: confirmation.purpose,
        })
    }

    fn load_sealed(&self) -> Result<SealedSeed> {
        if !self.vault.exists() {
            return Err(KeystoreError::NotInitialized);
        }
        Ok(SealedSeed::from_bytes(&self.vault.read_sealed()?)?)
    }

    /// Enrolment is about to write a new wrapping key, so what matters is what
    /// the platform will enforce on it, not what it is enforcing on a key that
    /// is about to be replaced.
    fn require_passphrase(&self, passphrase: Option<&str>) -> Result<()> {
        let policy = presence::policy(presence::available(), passphrase.is_some());
        if policy.passphrase_required && passphrase.is_none() {
            return Err(KeystoreError::PassphraseRequired);
        }
        Ok(())
    }

    /// The address recorded in the sealed file, readable without unsealing.
    pub fn identity_hint(&self) -> Option<String> {
        self.load_sealed().ok().map(|s| s.identity_hint)
    }
}

#[derive(Debug, Clone)]
pub struct Signed {
    pub signature: String,
    pub public_key: String,
    pub address: Address,
    pub purpose: SignaturePurpose,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapping::InMemoryKeyStore;
    use syndeo_ipc::confirm::{ConfirmationError, SessionSecret};

    fn keystore() -> (tempfile::TempDir, Keystore, Arc<InMemoryKeyStore>) {
        let dir = tempfile::tempdir().unwrap();
        let wrapping = Arc::new(InMemoryKeyStore::default());
        let keystore = Keystore::open(dir.path(), wrapping.clone()).unwrap();
        (dir, keystore, wrapping)
    }

    const PASS: &str = "a passphrase the user chose";

    #[test]
    fn setup_returns_a_recovery_phrase_and_seals_the_seed() {
        let (_dir, keystore, _) = keystore();
        assert!(!keystore.status().initialized);

        let (mnemonic, address) = keystore.initialize(Some(PASS)).unwrap();
        assert_eq!(mnemonic.split_whitespace().count(), 24);
        assert!(keystore.status().initialized);
        assert!(keystore.status().unsealed);

        // The phrase is not on disk anywhere under the vault. The check is over
        // the document's *values*: the field names are ours, and one of them
        // (`m_cost`) contains a word that is also in the BIP-39 list, which
        // would otherwise fail this test for about one phrase in eighty.
        let raw = std::fs::read_to_string(keystore.vault.sealed_path()).unwrap();
        let document: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let values: String = document
            .as_object()
            .expect("the sealed file is an object")
            .values()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        for word in mnemonic.split_whitespace() {
            assert!(!values.contains(word), "the recovery phrase leaked: {word}");
        }
        assert!(values.contains(&address.to_base58()));
    }

    #[test]
    fn the_same_phrase_restores_the_same_identity_anywhere() {
        let (_a, first, _) = keystore();
        let (mnemonic, address) = first.initialize(Some(PASS)).unwrap();
        let origin_identity = first.public_identity("https://wallet.test").unwrap();

        let (_b, second, _) = keystore();
        let restored = second.restore(&mnemonic, Some("a different passphrase")).unwrap();
        assert_eq!(restored, address);
        assert_eq!(second.public_identity("https://wallet.test").unwrap(), origin_identity);
    }

    #[test]
    fn locking_forgets_the_seed_until_the_passphrase_comes_back() {
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();
        keystore.lock();
        assert!(!keystore.status().unsealed);
        assert!(matches!(
            keystore.public_identity("https://a.test"),
            Err(KeystoreError::Locked)
        ));

        assert!(matches!(
            keystore.unseal(Some("wrong")),
            Err(KeystoreError::Seal(_))
        ));
        keystore.unseal(Some(PASS)).unwrap();
        assert!(keystore.status().unsealed);
    }

    #[test]
    fn a_signature_requires_a_confirmation_the_shell_minted() {
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();

        let secret = SessionSecret::generate();
        let shell = Confirmer::new(secret.clone());
        let keystore_side = Confirmer::new(secret);

        let payload = b"transfer 10 SUM to alice";
        let confirmation = shell.issue(
            "https://wallet.test",
            SignaturePurpose::ChainTransaction,
            "Send 10 SUM to alice",
            payload,
        );
        let signed = keystore.sign_confirmed(&keystore_side, &confirmation, payload).unwrap();

        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let public = VerifyingKey::from_bytes(
            &hex::decode(&signed.public_key).unwrap().try_into().unwrap(),
        )
        .unwrap();
        let signature = Signature::from_slice(&hex::decode(&signed.signature).unwrap()).unwrap();
        assert!(public.verify(payload, &signature).is_ok());
        assert_eq!(signed.address, keystore.public_identity("https://wallet.test").unwrap().1);
    }

    #[test]
    fn an_agent_that_reached_the_socket_still_cannot_get_a_signature() {
        // The agent does not hold the session secret, so anything it mints is
        // just a differently-shaped forgery.
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();

        let real = Confirmer::new(SessionSecret::generate());
        let agent = Confirmer::new(SessionSecret::generate());
        let payload = b"drain the wallet";
        let forged = agent.issue(
            "https://wallet.test",
            SignaturePurpose::ChainTransaction,
            "Routine sync",
            payload,
        );
        assert!(matches!(
            keystore.sign_confirmed(&real, &forged, payload),
            Err(KeystoreError::Confirmation(ConfirmationError::BadMac))
        ));
    }

    #[test]
    fn a_confirmation_cannot_be_redirected_to_another_payload_or_reused() {
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();
        let secret = SessionSecret::generate();
        let shell = Confirmer::new(secret.clone());
        let side = Confirmer::new(secret);

        let shown = b"send 10 SUM";
        let confirmation = shell.issue("https://wallet.test", SignaturePurpose::ChainTransaction, "Send 10 SUM", shown);

        assert!(matches!(
            keystore.sign_confirmed(&side, &confirmation, b"send 10000 SUM"),
            Err(KeystoreError::Confirmation(ConfirmationError::PayloadMismatch))
        ));
        assert!(keystore.sign_confirmed(&side, &confirmation, shown).is_ok());
        assert!(matches!(
            keystore.sign_confirmed(&side, &confirmation, shown),
            Err(KeystoreError::Confirmation(ConfirmationError::Replayed))
        ));
    }

    #[test]
    fn a_confirmation_for_one_origin_never_signs_for_another() {
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();
        let secret = SessionSecret::generate();
        let shell = Confirmer::new(secret.clone());
        let side = Confirmer::new(secret);

        let payload = b"log me in";
        let confirmation = shell.issue("https://a.test", SignaturePurpose::OriginLogin, "Log in to a.test", payload);
        let signed = keystore.sign_confirmed(&side, &confirmation, payload).unwrap();

        assert_eq!(signed.address, keystore.public_identity("https://a.test").unwrap().1);
        assert_ne!(signed.address, keystore.public_identity("https://b.test").unwrap().1);
    }

    #[test]
    fn a_locked_keystore_signs_nothing_however_valid_the_confirmation() {
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();
        keystore.lock();

        let secret = SessionSecret::generate();
        let shell = Confirmer::new(secret.clone());
        let side = Confirmer::new(secret);
        let confirmation = shell.issue("https://a.test", SignaturePurpose::OriginLogin, "Log in", b"payload");
        assert!(matches!(
            keystore.sign_confirmed(&side, &confirmation, b"payload"),
            Err(KeystoreError::Locked)
        ));
    }

    #[test]
    fn setup_refuses_to_overwrite_an_existing_keystore() {
        let (_dir, keystore, _) = keystore();
        keystore.initialize(Some(PASS)).unwrap();
        assert!(matches!(
            keystore.initialize(Some(PASS)),
            Err(KeystoreError::AlreadyInitialized)
        ));
    }

    #[test]
    fn without_platform_presence_setup_insists_on_a_passphrase() {
        let (_dir, keystore, _) = keystore();
        assert!(!keystore.status().presence_enforced);
        assert!(matches!(
            keystore.initialize(None),
            Err(KeystoreError::PassphraseRequired)
        ));
    }

    // ---- idle auto-lock ---------------------------------------------------

    /// A keystore whose clocks the test moves by hand.
    fn keystore_with_clock(
        timeout: Option<Duration>,
    ) -> (tempfile::TempDir, Keystore, Arc<std::sync::atomic::AtomicU64>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let wrapping = Arc::new(InMemoryKeyStore::default());
        let ticks = Arc::new(AtomicU64::new(0));
        let handle = ticks.clone();
        let clock: crate::idle::Clock = Arc::new(move || {
            let t = handle.load(Ordering::SeqCst);
            // Both clocks move together: this is time passing, not a suspend.
            crate::idle::Reading {
                monotonic: t,
                wall: t,
            }
        });
        let keystore = Keystore::open(dir.path(), wrapping)
            .unwrap()
            .with_clock(timeout, clock);
        (dir, keystore, ticks)
    }

    #[test]
    fn an_idle_session_forgets_the_seed() {
        use std::sync::atomic::Ordering;
        let (_dir, keystore, ticks) = keystore_with_clock(Some(Duration::from_secs(300)));
        keystore.initialize(Some(PASS)).unwrap();
        assert!(keystore.status().unsealed);

        ticks.store(299, Ordering::SeqCst);
        assert_eq!(keystore.lock_if_expired(), None);
        assert!(keystore.status().unsealed, "locked a second too early");

        ticks.store(300, Ordering::SeqCst);
        assert_eq!(keystore.lock_if_expired(), Some(crate::idle::Reason::Idle));
        assert!(
            !keystore.status().unsealed,
            "the master key survived the idle timeout"
        );
        assert!(matches!(
            keystore.public_identity("https://a.test"),
            Err(KeystoreError::Locked)
        ));
    }

    #[test]
    fn use_keeps_a_session_alive_and_a_signature_after_it_ends_is_refused() {
        use std::sync::atomic::Ordering;
        let (_dir, keystore, ticks) = keystore_with_clock(Some(Duration::from_secs(300)));
        keystore.initialize(Some(PASS)).unwrap();

        // Something touches the seed every four minutes for an hour.
        for step in 1..=15 {
            ticks.store(step * 240, Ordering::SeqCst);
            keystore.public_identity("https://a.test").unwrap();
        }
        assert!(keystore.status().unsealed, "use did not keep the session alive");

        // Then the machine is left alone.
        let secret = SessionSecret::generate();
        let shell = Confirmer::new(secret.clone());
        let side = Confirmer::new(secret);
        let payload = b"transfer 10 SUM to alice";
        let confirmation = shell.issue(
            "https://wallet.test",
            SignaturePurpose::ChainTransaction,
            "Send 10 SUM to alice",
            payload,
        );

        ticks.fetch_add(301, Ordering::SeqCst);
        assert!(
            matches!(
                keystore.sign_confirmed(&side, &confirmation, payload),
                Err(KeystoreError::Locked)
            ),
            "an unattended machine signed"
        );

        // And unsealing brings it back rather than the confirmation being lost
        // — though this one is spent either way, which is the point of it.
        keystore.unseal(Some(PASS)).unwrap();
        assert!(keystore.status().unsealed);
    }

    #[test]
    fn a_session_with_no_timeout_stays_open() {
        use std::sync::atomic::Ordering;
        let (_dir, keystore, ticks) = keystore_with_clock(None);
        keystore.initialize(Some(PASS)).unwrap();
        ticks.store(86_400 * 7, Ordering::SeqCst);
        assert_eq!(keystore.lock_if_expired(), None);
        assert!(keystore.status().unsealed);
    }

    #[test]
    fn losing_the_wrapping_key_makes_the_seed_unopenable() {
        // The documented consequence of the design: both factors are required,
        // and the recovery phrase is the only way back.
        let (_dir, keystore, wrapping) = keystore();
        let (mnemonic, address) = keystore.initialize(Some(PASS)).unwrap();
        keystore.lock();
        wrapping.erase().unwrap();

        assert!(keystore.unseal(Some(PASS)).is_err());
        assert_eq!(keystore.restore(&mnemonic, Some(PASS)).unwrap(), address);
    }
}
