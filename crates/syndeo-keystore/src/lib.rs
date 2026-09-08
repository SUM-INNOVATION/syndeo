//! The keystore process.
//!
//! Rule three of the process model: the keystore holds keys and exposes exactly
//! one operation, sign-this-payload-after-the-shell-confirms.
//!
//! Root secret custody, in order of what an attacker has to get past:
//!
//! 1. The seed is never a plaintext file. It is sealed with XChaCha20-Poly1305
//!    under a key the operating system holds, and, inside that, sealed again
//!    under an Argon2id key derived from the user's passphrase. A compromised
//!    Keychain alone is not sufficient.
//! 2. The sealed blob lives at `~/.syndeo/.keystore/seed.sealed`, directory mode
//!    0700 and file mode 0600, hidden, excluded from Time Machine, and checked
//!    for symlink, ownership and mode before every open.
//! 3. Signing requires per-operation user consent to the exact payload, carried
//!    as a MAC the shell mints and the agent cannot forge.
//! 4. A BIP-39 recovery phrase is shown once at setup and never written down by
//!    us. Losing both the credential store and the passphrase is unrecoverable
//!    by design, and the phrase is the answer to that.
//!
//! See [`presence`] for what the operating system currently enforces and what
//! this process enforces on its behalf.

pub mod address;
pub mod custody;
pub mod derive;
pub mod keystore;
pub mod presence;
pub mod seal;
pub mod service;
pub mod wrapping;

pub use address::Address;
pub use derive::{origin_key, ExtendedKey, SUM_COIN_TYPE};
pub use keystore::{Keystore, KeystoreError, Signed, Status};
pub use wrapping::{OsKeyring, WrappingKeyStore};
