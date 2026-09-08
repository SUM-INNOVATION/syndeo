//! Where the wrapping key lives.
//!
//! On macOS that is the Keychain, on Linux the Secret Service, on Windows the
//! Credential Manager — never a file we wrote ourselves. The trait exists so the
//! tests do not touch the user's real credential store, and so the presence-
//! enforced Security.framework path can slot in without touching the keystore.

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
    /// True when the operating system gates access on user presence.
    fn presence_enforced(&self) -> bool {
        crate::presence::available()
    }
}

/// The operating system's own credential store.
pub struct OsKeyring {
    service: String,
    account: String,
}

impl OsKeyring {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        OsKeyring {
            service: service.into(),
            account: account.into(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| WrappingError::Store(e.to_string()))
    }
}

impl WrappingKeyStore for OsKeyring {
    fn store(&self, key: &[u8; 32]) -> Result<()> {
        let encoded = Zeroizing::new(hex::encode(key));
        self.entry()?
            .set_password(&encoded)
            .map_err(|e| WrappingError::Store(e.to_string()))
    }

    fn load(&self) -> Result<Zeroizing<[u8; 32]>> {
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
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(WrappingError::Store(e.to_string())),
        }
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
