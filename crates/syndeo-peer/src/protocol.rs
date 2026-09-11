//! The peer protocol.
//!
//! There is exactly one thing you can ask a peer for: a body, named by a hash.
//! Not a URL, not a page, not "what do you have" — a hash. That constraint is
//! the whole security argument. A peer cannot answer with the wrong bytes,
//! because the request names what the right bytes hash to, and it cannot learn
//! what you are browsing from the request either, because a hash is not a URL.

use serde::{Deserialize, Serialize};
use syndeo_cache::sri::Algorithm;
use syndeo_cache::ContentId;

pub const PROTOCOL: &str = "/syndeo/blob/1.0.0";

/// Largest body we will accept from a peer. A peer is not trusted enough to be
/// allowed to decide how much memory we allocate.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// How a body is named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobRequest {
    /// Our own content address, from a prior origin fetch. Verified by BLAKE3.
    Content([u8; 32]),
    /// A Subresource Integrity digest declared by a page. Verified by the named
    /// SHA-2 function. This is the case that lets a peer answer a first,
    /// never-before-fetched subresource.
    Integrity { algorithm: u8, digest: Vec<u8> },
}

impl BlobRequest {
    pub fn content(id: ContentId) -> Self {
        BlobRequest::Content(id.0)
    }

    pub fn integrity(hash: &syndeo_cache::sri::Hash) -> Self {
        BlobRequest::Integrity {
            algorithm: algorithm_tag(hash.algorithm),
            digest: hash.digest.clone(),
        }
    }

    /// Do these bytes answer this request? Every branch recomputes the hash the
    /// request named; there is no path here that takes a peer's word for it.
    pub fn is_satisfied_by(&self, body: &[u8]) -> bool {
        match self {
            BlobRequest::Content(expected) => ContentId::of(body).0 == *expected,
            BlobRequest::Integrity { algorithm, digest } => {
                let Some(algorithm) = algorithm_from_tag(*algorithm) else {
                    return false;
                };
                let actual = algorithm.digest(body);
                actual.len() == digest.len() && constant_time_eq(&actual, digest)
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            BlobRequest::Content(id) => format!("blake3:{}", hex::encode(&id[..8])),
            BlobRequest::Integrity { algorithm, digest } => format!(
                "{}:{}",
                algorithm_from_tag(*algorithm)
                    .map(|a| a.name())
                    .unwrap_or("unknown"),
                hex::encode(&digest[..8.min(digest.len())])
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobResponse {
    Have(Vec<u8>),
    /// The peer does not have it. Says nothing about what it does have.
    Missing,
    /// The peer has spent its credit here and is being asked to wait. Distinct
    /// from `Missing` because it is not an answer about the body at all, and a
    /// requester should try somewhere else rather than conclude nobody has it.
    Throttled,
}

/// The DHT key a body is announced and looked up under.
///
/// Derived from the hash and nothing else, so a provider record says "somebody
/// has these bytes" and never says which URL they came from. It is still a
/// disclosure — see the note in the crate documentation.
pub fn record_key(request: &BlobRequest) -> Vec<u8> {
    let mut key = Vec::with_capacity(33);
    match request {
        BlobRequest::Content(id) => {
            key.push(0);
            key.extend_from_slice(id);
        }
        BlobRequest::Integrity { algorithm, digest } => {
            key.push(*algorithm);
            key.extend_from_slice(digest);
        }
    }
    key
}

pub fn algorithm_tag(algorithm: Algorithm) -> u8 {
    match algorithm {
        Algorithm::Sha256 => 1,
        Algorithm::Sha384 => 2,
        Algorithm::Sha512 => 3,
    }
}

pub fn algorithm_from_tag(tag: u8) -> Option<Algorithm> {
    match tag {
        1 => Some(Algorithm::Sha256),
        2 => Some(Algorithm::Sha384),
        3 => Some(Algorithm::Sha512),
        _ => None,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use syndeo_cache::sri::Hash;

    const BODY: &[u8] = b"the real body";

    #[test]
    fn a_content_request_accepts_only_the_matching_bytes() {
        let request = BlobRequest::content(ContentId::of(BODY));
        assert!(request.is_satisfied_by(BODY));
        assert!(!request.is_satisfied_by(b"substituted"));
        assert!(!request.is_satisfied_by(b""));
    }

    #[test]
    fn an_integrity_request_accepts_only_the_matching_bytes() {
        for algorithm in [Algorithm::Sha256, Algorithm::Sha384, Algorithm::Sha512] {
            let request = BlobRequest::integrity(&Hash::compute(algorithm, BODY));
            assert!(request.is_satisfied_by(BODY), "{}", algorithm.name());
            assert!(!request.is_satisfied_by(b"substituted"));
        }
    }

    #[test]
    fn an_unknown_algorithm_satisfies_nothing() {
        let request = BlobRequest::Integrity {
            algorithm: 99,
            digest: vec![0; 32],
        };
        assert!(!request.is_satisfied_by(BODY));
        assert!(!request.is_satisfied_by(b""));
    }

    #[test]
    fn a_truncated_digest_does_not_match_by_prefix() {
        let full = Hash::compute(Algorithm::Sha256, BODY);
        let request = BlobRequest::Integrity {
            algorithm: algorithm_tag(Algorithm::Sha256),
            digest: full.digest[..16].to_vec(),
        };
        assert!(
            !request.is_satisfied_by(BODY),
            "a short digest is not a weaker digest"
        );
    }
}
