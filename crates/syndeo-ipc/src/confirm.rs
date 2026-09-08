//! Shell-issued signing confirmations.
//!
//! The keystore holds keys and exposes exactly one operation: sign this payload
//! after the shell confirms. This module is the "after the shell confirms" part,
//! made checkable rather than trusted.
//!
//! The shell and the keystore share a secret established when the shell spawns
//! the keystore. The agent never sees it, so the agent cannot mint a
//! confirmation even if it somehow reached the keystore socket.

use crate::protocol::SignaturePurpose;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

type HmacSha256 = Hmac<Sha256>;

/// How long a confirmation stays valid. A human said yes to a specific payload
/// a moment ago; that consent does not extend to next week.
pub const CONFIRMATION_TTL: u64 = 120;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfirmationError {
    #[error("the confirmation signature does not verify")]
    BadMac,
    #[error("the confirmation expired")]
    Expired,
    #[error("the confirmation was issued in the future")]
    NotYetValid,
    #[error("this confirmation has already been used")]
    Replayed,
    #[error("the payload does not match the one the user was shown")]
    PayloadMismatch,
}

/// The secret shared between shell and keystore. Never written to disk, never
/// sent to the agent.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionSecret([u8; 32]);

impl SessionSecret {
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        SessionSecret(bytes)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s.trim()).ok()?;
        Some(SessionSecret(bytes.try_into().ok()?))
    }

    /// Only ever used to hand the secret to the keystore at spawn time.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Debug for SessionSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionSecret(redacted)")
    }
}

/// A human's answer to one specific question, in a form the keystore can check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Confirmation {
    pub origin: String,
    pub purpose: SignaturePurpose,
    /// BLAKE3 of the exact bytes the user was shown and agreed to sign.
    pub payload_hash: String,
    /// BLAKE3 of the description rendered in the dialog, so the record of what
    /// the user actually read is bound into the token.
    pub description_hash: String,
    pub nonce: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub mac: String,
}

impl Confirmation {
    /// The bytes the MAC covers. Every field that changes the meaning of the
    /// signature is in here.
    fn signing_input(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for part in [
            "syndeo-confirmation-v1",
            &self.origin,
            self.purpose.as_str(),
            &self.payload_hash,
            &self.description_hash,
            &self.nonce,
            &self.issued_at.to_string(),
            &self.expires_at.to_string(),
        ] {
            out.extend_from_slice(part.as_bytes());
            out.push(0x1f);
        }
        out
    }
}

/// Mints confirmations in the shell; verifies them in the keystore.
pub struct Confirmer {
    secret: SessionSecret,
    consumed: Mutex<HashSet<String>>,
}

impl Confirmer {
    pub fn new(secret: SessionSecret) -> Self {
        Confirmer {
            secret,
            consumed: Mutex::new(HashSet::new()),
        }
    }

    /// Called by the shell, once the user has said yes to this exact payload.
    pub fn issue(
        &self,
        origin: &str,
        purpose: SignaturePurpose,
        description: &str,
        payload: &[u8],
    ) -> Confirmation {
        let now = now();
        let mut confirmation = Confirmation {
            origin: origin.to_string(),
            purpose,
            payload_hash: blake3::hash(payload).to_hex().to_string(),
            description_hash: blake3::hash(description.as_bytes()).to_hex().to_string(),
            nonce: random_nonce(),
            issued_at: now,
            expires_at: now + CONFIRMATION_TTL,
            mac: String::new(),
        };
        confirmation.mac = hex::encode(self.mac(&confirmation.signing_input()));
        confirmation
    }

    /// Called by the keystore, before it will sign anything.
    pub fn verify(
        &self,
        confirmation: &Confirmation,
        payload: &[u8],
    ) -> Result<(), ConfirmationError> {
        let expected = self.mac(&confirmation.signing_input());
        let presented = hex::decode(&confirmation.mac).unwrap_or_default();
        if presented.len() != expected.len()
            || !bool::from(presented.ct_eq(&expected))
        {
            return Err(ConfirmationError::BadMac);
        }

        let now = now();
        if now > confirmation.expires_at {
            return Err(ConfirmationError::Expired);
        }
        // A little slack for clock jitter between processes, and no more.
        if confirmation.issued_at > now + 5 {
            return Err(ConfirmationError::NotYetValid);
        }

        let actual = blake3::hash(payload).to_hex().to_string();
        if actual != confirmation.payload_hash {
            return Err(ConfirmationError::PayloadMismatch);
        }

        let mut consumed = self.consumed.lock().unwrap();
        if !consumed.insert(confirmation.nonce.clone()) {
            return Err(ConfirmationError::Replayed);
        }
        // Nonces only need to be remembered for as long as one could still be
        // valid; anything older cannot pass the expiry check anyway.
        if consumed.len() > 4096 {
            consumed.clear();
            consumed.insert(confirmation.nonce.clone());
        }
        Ok(())
    }

    fn mac(&self, input: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.secret.0).expect("hmac accepts any key");
        mac.update(input);
        mac.finalize().into_bytes().to_vec()
    }
}

fn random_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirmer() -> Confirmer {
        Confirmer::new(SessionSecret::generate())
    }

    #[test]
    fn a_confirmation_the_shell_issued_verifies_once() {
        let c = confirmer();
        let payload = b"transfer 10 SUM to alice";
        let token = c.issue("https://wallet.test", SignaturePurpose::ChainTransaction, "Send 10 SUM", payload);
        assert_eq!(c.verify(&token, payload), Ok(()));
        assert_eq!(c.verify(&token, payload), Err(ConfirmationError::Replayed));
    }

    #[test]
    fn a_confirmation_does_not_transfer_to_a_different_payload() {
        let c = confirmer();
        let token = c.issue("https://wallet.test", SignaturePurpose::ChainTransaction, "Send 10 SUM", b"send 10");
        assert_eq!(
            c.verify(&token, b"send 10000"),
            Err(ConfirmationError::PayloadMismatch)
        );
    }

    #[test]
    fn tampering_with_any_bound_field_breaks_the_mac() {
        let c = confirmer();
        let payload = b"payload";
        let original = c.issue("https://a.test", SignaturePurpose::OriginLogin, "Log in to a.test", payload);

        for mutate in [
            (|t: &mut Confirmation| t.origin = "https://evil.test".into()) as fn(&mut Confirmation),
            |t| t.purpose = SignaturePurpose::ChainTransaction,
            |t| t.expires_at += 86_400,
            |t| t.description_hash = blake3::hash(b"something else").to_hex().to_string(),
        ] {
            let mut tampered = original.clone();
            mutate(&mut tampered);
            assert_eq!(c.verify(&tampered, payload), Err(ConfirmationError::BadMac));
        }
    }

    #[test]
    fn a_confirmation_from_another_session_is_worthless() {
        let shell = confirmer();
        let keystore = confirmer(); // a different secret
        let payload = b"payload";
        let token = shell.issue("https://a.test", SignaturePurpose::OriginLogin, "Log in", payload);
        assert_eq!(keystore.verify(&token, payload), Err(ConfirmationError::BadMac));
    }

    #[test]
    fn an_expired_confirmation_is_refused() {
        let c = confirmer();
        let payload = b"payload";
        let mut token = c.issue("https://a.test", SignaturePurpose::OriginLogin, "Log in", payload);
        token.expires_at = now() - 1;
        token.mac = hex::encode(c.mac(&token.signing_input()));
        assert_eq!(c.verify(&token, payload), Err(ConfirmationError::Expired));
    }

    #[test]
    fn the_session_secret_does_not_leak_through_debug() {
        let secret = SessionSecret::generate();
        let hex = secret.to_hex();
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains(&hex));
    }
}
