//! The cache facade: index plus blob store plus RFC 9111 policy.

use crate::blob::{BlobStore, ContentId};
use crate::error::{CacheError, Result};
use crate::headers::{now_secs, sanitize};
use crate::index::{entry_key, BlobRecord, EntryRecord, Index, Provenance};
use crate::policy::{self, CacheOptions, Freshness, StoredMeta, Storability};
use crate::sri::Integrity;
use crate::stats::Stats;
use crate::vary;
use http::{HeaderMap, HeaderName};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod counters {
    pub const HITS: &str = "hits";
    pub const STALE_HITS: &str = "stale_hits";
    pub const MISSES: &str = "misses";
    pub const REVALIDATIONS: &str = "revalidations";
    pub const NOT_MODIFIED: &str = "not_modified";
    pub const STORES: &str = "stores";
    pub const REJECTS: &str = "rejects";
    pub const BYTES_FROM_CACHE: &str = "bytes_from_cache";
    pub const BYTES_FROM_ORIGIN: &str = "bytes_from_origin";
    pub const BYTES_DEDUPED: &str = "bytes_deduped";
    pub const PEER_ACCEPTED: &str = "peer_accepted";
    pub const PEER_REJECTED: &str = "peer_rejected";
    pub const REQUESTS: &str = "requests";
}

/// A response reconstructed from the store.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    pub key: String,
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub content: ContentId,
    pub provenance: Provenance,
    pub age: u64,
    pub meta: StoredMeta,
}

#[derive(Debug)]
pub enum Lookup {
    /// Nothing usable; go to the origin.
    Miss(&'static str),
    /// Serve this, it is fresh.
    Fresh(Box<StoredResponse>),
    /// Serve this even though it is stale, per `stale-while-revalidate` or the
    /// client's own `max-stale`.
    Stale {
        response: Box<StoredResponse>,
        refresh_in_background: bool,
        reason: &'static str,
    },
    /// Ask the origin, conditionally.
    Revalidate {
        response: Box<StoredResponse>,
        conditional: Vec<(HeaderName, String)>,
        stale_if_error: Option<u64>,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    /// Written. `deduped` means the bytes were already on disk under this hash.
    Stored { content: ContentId, deduped: bool },
    NotStored(&'static str),
}

/// Source of "now", in Unix seconds. Injectable so freshness is testable without
/// waiting for real time to pass.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub struct Cache {
    index: Index,
    blobs: BlobStore,
    options: CacheOptions,
    root: PathBuf,
    clock: Clock,
}

impl Cache {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::with_options(root, CacheOptions::default())
    }

    pub fn with_options(root: impl AsRef<Path>, options: CacheOptions) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let index_path = root.join("index.redb");
        let blob_path = root.join("blobs");

        // A cache is disposable, and that is the whole answer to a schema
        // change. Migrating records we could just refetch would be a
        // considerable amount of code to preserve something the origin will
        // hand back anyway; discarding is cheaper and cannot corrupt.
        //
        // The blobs go with the index. Without the index nothing refers to them,
        // so keeping them would be keeping garbage with no refcount to free it.
        let index = match Index::open(&index_path) {
            Ok(index) => index,
            Err(CacheError::SchemaMismatch { found, expected }) => {
                tracing::warn!(
                    ?found,
                    expected,
                    path = %index_path.display(),
                    "the cache was written under a different schema; discarding and rebuilding it"
                );
                let _ = std::fs::remove_file(&index_path);
                let _ = std::fs::remove_dir_all(&blob_path);
                Index::open(&index_path)?
            }
            Err(err) => return Err(err),
        };

        let blobs = BlobStore::open(&blob_path)?;
        Ok(Cache {
            index,
            blobs,
            options,
            root,
            clock: Arc::new(now_secs),
        })
    }

    /// Replace the clock. Tests use this to age entries instantly.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    pub fn options(&self) -> &CacheOptions {
        &self.options
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical form of a URL for keying: no fragment, lowercase scheme and
    /// host, default ports removed.
    pub fn normalize_url(raw: &str) -> String {
        match url::Url::parse(raw) {
            Ok(mut u) => {
                u.set_fragment(None);
                let _ = u.set_port(u.port_or_known_default().filter(|p| {
                    !matches!((u.scheme(), *p), ("http", 80) | ("https", 443))
                }));
                u.to_string()
            }
            Err(_) => raw.to_string(),
        }
    }

    // ---- read path ---------------------------------------------------------

    pub fn lookup(&self, method: &str, url: &str, request_headers: &HeaderMap) -> Result<Lookup> {
        self.index.bump(counters::REQUESTS, 1)?;
        let url = Self::normalize_url(url);

        if !policy::is_cacheable_method(method) {
            self.index.bump(counters::MISSES, 1)?;
            return Ok(Lookup::Miss("method is not cacheable"));
        }

        let Some((key, record)) = self.select_variant(method, &url, request_headers)? else {
            self.index.bump(counters::MISSES, 1)?;
            return Ok(Lookup::Miss("no stored variant"));
        };

        let meta = StoredMeta {
            status: record.status,
            headers: to_header_map(&record.headers),
            request_time: record.request_time,
            response_time: record.response_time,
        };
        let now = self.now();

        match policy::evaluate(request_headers, &meta, now, &self.options) {
            Freshness::Fresh { age, .. } => {
                let response = self.materialize(key.clone(), &record, &meta, age)?;
                self.index.touch(&key, now)?;
                self.index.bump(counters::HITS, 1)?;
                self.index
                    .bump(counters::BYTES_FROM_CACHE, response.body.len() as u64)?;
                Ok(Lookup::Fresh(Box::new(response)))
            }
            Freshness::ServeStale {
                age,
                refresh_in_background,
                reason,
                ..
            } => {
                let response = self.materialize(key.clone(), &record, &meta, age)?;
                self.index.touch(&key, now)?;
                self.index.bump(counters::STALE_HITS, 1)?;
                self.index
                    .bump(counters::BYTES_FROM_CACHE, response.body.len() as u64)?;
                Ok(Lookup::Stale {
                    response: Box::new(response),
                    refresh_in_background,
                    reason,
                })
            }
            Freshness::Revalidate {
                age,
                stale_if_error,
                reason,
                ..
            } => {
                if !policy::has_validator(&meta) {
                    self.index.bump(counters::MISSES, 1)?;
                    return Ok(Lookup::Miss("stale with no validator"));
                }
                let response = self.materialize(key.clone(), &record, &meta, age)?;
                self.index.bump(counters::REVALIDATIONS, 1)?;
                Ok(Lookup::Revalidate {
                    conditional: policy::conditional_headers(&meta),
                    response: Box::new(response),
                    stale_if_error,
                    reason,
                })
            }
            Freshness::Unusable(reason) => {
                self.index.bump(counters::MISSES, 1)?;
                Ok(Lookup::Miss(reason))
            }
        }
    }

    /// Find the stored variant whose selecting headers match this request.
    fn select_variant(
        &self,
        method: &str,
        url: &str,
        request_headers: &HeaderMap,
    ) -> Result<Option<(String, EntryRecord)>> {
        for candidate in self.index.variant_keys(method, url)? {
            let key = entry_key(method, url, &candidate);
            let Some(record) = self.index.get_entry(&key)? else {
                continue;
            };
            let computed = vary::vary_key(&record.vary_fields, request_headers);
            if computed == record.vary_key {
                return Ok(Some((key, record)));
            }
        }
        Ok(None)
    }

    fn materialize(
        &self,
        key: String,
        record: &EntryRecord,
        meta: &StoredMeta,
        age: u64,
    ) -> Result<StoredResponse> {
        let body = self.blobs.get(record.content_id())?;
        let mut headers = meta.headers.clone();
        // A qualified `no-cache="field"` means that field may not be reused.
        for field in policy::suppressed_fields(meta) {
            if let Ok(name) = HeaderName::from_bytes(field.as_bytes()) {
                headers.remove(name);
            }
        }
        headers.remove(http::header::AGE);
        if let Ok(value) = age.to_string().parse() {
            headers.insert(http::header::AGE, value);
        }
        Ok(StoredResponse {
            key,
            status: record.status,
            headers,
            body,
            content: record.content_id(),
            provenance: record.provenance,
            age,
            meta: meta.clone(),
        })
    }

    /// Find a stored body by the Subresource Integrity digest a page declared.
    ///
    /// This is what makes peer fetch usable for a subresource nobody has fetched
    /// before: the page names a sha384, a peer is asked for that sha384, and the
    /// bytes that come back either hash to it or are discarded.
    pub fn content_for_integrity(&self, hash: &crate::sri::Hash) -> Result<Option<ContentId>> {
        self.index.content_for_sri(&sri_key(hash.algorithm, &hash.digest))
    }

    /// Record that an SRI digest names a stored body.
    ///
    /// The store computes these itself on write, so this exists for importing an
    /// index built elsewhere — and for tests that need to poison one. It asserts
    /// an association without checking it, so a caller that has not verified the
    /// body against the digest is putting a lie into the index. The requesting
    /// side of peer fetch re-derives the hash regardless, which is why a lie here
    /// costs a wasted round trip and nothing more.
    pub fn associate_integrity(&self, hash: &crate::sri::Hash, content: ContentId) -> Result<()> {
        self.index
            .index_sri(&[(sri_key(hash.algorithm, &hash.digest), content.0)])
    }

    /// Read a body straight out of the blob store by content address. This is
    /// what a peer request is answered from — it never consults the index, so it
    /// cannot leak which URL the body came from.
    pub fn body_by_content(&self, id: ContentId) -> Result<Vec<u8>> {
        self.blobs.get(id)
    }

    pub fn has_content(&self, id: ContentId) -> bool {
        self.blobs.contains(id)
    }

    // ---- write path --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn store(
        &self,
        method: &str,
        url: &str,
        request_headers: &HeaderMap,
        status: u16,
        response_headers: &HeaderMap,
        body: &[u8],
        request_time: u64,
        response_time: u64,
    ) -> Result<StoreOutcome> {
        self.store_with_provenance(
            method,
            url,
            request_headers,
            status,
            response_headers,
            body,
            request_time,
            response_time,
            Provenance::Origin,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn store_with_provenance(
        &self,
        method: &str,
        url: &str,
        request_headers: &HeaderMap,
        status: u16,
        response_headers: &HeaderMap,
        body: &[u8],
        request_time: u64,
        response_time: u64,
        provenance: Provenance,
    ) -> Result<StoreOutcome> {
        let url = Self::normalize_url(url);
        self.index
            .bump(counters::BYTES_FROM_ORIGIN, body.len() as u64)?;

        let meta = StoredMeta {
            status,
            headers: response_headers.clone(),
            request_time,
            response_time,
        };

        if let Storability::Reject(reason) =
            policy::storability(method, request_headers, &meta, body.len() as u64, &self.options)
        {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(reason));
        }

        let fields = vary::vary_fields(response_headers);
        let vkey = vary::vary_key(&fields, request_headers);
        let receipt = self.blobs.put(body)?;
        if !receipt.newly_written {
            self.index.bump(counters::BYTES_DEDUPED, receipt.len)?;
        }

        let now = self.now();
        let record = EntryRecord {
            url,
            method: method.to_ascii_uppercase(),
            status,
            headers: sanitize(response_headers),
            vary_fields: fields,
            vary_key: vkey,
            content: receipt.id.0,
            body_len: receipt.len,
            request_time,
            response_time,
            stored_at: now,
            last_used: now,
            hits: 0,
            provenance,
        };
        let blob = BlobRecord {
            len: receipt.len,
            stored_len: receipt.stored_len,
            compression: receipt.compression,
            refcount: 0,
            created: now,
        };
        self.index.put_entry(&record, blob)?;
        self.index.index_sri(&sri_digests(body, receipt.id))?;
        self.index.bump(counters::STORES, 1)?;

        Ok(StoreOutcome::Stored {
            content: receipt.id,
            deduped: !receipt.newly_written,
        })
    }

    /// Fold a 304 into the stored entry so it becomes fresh again.
    pub fn record_not_modified(
        &self,
        key: &str,
        fresh_headers: &HeaderMap,
        request_time: u64,
        response_time: u64,
    ) -> Result<Option<StoredResponse>> {
        let Some(mut record) = self.index.get_entry(key)? else {
            return Ok(None);
        };
        let mut headers = to_header_map(&record.headers);
        policy::apply_304(&mut headers, fresh_headers);
        record.headers = sanitize(&headers);
        record.request_time = request_time;
        record.response_time = response_time;
        self.index.refresh_entry(&record)?;
        self.index.bump(counters::NOT_MODIFIED, 1)?;

        let meta = StoredMeta {
            status: record.status,
            headers,
            request_time,
            response_time,
        };
        let age = policy::current_age(&meta, self.now());
        let response = self.materialize(key.to_string(), &record, &meta, age)?;
        self.index
            .bump(counters::BYTES_FROM_CACHE, response.body.len() as u64)?;
        self.index.bump(counters::HITS, 1)?;
        Ok(Some(response))
    }

    /// Accept a body offered by a peer. The rule is absolute: without an
    /// independent hash to check it against, the body is refused.
    pub fn accept_peer_body(&self, expected: &PeerProof, body: &[u8]) -> Result<()> {
        let ok = match expected {
            PeerProof::Content(id) => ContentId::of(body) == *id,
            PeerProof::Integrity(integrity) => {
                if integrity.is_empty() {
                    false
                } else {
                    integrity.verify(body)
                }
            }
        };
        if !ok {
            self.index.bump(counters::PEER_REJECTED, 1)?;
            return Err(CacheError::Integrity {
                expected: expected.describe(),
                actual: ContentId::of(body).to_hex(),
            });
        }
        self.blobs.put(body)?;
        self.index.bump(counters::PEER_ACCEPTED, 1)?;
        Ok(())
    }

    /// Unsafe methods invalidate the target URI (RFC 9111 §4.4).
    pub fn invalidate(&self, method: &str, url: &str) -> Result<usize> {
        if !policy::invalidates(method) {
            return Ok(0);
        }
        let url = Self::normalize_url(url);
        let mut removed = 0;
        for m in ["GET", "HEAD"] {
            for orphan in self.index.invalidate(m, &url)? {
                self.blobs.remove(orphan)?;
                self.index.forget_blob(orphan)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn purge(&self, method: &str, url: &str) -> Result<()> {
        let url = Self::normalize_url(url);
        for orphan in self.index.invalidate(method, &url)? {
            self.blobs.remove(orphan)?;
            self.index.forget_blob(orphan)?;
        }
        Ok(())
    }

    /// Delete blobs nothing points at any more.
    pub fn collect_garbage(&self) -> Result<usize> {
        let orphans = self.index.orphaned_blobs()?;
        for id in &orphans {
            self.blobs.remove(*id)?;
            self.index.forget_blob(*id)?;
        }
        Ok(orphans.len())
    }

    pub fn stats(&self) -> Result<Stats> {
        let (blobs, unique_bytes, on_disk_bytes, logical_bytes) = self.index.blob_totals()?;
        let mut stats = Stats {
            entries: self.index.entry_count()?,
            blobs,
            unique_bytes,
            on_disk_bytes,
            logical_bytes,
            ..Default::default()
        };
        for (name, value) in self.index.counters()? {
            stats.apply_counter(&name, value);
        }
        Ok(stats)
    }
}

/// The independent hash a peer body is checked against.
#[derive(Debug, Clone)]
pub enum PeerProof {
    /// A content address from a prior origin fetch.
    Content(ContentId),
    /// Subresource Integrity metadata declared by the page.
    Integrity(Integrity),
}

impl PeerProof {
    fn describe(&self) -> String {
        match self {
            PeerProof::Content(id) => id.to_hex(),
            PeerProof::Integrity(i) => i
                .hashes
                .iter()
                .map(|h| h.to_token())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

pub fn to_header_map(pairs: &[(String, String)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            headers.append(n, v);
        }
    }
    headers
}

/// The key an SRI digest is stored under: one byte of algorithm, then the digest.
pub fn sri_key(algorithm: crate::sri::Algorithm, digest: &[u8]) -> Vec<u8> {
    let tag = match algorithm {
        crate::sri::Algorithm::Sha256 => 1u8,
        crate::sri::Algorithm::Sha384 => 2,
        crate::sri::Algorithm::Sha512 => 3,
    };
    let mut key = Vec::with_capacity(1 + digest.len());
    key.push(tag);
    key.extend_from_slice(digest);
    key
}

/// Every SRI digest a body could be named by. Computing all three on store costs
/// a few hundred microseconds and saves needing the caller to have known the
/// page's `integrity` attribute at the time.
fn sri_digests(body: &[u8], content: ContentId) -> Vec<(Vec<u8>, [u8; 32])> {
    use crate::sri::Algorithm;
    [Algorithm::Sha256, Algorithm::Sha384, Algorithm::Sha512]
        .into_iter()
        .map(|algorithm| (sri_key(algorithm, &algorithm.digest(body)), content.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(cache: &Cache, url: &str, body: &[u8]) {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CACHE_CONTROL, "max-age=600".parse().unwrap());
        let now = cache.now();
        cache
            .store("GET", url, &HeaderMap::new(), 200, &headers, body, now, now)
            .unwrap();
    }

    #[test]
    fn an_index_from_another_schema_is_discarded_and_rebuilt_rather_than_mis_parsed() {
        let dir = tempfile::tempdir().unwrap();
        {
            let cache = Cache::open(dir.path()).unwrap();
            stored(&cache, "https://a.test/one", b"hello");
            assert_eq!(cache.stats().unwrap().entries, 1);
        }

        // What a layout change looks like on a user's real cache.
        crate::index::stamp_schema_version(&dir.path().join("index.redb"), Some(9_999)).unwrap();

        let rebuilt = Cache::open(dir.path()).unwrap();
        assert_eq!(
            rebuilt.stats().unwrap().entries,
            0,
            "a cache is disposable; a schema it does not understand is discarded"
        );
        assert!(
            !dir.path().join("blobs").join("hello").exists(),
            "the blobs go with the index that referred to them"
        );

        // And it is a working cache afterwards, not a wedged one.
        stored(&rebuilt, "https://a.test/two", b"world");
        assert_eq!(rebuilt.stats().unwrap().entries, 1);
    }
}
