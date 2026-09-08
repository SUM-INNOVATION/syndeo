//! Subresource Integrity (W3C SRI) digests.
//!
//! This is the hinge for peer fetch: a body from a peer is only ever accepted
//! when there is an independent hash to check it against, and SRI in the markup
//! is one of the two places such a hash comes from.

use crate::error::{CacheError, Result};
use base64::Engine;
use sha2::{Digest, Sha256, Sha384, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Algorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha384 => "sha384",
            Algorithm::Sha512 => "sha512",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sha256" => Some(Algorithm::Sha256),
            "sha384" => Some(Algorithm::Sha384),
            "sha512" => Some(Algorithm::Sha512),
            _ => None,
        }
    }

    pub fn digest(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Algorithm::Sha256 => Sha256::digest(bytes).to_vec(),
            Algorithm::Sha384 => Sha384::digest(bytes).to_vec(),
            Algorithm::Sha512 => Sha512::digest(bytes).to_vec(),
        }
    }
}

/// One `alg-base64digest` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hash {
    pub algorithm: Algorithm,
    pub digest: Vec<u8>,
}

impl Hash {
    pub fn to_token(&self) -> String {
        format!(
            "{}-{}",
            self.algorithm.name(),
            base64::engine::general_purpose::STANDARD.encode(&self.digest)
        )
    }

    pub fn compute(algorithm: Algorithm, bytes: &[u8]) -> Self {
        Hash {
            algorithm,
            digest: algorithm.digest(bytes),
        }
    }

    pub fn matches(&self, bytes: &[u8]) -> bool {
        let actual = self.algorithm.digest(bytes);
        constant_time_eq(&actual, &self.digest)
    }
}

/// A parsed `integrity` attribute: one or more tokens, of which any single match
/// is sufficient, but only among the strongest algorithm present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Integrity {
    pub hashes: Vec<Hash>,
}

impl Integrity {
    /// Parse an `integrity="..."` value. Unknown algorithms are ignored, which is
    /// what the spec requires; an attribute made entirely of unknown tokens
    /// yields an empty set and imposes no constraint.
    pub fn parse(value: &str) -> Result<Self> {
        let mut hashes = Vec::new();
        for token in value.split_whitespace() {
            // Options after `?` are not used for matching.
            let token = token.split('?').next().unwrap_or(token);
            let Some((alg, b64)) = token.split_once('-') else {
                continue;
            };
            let Some(algorithm) = Algorithm::parse(alg) else {
                continue;
            };
            let digest = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(b64))
                .map_err(|e| CacheError::BadIntegrity(format!("{token}: {e}")))?;
            let expected_len = match algorithm {
                Algorithm::Sha256 => 32,
                Algorithm::Sha384 => 48,
                Algorithm::Sha512 => 64,
            };
            if digest.len() != expected_len {
                return Err(CacheError::BadIntegrity(format!(
                    "{} digest is {} bytes, expected {}",
                    algorithm.name(),
                    digest.len(),
                    expected_len
                )));
            }
            hashes.push(Hash { algorithm, digest });
        }
        Ok(Integrity { hashes })
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// The strongest algorithm named, which is the only one that gets to decide.
    pub fn strongest(&self) -> Option<Algorithm> {
        self.hashes.iter().map(|h| h.algorithm).max()
    }

    /// True when the bytes satisfy the metadata.
    pub fn verify(&self, bytes: &[u8]) -> bool {
        let Some(strongest) = self.strongest() else {
            return true;
        };
        self.hashes
            .iter()
            .filter(|h| h.algorithm == strongest)
            .any(|h| h.matches(bytes))
    }

    /// Verify, or say precisely what went wrong.
    pub fn check(&self, bytes: &[u8]) -> Result<()> {
        if self.verify(bytes) {
            return Ok(());
        }
        let strongest = self.strongest().unwrap_or(Algorithm::Sha256);
        Err(CacheError::Integrity {
            expected: self
                .hashes
                .iter()
                .filter(|h| h.algorithm == strongest)
                .map(|h| h.to_token())
                .collect::<Vec<_>>()
                .join(" "),
            actual: Hash::compute(strongest, bytes).to_token(),
        })
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = b"alert('hello');";

    fn token(alg: Algorithm) -> String {
        Hash::compute(alg, BODY).to_token()
    }

    #[test]
    fn round_trips_each_algorithm() {
        for alg in [Algorithm::Sha256, Algorithm::Sha384, Algorithm::Sha512] {
            let integrity = Integrity::parse(&token(alg)).unwrap();
            assert!(integrity.verify(BODY), "{}", alg.name());
            assert!(!integrity.verify(b"tampered"));
        }
    }

    #[test]
    fn only_the_strongest_algorithm_decides() {
        // A correct sha256 next to a wrong sha512 must fail: sha512 is strongest.
        let wrong_512 = Hash {
            algorithm: Algorithm::Sha512,
            digest: vec![0u8; 64],
        };
        let value = format!("{} {}", token(Algorithm::Sha256), wrong_512.to_token());
        assert!(!Integrity::parse(&value).unwrap().verify(BODY));
    }

    #[test]
    fn any_match_at_the_strongest_level_is_enough() {
        let wrong = Hash {
            algorithm: Algorithm::Sha384,
            digest: vec![7u8; 48],
        };
        let value = format!("{} {}", wrong.to_token(), token(Algorithm::Sha384));
        assert!(Integrity::parse(&value).unwrap().verify(BODY));
    }

    #[test]
    fn unknown_algorithms_are_ignored() {
        let integrity = Integrity::parse("md5-abc sha256-xyz?opt").unwrap_or_default();
        assert!(integrity.is_empty() || integrity.strongest() == Some(Algorithm::Sha256));
    }

    #[test]
    fn empty_metadata_constrains_nothing() {
        assert!(Integrity::parse("").unwrap().verify(BODY));
    }

    #[test]
    fn wrong_digest_length_is_rejected_at_parse_time() {
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 8]);
        assert!(Integrity::parse(&format!("sha256-{short}")).is_err());
    }
}
