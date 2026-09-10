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
//! signed binary with a keychain access group; a build without one falls back to
//! the ordinary keychain and says so, which is what keeps the passphrase
//! mandatory there.

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
    #[cfg(target_os = "macos")]
    fn enrolled_in_enclave(&self) -> bool {
        self.enclave.exists()
    }

    #[cfg(not(target_os = "macos"))]
    fn enrolled_in_enclave(&self) -> bool {
        false
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

    fn presence_enforced(&self) -> bool {
        false
    }
}
