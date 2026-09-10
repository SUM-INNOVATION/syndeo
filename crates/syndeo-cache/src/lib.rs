//! Syndeo's HTTP cache.
//!
//! Three pieces, kept apart on purpose:
//!
//! * [`policy`] is RFC 9111 as pure functions — freshness, storability,
//!   revalidation — with no notion of where anything is kept.
//! * [`index`] maps request to content address, and content address to blob
//!   metadata with a refcount. That split is what buys dedupe and peer fetch.
//! * [`blob`] keeps bodies on disk under their BLAKE3 address.
//!
//! [`sri`] sits alongside because Subresource Integrity is the hash a peer-supplied
//! body gets checked against before it is ever believed.

pub mod blob;
pub mod cache;
pub mod error;
pub mod headers;
pub mod index;
pub mod policy;
pub mod range;
pub mod sri;
pub mod stats;
pub mod vary;

pub use blob::{BlobStore, Compression, ContentId};
pub use cache::{Cache, Clock, Lookup, PeerProof, StoreOutcome, StoredResponse};
pub use error::{CacheError, Result};
pub use index::Provenance;
pub use policy::{CacheOptions, Eviction, Freshness, StoredMeta, Storability};
pub use range::{Coverage, ContentRange, RangeSpec, Resolved};
pub use sri::Integrity;
pub use stats::Stats;
