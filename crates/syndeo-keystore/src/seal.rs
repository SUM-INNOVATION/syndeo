//! Sealing the root secret.
//!
//! Two independent layers, and both must be broken:
//!
//! 1. **Passphrase layer.** Argon2id over the user's passphrase, tuned at setup
//!    to roughly a quarter of a second on this machine, then XChaCha20-Poly1305
//!    over the seed itself.
//! 2. **Wrapping layer.** A random key held by the operating system — Keychain,
//!    Secret Service, or Credential Manager — and XChaCha20-Poly1305 again over
//!    the result of layer one.
//!
//! The order matters. Layer one is on the inside, so a compromised Keychain
//! yields a blob that is still passphrase-protected.
//!
//! The seed never exists as plaintext on disk at any point, including
//! transiently: the file is written already sealed, to a temporary name in the
//! same directory, and renamed.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("the passphrase is wrong, or the sealed seed has been altered")]
    Unseal,
    #[error("this build cannot read a version {0} sealed seed")]
    Version(u32),
    #[error("malformed sealed seed: {0}")]
    Malformed(String),
    #[error("argon2: {0}")]
    Kdf(String),
    #[error("a passphrase is required to unseal this seed")]
    PassphraseRequired,
    #[error("this seed was sealed without a passphrase")]
    UnexpectedPassphrase,
}

pub type Result<T> = std::result::Result<T, SealError>;

const VERSION: u32 = 1;
/// Bound into the AEAD so a blob cannot be replayed into a different context.
const AAD: &[u8] = b"syndeo-keystore-seed-v1";

/// Argon2id cost, recorded in the blob so a seed sealed on one machine can be
/// opened on a slower one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory, in KiB.
    pub m_cost: u32,
    /// Iterations.
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        // 64 MiB and three passes lands near a quarter second on current laptop
        // hardware, and is memory-hard enough that a GPU array does not help much.
        KdfParams {
            m_cost: 64 * 1024,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

impl KdfParams {
    fn build(&self) -> Result<Argon2<'static>> {
        let params = Params::new(self.m_cost, self.t_cost, self.p_cost, Some(32))
            .map_err(|e| SealError::Kdf(e.to_string()))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    /// Pick an iteration count that takes about `target_ms` on this machine, at
    /// the default memory cost. Measured, not guessed, and measured once: the
    /// machine does not get faster between two calls in the same process.
    pub fn calibrate(target_ms: u128) -> Self {
        static CACHED: std::sync::OnceLock<KdfParams> = std::sync::OnceLock::new();
        *CACHED.get_or_init(|| Self::measure(target_ms))
    }

    fn measure(target_ms: u128) -> Self {
        let mut params = KdfParams::default();
        let salt = [0u8; 16];
        let mut out = Zeroizing::new([0u8; 32]);
        let mut best = params;
        for t_cost in 1..=10u32 {
            params.t_cost = t_cost;
            let Ok(argon) = params.build() else { break };
            let started = std::time::Instant::now();
            if argon
                .hash_password_into(b"calibration", &salt, out.as_mut())
                .is_err()
            {
                break;
            }
            let elapsed = started.elapsed().as_millis();
            best = params;
            if elapsed >= target_ms {
                break;
            }
        }
        best
    }
}

/// What lands on disk. Everything here is public except by construction: the
/// ciphertext is useless without both the wrapping key and the passphrase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedSeed {
    pub version: u32,
    /// Whether layer one is present.
    pub passphrase_layer: bool,
    pub kdf: KdfParams,
    pub kdf_salt: String,
    pub inner_nonce: String,
    pub outer_nonce: String,
    pub ciphertext: String,
    /// The address of the first origin-independent key, so the shell can show
    /// which identity this file holds without unsealing it.
    pub identity_hint: String,
}

impl SealedSeed {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("sealed seed is serializable")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let sealed: SealedSeed =
            serde_json::from_slice(bytes).map_err(|e| SealError::Malformed(e.to_string()))?;
        if sealed.version != VERSION {
            return Err(SealError::Version(sealed.version));
        }
        Ok(sealed)
    }
}

/// Seal a seed. `wrapping_key` comes from the operating system's credential
/// store; `passphrase` is the user's second factor.
pub fn seal(
    seed: &[u8],
    wrapping_key: &[u8; 32],
    passphrase: Option<&str>,
    kdf: KdfParams,
    identity_hint: String,
) -> Result<SealedSeed> {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let mut inner_nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut inner_nonce);
    let mut outer_nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut outer_nonce);

    // Layer one, innermost: the passphrase.
    let inner = match passphrase {
        Some(passphrase) => {
            let key = derive_key(passphrase, &salt, kdf)?;
            Zeroizing::new(encrypt(&key, &inner_nonce, seed)?)
        }
        None => Zeroizing::new(seed.to_vec()),
    };

    // Layer two: the wrapping key the operating system holds.
    let outer = encrypt(wrapping_key, &outer_nonce, &inner)?;

    Ok(SealedSeed {
        version: VERSION,
        passphrase_layer: passphrase.is_some(),
        kdf,
        kdf_salt: hex::encode(salt),
        inner_nonce: hex::encode(inner_nonce),
        outer_nonce: hex::encode(outer_nonce),
        ciphertext: hex::encode(outer),
        identity_hint,
    })
}

/// Unseal a seed. The returned buffer zeroizes on drop.
pub fn unseal(
    sealed: &SealedSeed,
    wrapping_key: &[u8; 32],
    passphrase: Option<&str>,
) -> Result<Zeroizing<Vec<u8>>> {
    if sealed.passphrase_layer && passphrase.is_none() {
        return Err(SealError::PassphraseRequired);
    }
    if !sealed.passphrase_layer && passphrase.is_some() {
        return Err(SealError::UnexpectedPassphrase);
    }

    let outer_nonce = decode_nonce(&sealed.outer_nonce)?;
    let ciphertext =
        hex::decode(&sealed.ciphertext).map_err(|e| SealError::Malformed(e.to_string()))?;
    let inner = Zeroizing::new(decrypt(wrapping_key, &outer_nonce, &ciphertext)?);

    match passphrase {
        Some(passphrase) => {
            let salt =
                hex::decode(&sealed.kdf_salt).map_err(|e| SealError::Malformed(e.to_string()))?;
            let inner_nonce = decode_nonce(&sealed.inner_nonce)?;
            let key = derive_key(passphrase, &salt, sealed.kdf)?;
            Ok(Zeroizing::new(decrypt(&key, &inner_nonce, &inner)?))
        }
        None => Ok(inner),
    }
}

/// Argon2id. The output zeroizes on drop, including the intermediate buffer.
fn derive_key(passphrase: &str, salt: &[u8], kdf: KdfParams) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    kdf.build()?
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| SealError::Kdf(e.to_string()))?;
    Ok(key)
}

fn encrypt(key: &[u8; 32], nonce: &[u8; 24], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .encrypt(
            XNonce::from_slice(nonce),
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad: AAD,
            },
        )
        .map_err(|_| SealError::Unseal)
}

fn decrypt(key: &[u8; 32], nonce: &[u8; 24], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            chacha20poly1305::aead::Payload {
                msg: ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| SealError::Unseal)
}

fn decode_nonce(hex_value: &str) -> Result<[u8; 24]> {
    let bytes = hex::decode(hex_value).map_err(|e| SealError::Malformed(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| SealError::Malformed("nonce is not 24 bytes".into()))
}

/// A wrapping key, generated once and then held by the operating system.
pub fn generate_wrapping_key() -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(key.as_mut());
    key
}

/// Zeroize a passphrase the moment it stops being needed.
pub fn forget(mut passphrase: String) {
    passphrase.zeroize();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters: these tests are about the construction, not the cost.
    fn fast() -> KdfParams {
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    const SEED: &[u8] = b"a sixty four byte bip39 seed would go here, this stands in ok!!!";

    #[test]
    fn a_sealed_seed_round_trips_with_both_factors() {
        let wrapping = generate_wrapping_key();
        let sealed = seal(SEED, &wrapping, Some("correct horse"), fast(), "addr".into()).unwrap();
        let opened = unseal(&sealed, &wrapping, Some("correct horse")).unwrap();
        assert_eq!(&opened[..], SEED);
    }

    #[test]
    fn the_plaintext_seed_never_appears_in_the_sealed_bytes() {
        let wrapping = generate_wrapping_key();
        const PASSPHRASE: &str = "correct-horse-battery-staple";
        let sealed = seal(SEED, &wrapping, Some(PASSPHRASE), fast(), "addr".into()).unwrap();
        let bytes = sealed.to_bytes();
        assert!(
            bytes.windows(SEED.len()).all(|w| w != SEED),
            "the seed leaked into the file"
        );
        assert!(!String::from_utf8_lossy(&bytes).contains(PASSPHRASE));
    }

    #[test]
    fn the_wrapping_key_alone_is_not_enough() {
        let wrapping = generate_wrapping_key();
        let sealed = seal(SEED, &wrapping, Some("pass"), fast(), "addr".into()).unwrap();
        // Exactly the compromised-Keychain case: attacker has the wrapping key.
        assert!(matches!(
            unseal(&sealed, &wrapping, Some("wrong")),
            Err(SealError::Unseal)
        ));
        assert!(matches!(
            unseal(&sealed, &wrapping, None),
            Err(SealError::PassphraseRequired)
        ));
    }

    #[test]
    fn the_passphrase_alone_is_not_enough() {
        let wrapping = generate_wrapping_key();
        let other = generate_wrapping_key();
        let sealed = seal(SEED, &wrapping, Some("pass"), fast(), "addr".into()).unwrap();
        assert!(matches!(
            unseal(&sealed, &other, Some("pass")),
            Err(SealError::Unseal)
        ));
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let wrapping = generate_wrapping_key();
        let mut sealed = seal(SEED, &wrapping, Some("pass"), fast(), "addr".into()).unwrap();
        let mut bytes = hex::decode(&sealed.ciphertext).unwrap();
        bytes[0] ^= 0x01;
        sealed.ciphertext = hex::encode(bytes);
        assert!(matches!(
            unseal(&sealed, &wrapping, Some("pass")),
            Err(SealError::Unseal)
        ));
    }

    #[test]
    fn each_sealing_uses_fresh_randomness() {
        let wrapping = generate_wrapping_key();
        let a = seal(SEED, &wrapping, Some("pass"), fast(), "addr".into()).unwrap();
        let b = seal(SEED, &wrapping, Some("pass"), fast(), "addr".into()).unwrap();
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_ne!(a.kdf_salt, b.kdf_salt);
        assert_ne!(a.outer_nonce, b.outer_nonce);
    }

    #[test]
    fn a_seed_sealed_without_a_passphrase_still_needs_the_wrapping_key() {
        let wrapping = generate_wrapping_key();
        let other = generate_wrapping_key();
        let sealed = seal(SEED, &wrapping, None, fast(), "addr".into()).unwrap();
        assert_eq!(&unseal(&sealed, &wrapping, None).unwrap()[..], SEED);
        assert!(unseal(&sealed, &other, None).is_err());
    }

    #[test]
    fn the_blob_survives_a_disk_round_trip() {
        let wrapping = generate_wrapping_key();
        let sealed = seal(SEED, &wrapping, Some("pass"), fast(), "addr".into()).unwrap();
        let reloaded = SealedSeed::from_bytes(&sealed.to_bytes()).unwrap();
        assert_eq!(&unseal(&reloaded, &wrapping, Some("pass")).unwrap()[..], SEED);
    }

    #[test]
    fn calibration_picks_something_usable() {
        let params = KdfParams::calibrate(10);
        assert!(params.t_cost >= 1 && params.t_cost <= 10);
        assert!(params.build().is_ok());
    }
}
