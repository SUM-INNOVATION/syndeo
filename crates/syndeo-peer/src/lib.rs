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
//!
//! # What the privacy properties actually are
//!
//! Worth stating plainly, because "a request names a hash, not a URL" is true
//! and is easily heard as more than it is.
//!
//! **What holds.** A peer never learns a URL, a page, a referrer, or a cookie
//! from us — the protocol has no field to carry one. It cannot substitute bytes,
//! because the request names what they must hash to. It cannot enumerate what we
//! hold, because there is no "what do you have" request.
//!
//! **What does not.** A peer learns *which hashes we want, and when*. For a
//! subresource with declared integrity that hash is public — it is written in
//! the markup of whatever page declares it — so a peer that has crawled the web
//! can map hashes back to the pages that reference them. Asking for the hash of
//! a script that only one site serves tells a peer we are on that site, near
//! enough. The timing sequence of several such requests is a stronger signal
//! still.
//!
//! Announcing to the DHT is a further disclosure, in the other direction: a
//! provider record says we hold those bytes and are willing to serve them, which
//! is a claim about our history rather than our present. Only bodies whose page
//! declared an integrity hash are announced, which bounds it to resources that
//! are shared between sites and were public to begin with; nothing is announced
//! by a node that is not serving.
//!
//! **What would fix it,** and is not built: padding and cover traffic to blunt
//! the timing signal, and asking through a relay so the peer that answers is not
//! the peer that learns who asked. Both are real work and neither is here, so
//! this should be read as "peer fetch does not leak your browsing to the origin"
//! and not as "peer fetch is anonymous".

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
