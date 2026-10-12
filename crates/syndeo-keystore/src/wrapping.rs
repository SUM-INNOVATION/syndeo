//! Where the wrapping key lives.
//!
//! On macOS that is the Keychain, on Linux the Secret Service, on Windows the
//! Credential Manager — never a file we wrote ourselves. The trait exists so the
//! tests do not touch the user's real credential store, and so the
//! presence-enforced Security.framework path slots in without `keystore.rs`
//! changing at all.
//!
//! On macOS [`OsKeyring`] prefers [`crate::enclave`], where the item carries
//! `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` and a `SecAccessControl`
//! requiring user presence, so Touch ID is enforced by the operating system
//! rather than by us. That needs the data protection keychain, which needs a
//! keychain access group entitlement backed by a provisioning profile. Syndeo's
//! standalone binaries have none, signed or not, so they fall back to the
//! ordinary keychain and say so, which is what keeps the passphrase mandatory
//! (see [`crate::presence`]).

use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum WrappingError {
    #[error("credential store: {0}")]
    Store(String),
    #[error("no wrapping key has been enrolled")]
    Missing,
    #[error("the stored wrapping key is malformed")]
    Malformed,
}

pub type Result<T> = std::result::Result<T, WrappingError>;

pub trait WrappingKeyStore: Send + Sync {
    fn store(&self, key: &[u8; 32]) -> Result<()>;
    fn load(&self) -> Result<Zeroizing<[u8; 32]>>;
    fn erase(&self) -> Result<()>;
    /// Whether any wrapping key is held under this store's name, wherever the
    /// store might have put it.
    ///
    /// Asked before enrolment writes a new key, because writing replaces: the
    /// entry is shared by every keystore home on the machine, so an `Ok(false)`
    /// here is permission to destroy whatever key was there. An `Err` means
    /// absence could not be shown, and callers treat it as presence. Where the
    /// store can answer from metadata alone, it must not read the key or ask
    /// the user for anything.
    fn exists(&self) -> Result<bool>;
    /// True when the operating system gates access to *this* wrapping key on
    /// user presence. A property of the key as enrolled, not of the platform:
    /// claiming it because the platform could have enforced it, when the key was
    /// actually written somewhere ungated, would be claiming a guarantee nothing
    /// is making.
    fn presence_enforced(&self) -> bool {
        false
    }
}

/// The operating system's own credential store, presence-enforced where it can
/// be.
pub struct OsKeyring {
    service: String,
    account: String,
    #[cfg(target_os = "macos")]
    enclave: crate::enclave::EnclaveItem,
}

impl OsKeyring {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        let service = service.into();
        let account = account.into();
        OsKeyring {
            #[cfg(target_os = "macos")]
            enclave: crate::enclave::EnclaveItem::new(service.clone(), account.clone()),
            service,
            account,
        }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| WrappingError::Store(e.to_string()))
    }

    /// Whether the key is enrolled where the operating system gates it.
    ///
    /// Only ever used to choose where to read from and what to report, so an
    /// answer that could not be had reads as "not gated": claiming a guarantee
    /// we could not confirm is the worse mistake of the two.
    #[cfg(target_os = "macos")]
    fn enrolled_in_enclave(&self) -> bool {
        matches!(self.enclave.exists(), Ok(true))
    }

    #[cfg(not(target_os = "macos"))]
    fn enrolled_in_enclave(&self) -> bool {
        false
    }

    /// Whether the ordinary keychain holds the entry `keyring` would read,
    /// asked for its attributes only.
    ///
    /// Same keychain as `keyring` uses (the user's default), same class,
    /// service and account, so it is the same item. The legacy keychain applies
    /// its access list when the secret is read, not when attributes are, so
    /// this does not raise the "allow access" dialog that `get_password` can.
    #[cfg(target_os = "macos")]
    fn fallback_exists(&self) -> Result<bool> {
        use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
        use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};
        use security_framework_sys::base::errSecItemNotFound;

        let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
            .map_err(|e| WrappingError::Store(e.to_string()))?;
        let found = ItemSearchOptions::new()
            .keychains(&[keychain])
            .class(ItemClass::generic_password())
            .service(&self.service)
            .account(&self.account)
            .load_attributes(true)
            .limit(Limit::Max(1))
            .search();
        match found {
            Ok(items) => Ok(!items.is_empty()),
            Err(e) if e.code() == errSecItemNotFound => Ok(false),
            Err(e) => Err(WrappingError::Store(e.to_string())),
        }
    }

    /// Whether the platform credential store holds the entry.
    ///
    /// `keyring` has no metadata-only query, so this is `get_password`, and the
    /// secret it returns is dropped (zeroized) unread. On Linux that may ask the
    /// Secret Service to unlock its collection, which is the same prompt
    /// unsealing would raise. Only `NoEntry` is absence; any other failure,
    /// including an ambiguous match, is an error, never a "no".
    #[cfg(not(target_os = "macos"))]
    fn fallback_exists(&self) -> Result<bool> {
        match self.entry()?.get_password() {
            Ok(secret) => {
                drop(Zeroizing::new(secret));
                Ok(true)
            }
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(WrappingError::Store(e.to_string())),
        }
    }
}

impl WrappingKeyStore for OsKeyring {
    fn store(&self, key: &[u8; 32]) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            if crate::enclave::EnclaveItem::presence_can_be_enforced() {
                self.enclave.store(key)?;
                // Nothing ungated may be left holding the same key: a fallback
                // copy would be a way around the presence check rather than a
                // convenience.
                let _ = self.entry().map(|e| e.delete_credential());
                return Ok(());
            }
        }

        let encoded = Zeroizing::new(hex::encode(key));
        self.entry()?
            .set_password(&encoded)
            .map_err(|e| WrappingError::Store(e.to_string()))
    }

    fn load(&self) -> Result<Zeroizing<[u8; 32]>> {
        #[cfg(target_os = "macos")]
        {
            // Asked before reading, so a build that cannot reach the enclave does
            // not put a doomed authentication prompt in front of the user.
            if self.enrolled_in_enclave() {
                return self.enclave.load();
            }
        }

        let encoded = match self.entry()?.get_password() {
            Ok(p) => Zeroizing::new(p),
            Err(keyring::Error::NoEntry) => return Err(WrappingError::Missing),
            Err(e) => return Err(WrappingError::Store(e.to_string())),
        };
        let bytes = hex::decode(encoded.as_str()).map_err(|_| WrappingError::Malformed)?;
        let array: [u8; 32] = bytes.try_into().map_err(|_| WrappingError::Malformed)?;
        Ok(Zeroizing::new(array))
    }

    fn erase(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        let _ = self.enclave.delete();

        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(WrappingError::Store(e.to_string())),
        }
    }

    /// Both places a key could be, whichever this build would write to.
    ///
    /// A build that can enrol in the enclave writes there and every other build
    /// to the ordinary keychain — every build Syndeo ships today is the latter —
    /// and either may run against a machine the other enrolled, so
    /// asking only where this build would write could miss the key it is about
    /// to orphan. The enclave is asked even when this build cannot enrol there,
    /// and whatever it answers other than "no such item" is an error, not
    /// absence.
    fn exists(&self) -> Result<bool> {
        #[cfg(target_os = "macos")]
        {
            if self.enclave.exists()? {
                return Ok(true);
            }
        }
        self.fallback_exists()
    }

    /// Whether the key *we actually hold* is gated by the operating system.
    ///
    /// Only the enclave is asked, and only about the item's existence, never its
    /// data — reading data is what triggers an authentication prompt, and this
    /// is called on every status request. A key sitting in the ordinary keychain
    /// is not gated, whatever this build could have done with a different one;
    /// re-enrolling (`syndeo-keystore restore`) moves it.
    fn presence_enforced(&self) -> bool {
        self.enrolled_in_enclave()
    }
}

/// For tests, and only for tests. Never reachable from the binary.
#[derive(Default)]
pub struct InMemoryKeyStore {
    key: std::sync::Mutex<Option<[u8; 32]>>,
    /// When set, [`WrappingKeyStore::exists`] fails, standing in for a
    /// credential store that could not be asked.
    unanswerable: std::sync::atomic::AtomicBool,
}

impl InMemoryKeyStore {
    /// Make [`WrappingKeyStore::exists`] return an error from now on, so a test
    /// can show that not knowing is treated as a key being there.
    pub fn fail_existence_checks(&self) {
        self.unanswerable
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The key as it is stored now, for comparing before and after.
    pub fn stored(&self) -> Option<[u8; 32]> {
        *self.key.lock().unwrap()
    }
}

impl WrappingKeyStore for InMemoryKeyStore {
    fn store(&self, key: &[u8; 32]) -> Result<()> {
        *self.key.lock().unwrap() = Some(*key);
        Ok(())
    }

    fn load(&self) -> Result<Zeroizing<[u8; 32]>> {
        self.key
            .lock()
            .unwrap()
            .map(Zeroizing::new)
            .ok_or(WrappingError::Missing)
    }

    fn erase(&self) -> Result<()> {
        *self.key.lock().unwrap() = None;
        Ok(())
    }

    fn exists(&self) -> Result<bool> {
        if self.unanswerable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(WrappingError::Store(
                "the credential store could not be asked".into(),
            ));
        }
        Ok(self.key.lock().unwrap().is_some())
    }

    fn presence_enforced(&self) -> bool {
        false
    }
}

/// For tests that need the real ordinary keychain, which is the user's default
/// keychain: one generic-password item, under a service name of its own, is
/// written, read and deleted. Only where `SYNDEO_KEYSTORE_KEYCHAIN_TEST=1` —
/// CI sets it on a macOS runner thrown away afterwards — and nowhere else, so
/// running the suite on a developer's Mac leaves their keychain alone.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod default_keychain {
    use super::{OsKeyring, WrappingKeyStore};
    use std::sync::Arc;

    pub fn allowed() -> bool {
        let allowed = std::env::var_os("SYNDEO_KEYSTORE_KEYCHAIN_TEST").is_some_and(|v| v == "1");
        if !allowed {
            eprintln!(
                "skipped: writes to the default keychain; set SYNDEO_KEYSTORE_KEYCHAIN_TEST=1 \
                 on a machine that is thrown away afterwards"
            );
        }
        allowed
    }

    /// A keyring under a service no one else uses, so the item is nobody's key.
    pub fn throwaway() -> Arc<OsKeyring> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Arc::new(OsKeyring::new(
            format!(
                "com.sum.syndeo.keystore.test.{}.{nanos}",
                std::process::id()
            ),
            "root-seed",
        ))
    }

    /// Erases the item when the test ends, however it ends.
    pub struct Erased(pub Arc<OsKeyring>);

    impl Drop for Erased {
        fn drop(&mut self) {
            let _ = self.0.erase();
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The keystore as shipped, signed or not: no keychain access group, so the
    /// enclave refuses it and the wrapping key goes to the ordinary keychain —
    /// with no access group attribute on it — and is reported as not gated.
    #[test]
    fn without_the_entitlement_the_key_is_kept_in_the_ordinary_keychain() {
        if !default_keychain::allowed() {
            return;
        }
        assert!(
            !crate::enclave::EnclaveItem::presence_can_be_enforced(),
            "this test binary has no keychain access group, so the enclave must refuse it"
        );
        let store = default_keychain::throwaway();
        let _erased = default_keychain::Erased(store.clone());
        assert!(matches!(store.exists(), Ok(false)));

        let key = [7u8; 32];
        store.store(&key).unwrap();
        assert!(
            matches!(store.enclave.exists(), Ok(false)),
            "nothing may reach the data protection keychain"
        );
        assert!(
            matches!(store.fallback_exists(), Ok(true)),
            "the ordinary keychain holds the key"
        );
        assert!(matches!(store.exists(), Ok(true)));
        assert_eq!(*store.load().unwrap(), key);
        assert!(
            !store.presence_enforced(),
            "a key in the ordinary keychain is not gated, and must not be reported as gated"
        );
        assert_eq!(
            attributes(&store).get("agrp"),
            None,
            "an access group on the item"
        );

        store.erase().unwrap();
        assert!(matches!(store.exists(), Ok(false)));
        assert!(matches!(store.load(), Err(WrappingError::Missing)));
    }

    /// The item's attributes, read the way `fallback_exists` finds it.
    fn attributes(store: &OsKeyring) -> std::collections::HashMap<String, String> {
        use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
        use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};

        let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User).unwrap();
        let found = ItemSearchOptions::new()
            .keychains(&[keychain])
            .class(ItemClass::generic_password())
            .service(&store.service)
            .account(&store.account)
            .load_attributes(true)
            .limit(Limit::Max(1))
            .search()
            .unwrap();
        assert_eq!(found.len(), 1, "exactly the one item");
        found[0]
            .simplify_dict()
            .expect("the item's attributes, as a dictionary")
    }
}
