//! A passphrase or recovery phrase, held so that it is wiped when dropped.
//!
//! [`SecretString`] is what a secret travels in from the moment it is read —
//! from a prompt, from a scripted run's environment, off a socket — until the
//! code that uses it is done. It is written on the wire exactly as a plain
//! string is, so a message that carries one encodes byte for byte as it did
//! when the field was a `String`; it overwrites its bytes when it is dropped;
//! and its `Debug` output names no part of it, so a request that carries one
//! can be logged without logging the secret.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use zeroize::Zeroizing;

/// A secret string, wiped on drop and never shown by `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(value: String) -> Self {
        SecretString(Zeroizing::new(value))
    }

    /// The secret itself, for the code that has to use it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        SecretString::new(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        SecretString::new(value.to_owned())
    }
}

impl From<Zeroizing<String>> for SecretString {
    fn from(value: Zeroizing<String>) -> Self {
        SecretString(value)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Straight into the wrapper: the `String` serde builds is the one that
        // is kept, and so the one that is wiped.
        String::deserialize(deserializer).map(SecretString::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_written_exactly_as_a_plain_string() {
        for value in ["", "pass", "a \"quoted\" \\ passphrase\n", "日本語 ☃"] {
            let secret = SecretString::from(value);
            let wire = serde_json::to_string(&secret).unwrap();
            assert_eq!(wire, serde_json::to_string(value).unwrap());
            let read: SecretString = serde_json::from_str(&wire).unwrap();
            assert_eq!(read.expose(), value);
        }
    }

    #[test]
    fn debug_names_no_part_of_it() {
        let secret = SecretString::from("correct horse battery staple");
        for shown in [format!("{secret:?}"), format!("{:?}", Some(&secret))] {
            assert!(shown.contains("<redacted>"), "{shown}");
            for word in ["correct", "horse", "battery", "staple"] {
                assert!(!shown.contains(word), "{shown}");
            }
        }
    }
}
