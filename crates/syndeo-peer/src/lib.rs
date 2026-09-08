//! Peer fetch.
//!
//! The rule, and it does not bend: a peer-supplied body is only accepted when
//! there is an independent hash to check it against — Subresource Integrity from
//! the markup, or a content address from a prior origin fetch. Never a first,
//! unverified fetch.
//!
//! The protocol is built so that rule is structural rather than a policy someone
//! could forget to apply. You cannot ask a peer for a URL. You can only ask for
//! a hash, and [`BlobRequest::is_satisfied_by`] recomputes it before the bytes
//! are believed. A peer that lies produces bytes that fail the check; a peer
//! that is curious learns a hash, not a page.

pub mod codec;
pub mod protocol;
pub mod swarm;

pub use protocol::{BlobRequest, BlobResponse, MAX_BODY, PROTOCOL};
pub use swarm::{PeerConfig, PeerHandle, PeerNode};

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("no peer had it")]
    NotFound,
    #[error("a peer answered with bytes that do not match {0}")]
    Rejected(String),
    #[error("the request timed out")]
    Timeout,
    #[error("the swarm is not running")]
    Stopped,
    #[error("cache: {0}")]
    Cache(#[from] syndeo_cache::CacheError),
}

pub type Result<T> = std::result::Result<T, PeerError>;
