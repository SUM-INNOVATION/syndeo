//! The cache facade: index plus blob store plus RFC 9111 policy.

use crate::blob::{BlobStore, ContentId};
use crate::error::{CacheError, Result};
use crate::headers::{now_secs, StoredHeaders};
use crate::index::{entry_key, BlobRecord, EntryRecord, Index, Provenance, Segment, StoredBody};
use crate::policy::{self, CacheOptions, Freshness, Storability, StoredMeta};
use crate::range::{self, Coverage, Resolved};
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
    pub const RANGE_HITS: &str = "range_hits";
    pub const PARTIAL_STORES: &str = "partial_stores";
    pub const EVICTIONS: &str = "evictions";
    /// Entries dropped because their stored body was missing or corrupt.
    pub const CORRUPT_ENTRIES: &str = "corrupt_entries";
    /// Temporary files from interrupted writes, removed when the cache opened.
    pub const SWEPT_TEMPORARIES: &str = "swept_temporaries";
}

/// A response reconstructed from the store.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    pub key: String,
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// The address of the whole body. A partial entry has none, and a range
    /// served out of a complete body still names the complete body.
    pub content: Option<ContentId>,
    pub provenance: Provenance,
    pub age: u64,
    pub meta: StoredMeta,
    /// Set when this is a 206 we assembled ourselves.
    pub range: Option<Resolved>,
    /// True when the request was a HEAD, so the headers are real and the body
    /// is deliberately empty.
    pub body_omitted: bool,
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
    Stored {
        content: ContentId,
        deduped: bool,
    },
    /// A range was written into an entry that is still missing bytes. There is
    /// no content address yet, because there is no whole body to address.
    StoredPartial {
        held: u64,
        complete_len: Option<u64>,
    },
    NotStored(&'static str),
}

/// Which bytes of a stored entry a request can be answered with.
enum Wanted {
    /// The whole stored representation.
    Whole,
    /// One resolved byte range, entirely covered by what is stored.
    Range(Resolved),
    /// This entry cannot answer this request; the reason is the miss reason.
    Unusable(&'static str),
}

/// The resolved form of [`Wanted`], plus whether the caller asked with HEAD.
struct Want {
    range: Option<Resolved>,
    omit_body: bool,
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
    /// Orders deleting a blob file against storing one.
    ///
    /// A store checks whether the bytes are already on disk and, if they
    /// are, takes a reference to the existing file. A deletion checks that
    /// nothing refers to a blob and removes its file. Interleaved, the store
    /// can take its reference to a file the deletion is about to remove, and
    /// the entry it writes names bytes that are gone. So every store holds this
    /// shared from the moment it looks at the disk until its references are
    /// committed, and every deletion holds it exclusively. The index is opened
    /// by one process at a time, so an in-process lock is enough.
    blob_gate: std::sync::RwLock<()>,
    /// Test-only pause points, for driving an interleaving deterministically.
    #[cfg(test)]
    pause: std::sync::Mutex<Option<PauseHook>>,
}

#[cfg(test)]
type PauseHook = Arc<dyn Fn(&'static str) + Send + Sync>;

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
        // The index is ours alone now (redb holds it exclusively), so nothing
        // else can be mid-write: whatever temporary files are here were left by
        // a write that never finished.
        let swept = blobs.sweep_abandoned()?;
        if swept > 0 {
            tracing::info!(swept, "removed files left by interrupted cache writes");
            index.bump(counters::SWEPT_TEMPORARIES, swept as u64)?;
        }
        Ok(Cache {
            index,
            blobs,
            options,
            root,
            clock: Arc::new(now_secs),
            blob_gate: std::sync::RwLock::new(()),
            #[cfg(test)]
            pause: std::sync::Mutex::new(None),
        })
    }

    /// Held by a store from its look at the disk until its references commit.
    fn storing(&self) -> std::sync::RwLockReadGuard<'_, ()> {
        self.blob_gate.read().unwrap_or_else(|e| e.into_inner())
    }

    fn pause_point(&self, _name: &'static str) {
        #[cfg(test)]
        {
            let hook = self.pause.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(_name);
            }
        }
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
                let _ = u.set_port(
                    u.port_or_known_default()
                        .filter(|p| !matches!((u.scheme(), *p), ("http", 80) | ("https", 443))),
                );
                u.to_string()
            }
            Err(_) => raw.to_string(),
        }
    }

    // ---- read path ---------------------------------------------------------

    pub fn lookup(
        &self,
        partition: Option<&str>,
        method: &str,
        url: &str,
        request_headers: &HeaderMap,
    ) -> Result<Lookup> {
        // Counted in memory and written once, at the end, in a single
        // transaction. Writing each of these as it happened cost an fsync
        // apiece against redb's one writer, and made a cache hit slower than
        // the origin fetch it replaced. See `Index::record_access`.
        let mut tally: Vec<(&str, u64)> = vec![(counters::REQUESTS, 1)];
        let url = Self::normalize_url(url);

        if !policy::is_cacheable_method(method) {
            tally.push((counters::MISSES, 1));
            self.index.record_access(None, 0, &tally)?;
            return Ok(Lookup::Miss("method is not cacheable"));
        }

        // RFC 9111 §4: a stored GET can answer a HEAD, because the GET's headers
        // are exactly what a HEAD response is. Its own stored variant is tried
        // first; the fallback is what saves the origin round trip.
        let head = method.eq_ignore_ascii_case("HEAD");
        let selected = match self.select_variant(partition, method, &url, request_headers)? {
            Some(found) => Some(found),
            None if head => self.select_variant(partition, "GET", &url, request_headers)?,
            None => None,
        };
        let Some((key, record)) = selected else {
            tally.push((counters::MISSES, 1));
            self.index.record_access(None, 0, &tally)?;
            return Ok(Lookup::Miss("no stored variant"));
        };

        let meta = StoredMeta {
            status: record.status,
            headers: record.headers.to_header_map(),
            request_time: record.request_time,
            response_time: record.response_time,
        };
        let now = self.now();

        // Which bytes this request wants out of what we hold. Ranges are decided
        // here rather than in `policy::evaluate` because resolving one needs the
        // stored length, which the policy layer deliberately does not know.
        let want = match self.wanted_bytes(request_headers, &record, &meta) {
            Wanted::Unusable(reason) => {
                tally.push((counters::MISSES, 1));
                self.index.record_access(None, 0, &tally)?;
                return Ok(Lookup::Miss(reason));
            }
            Wanted::Range(resolved) => Want {
                range: Some(resolved),
                omit_body: head,
            },
            Wanted::Whole => Want {
                range: None,
                omit_body: head,
            },
        };

        match policy::evaluate(request_headers, &meta, now, &self.options) {
            Freshness::Fresh { age, .. } => {
                let response = match self.materialize(key.clone(), &record, &meta, age, &want) {
                    Ok(response) => response,
                    Err(err) if err.is_lost_body() => {
                        return self.discard_broken(&key, &record, tally, &err)
                    }
                    Err(err) => return Err(err),
                };
                tally.push((counters::HITS, 1));
                if want.range.is_some() {
                    tally.push((counters::RANGE_HITS, 1));
                }
                tally.push((counters::BYTES_FROM_CACHE, response.body.len() as u64));
                self.index.record_access(Some(&key), now, &tally)?;
                Ok(Lookup::Fresh(Box::new(response)))
            }
            Freshness::ServeStale {
                age,
                refresh_in_background,
                reason,
                ..
            } => {
                let response = match self.materialize(key.clone(), &record, &meta, age, &want) {
                    Ok(response) => response,
                    Err(err) if err.is_lost_body() => {
                        return self.discard_broken(&key, &record, tally, &err)
                    }
                    Err(err) => return Err(err),
                };
                tally.push((counters::STALE_HITS, 1));
                if want.range.is_some() {
                    tally.push((counters::RANGE_HITS, 1));
                }
                tally.push((counters::BYTES_FROM_CACHE, response.body.len() as u64));
                self.index.record_access(Some(&key), now, &tally)?;
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
                    tally.push((counters::MISSES, 1));
                    self.index.record_access(None, 0, &tally)?;
                    return Ok(Lookup::Miss("stale with no validator"));
                }
                // A stale partial has no whole body to serve if revalidation
                // succeeds and nothing to fall back on if it does not. Refetch.
                if !record.body.is_complete() {
                    tally.push((counters::MISSES, 1));
                    self.index.record_access(None, 0, &tally)?;
                    return Ok(Lookup::Miss(
                        "a stale partial entry is refetched, not revalidated",
                    ));
                }
                let response = match self.materialize(key.clone(), &record, &meta, age, &want) {
                    Ok(response) => response,
                    Err(err) if err.is_lost_body() => {
                        return self.discard_broken(&key, &record, tally, &err)
                    }
                    Err(err) => return Err(err),
                };
                tally.push((counters::REVALIDATIONS, 1));
                self.index.record_access(None, 0, &tally)?;
                Ok(Lookup::Revalidate {
                    conditional: policy::conditional_headers(&meta),
                    response: Box::new(response),
                    stale_if_error,
                    reason,
                })
            }
            Freshness::Unusable(reason) => {
                tally.push((counters::MISSES, 1));
                self.index.record_access(None, 0, &tally)?;
                Ok(Lookup::Miss(reason))
            }
        }
    }

    /// An entry whose stored body is missing or corrupt stops being an entry.
    ///
    /// The alternative is a URL that fails on every request until eviction
    /// happens to reach it. The entry is dropped (only if it is still exactly
    /// the entry that failed; see [`Index::drop_broken_entry`]), the blobs it alone
    /// referred to are deleted, and the request becomes a miss, so the caller
    /// fetches it again and the store is repaired by the next write.
    fn discard_broken(
        &self,
        key: &str,
        record: &EntryRecord,
        mut tally: Vec<(&str, u64)>,
        err: &CacheError,
    ) -> Result<Lookup> {
        self.drop_broken(key, record, err)?;
        tally.push((counters::MISSES, 1));
        self.index.record_access(None, 0, &tally)?;
        Ok(Lookup::Miss("stored body missing or corrupt"))
    }

    fn drop_broken(&self, key: &str, record: &EntryRecord, err: &CacheError) -> Result<()> {
        if let Some(orphaned) = self.index.drop_broken_entry(key, record)? {
            tracing::warn!(url = %record.url, %err, "dropped a cache entry whose body was lost");
            self.index.bump(counters::CORRUPT_ENTRIES, 1)?;
            self.delete_orphans(&orphaned)?;
        }
        Ok(())
    }

    /// Delete the files of blobs nothing refers to, and their records.
    ///
    /// The only place a blob file is deleted. Each one is re-checked under the
    /// exclusive gate, so a blob a store has taken a new reference to since it
    /// was released keeps its file. Returns how many were deleted.
    fn delete_orphans(&self, candidates: &[ContentId]) -> Result<usize> {
        if candidates.is_empty() {
            return Ok(0);
        }
        let _exclusive = self.blob_gate.write().unwrap_or_else(|e| e.into_inner());
        self.pause_point("delete:locked");
        let mut deleted = 0;
        for id in candidates {
            if self.index.forget_blob_if_unreferenced(*id)? {
                self.pause_point("delete:forgotten");
                self.blobs.remove(*id)?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// What this request wants out of the stored entry.
    fn wanted_bytes(
        &self,
        request_headers: &HeaderMap,
        record: &EntryRecord,
        meta: &StoredMeta,
    ) -> Wanted {
        // Whether the whole representation is servable from what we hold.
        let whole = || {
            if record.body.is_complete() {
                Wanted::Whole
            } else {
                Wanted::Unusable("only part of the body is stored")
            }
        };

        let Some(raw) = request_headers
            .get(http::header::RANGE)
            .and_then(|v| v.to_str().ok())
        else {
            return whole();
        };

        // `If-Range` asks for the range only if the representation is unchanged.
        // If it does not match what we hold, the client wants the whole thing
        // from the origin rather than our copy of an older one.
        if let Some(condition) = request_headers
            .get(http::header::IF_RANGE)
            .and_then(|v| v.to_str().ok())
        {
            if !range::if_range_matches(condition, &meta.headers) {
                return Wanted::Unusable("if-range does not match the stored validator");
            }
        }

        // RFC 9110 §14.2: an unparsable Range is ignored, not an error.
        let Some(specs) = range::parse_range(raw) else {
            return whole();
        };
        if specs.len() > 1 {
            // A multipart answer has to be assembled and framed; the origin does
            // that correctly and we pass it through rather than approximate it.
            return Wanted::Unusable("multipart ranges are not served from the store");
        }

        let Some(complete_len) = record.body.complete_len() else {
            return Wanted::Unusable("the length of the whole body is not known");
        };
        // Unsatisfiable: ignore the range and serve the representation.
        let Some(resolved) = range::resolve(specs[0], complete_len) else {
            return whole();
        };

        let (start, end) = resolved.half_open();
        if !record.body.coverage().covers(start, end) {
            return Wanted::Unusable("the requested range is not stored");
        }
        Wanted::Range(resolved)
    }

    /// Find the stored variant whose selecting headers match this request.
    fn select_variant(
        &self,
        partition: Option<&str>,
        method: &str,
        url: &str,
        request_headers: &HeaderMap,
    ) -> Result<Option<(String, EntryRecord)>> {
        for candidate in self.index.variant_keys(partition, method, url)? {
            let key = entry_key(partition, method, url, &candidate);
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

    /// Read `[start, end)` out of an entry, whether it is stored whole or in
    /// runs. The caller has already established that the entry covers it.
    fn read_span(&self, record: &EntryRecord, start: u64, end: u64) -> Result<Vec<u8>> {
        match &record.body {
            StoredBody::Complete { content, .. } => {
                let body = self.blobs.get(ContentId(*content))?;
                let to = (end as usize).min(body.len());
                let from = (start as usize).min(to);
                Ok(body[from..to].to_vec())
            }
            StoredBody::Partial { segments, .. } => {
                let mut out = Vec::with_capacity((end - start) as usize);
                let mut cursor = start;
                for segment in segments {
                    if segment.end <= cursor {
                        continue;
                    }
                    if segment.start > cursor || segment.start >= end {
                        break;
                    }
                    let bytes = self.blobs.get(segment.content_id())?;
                    let stop = end.min(segment.end);
                    let from = (cursor - segment.start) as usize;
                    let to = ((stop - segment.start) as usize).min(bytes.len());
                    out.extend_from_slice(&bytes[from.min(to)..to]);
                    cursor = stop;
                    if cursor >= end {
                        break;
                    }
                }
                if cursor < end {
                    return Err(CacheError::MissingBlob(format!(
                        "bytes {start}..{end} of {}",
                        record.url
                    )));
                }
                Ok(out)
            }
        }
    }

    fn materialize(
        &self,
        key: String,
        record: &EntryRecord,
        meta: &StoredMeta,
        age: u64,
        want: &Want,
    ) -> Result<StoredResponse> {
        let body = match (want.omit_body, want.range) {
            // A HEAD gets the headers and nothing else, by definition.
            (true, _) => Vec::new(),
            (false, Some(resolved)) => {
                let (start, end) = resolved.half_open();
                self.read_span(record, start, end)?
            }
            (false, None) => {
                let len = record.body.complete_len().unwrap_or(0);
                self.read_span(record, 0, len)?
            }
        };

        // The serving boundary. Storage already refuses these fields; an index
        // written before it did can still hold them, and nothing that leaves
        // the store may carry one. The metadata goes out with the response too,
        // so it is cleaned the same way.
        let mut meta = meta.clone();
        crate::headers::strip_never_stored(&mut meta.headers);
        let mut headers = meta.headers.clone();
        // A qualified `no-cache="field"` means that field may not be reused.
        for field in policy::suppressed_fields(&meta) {
            if let Ok(name) = HeaderName::from_bytes(field.as_bytes()) {
                headers.remove(name);
            }
        }
        headers.remove(http::header::AGE);
        if let Ok(value) = age.to_string().parse() {
            headers.insert(http::header::AGE, value);
        }

        let status = match want.range {
            Some(resolved) => {
                if let Ok(value) = resolved.content_range().parse() {
                    headers.insert(http::header::CONTENT_RANGE, value);
                }
                if let Ok(value) = resolved.len().to_string().parse() {
                    headers.insert(http::header::CONTENT_LENGTH, value);
                }
                206
            }
            None => record.status,
        };

        Ok(StoredResponse {
            key,
            status,
            headers,
            body,
            content: record.content_id(),
            provenance: record.provenance,
            age,
            meta,
            range: want.range,
            body_omitted: want.omit_body,
        })
    }

    /// Find a stored body by the Subresource Integrity digest a page declared.
    ///
    /// This is what makes peer fetch usable for a subresource nobody has fetched
    /// before: the page names a sha384, a peer is asked for that sha384, and the
    /// bytes that come back either hash to it or are discarded.
    pub fn content_for_integrity(&self, hash: &crate::sri::Hash) -> Result<Option<ContentId>> {
        self.index
            .content_for_sri(&sri_key(hash.algorithm, &hash.digest))
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

    /// Drop a stored entry because its bytes are not the representation a
    /// caller asked for — they do not satisfy a declared integrity value —
    /// but only while it still holds `content`. Returns whether it went.
    ///
    /// The bytes themselves are sound, so this is not counted as a lost body
    /// and whether a peer may have them is left as it was.
    pub fn discard_representation(&self, key: &str, content: ContentId) -> Result<bool> {
        let Some(record) = self.index.get_entry(key)? else {
            return Ok(false);
        };
        if record.content_id() != Some(content) {
            return Ok(false);
        }
        match self.index.drop_entry_holding(key, &record)? {
            Some(orphaned) => {
                self.delete_orphans(&orphaned)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Mark a stored body as one a peer may be given, by the declared hashes it
    /// actually satisfies.
    ///
    /// The only way anything becomes shareable, and it checks rather than
    /// trusts: the body is read back from disk (which re-verifies its content
    /// address), hashed at the strongest algorithm `declared` names, and only
    /// the declared hashes at that level which match are recorded. A weaker
    /// algorithm, or a declared hash that does not match, is never recorded
    /// even when another hash made the declaration as a whole valid. Nothing
    /// is granted for a body no entry refers to.
    ///
    /// Returns the hashes recorded, which are exactly the ones worth
    /// announcing; empty means nothing was granted.
    pub fn grant_peer_eligibility(
        &self,
        content: ContentId,
        declared: &Integrity,
    ) -> Result<Vec<crate::sri::Hash>> {
        let Some(strongest) = declared.strongest() else {
            return Ok(Vec::new());
        };
        // Shared, so the file cannot be deleted between the check and the grant.
        let _gate = self.storing();
        let bytes = match self.blobs.get(content) {
            Ok(bytes) => bytes,
            Err(err) if err.is_lost_body() => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut matching: Vec<crate::sri::Hash> = declared
            .hashes
            .iter()
            .filter(|h| h.algorithm == strongest && h.matches(&bytes))
            .cloned()
            .collect();
        matching.dedup();
        let keys: Vec<Vec<u8>> = matching
            .iter()
            .map(|h| sri_key(h.algorithm, &h.digest))
            .collect();
        if !self.index.grant_eligibility(content, &keys)? {
            return Ok(Vec::new());
        }
        Ok(matching)
    }

    /// The verified hashes a peer may name this body by. Empty: not shareable.
    pub fn peer_eligibility(&self, content: ContentId) -> Result<Vec<crate::sri::Hash>> {
        Ok(self
            .index
            .eligible_hashes(content)?
            .iter()
            .filter_map(|key| hash_from_sri_key(key))
            .collect())
    }

    /// What a peer asking for this gets: the body, if it is one we may share
    /// under this name, and `None` otherwise — including when we hold it but
    /// no page's declared integrity was ever verified against it.
    ///
    /// Never consults the entries, so it cannot say which URL a body came from.
    pub fn body_for_peer(&self, ask: PeerAsk<'_>) -> Result<Option<Vec<u8>>> {
        let (content, named) = match ask {
            PeerAsk::Content(id) => (id, None),
            PeerAsk::Integrity(hash) => match self.content_for_integrity(hash)? {
                Some(id) => (id, Some(hash)),
                None => return Ok(None),
            },
        };
        let eligible = self.index.eligible_hashes(content)?;
        if eligible.is_empty() {
            return Ok(None);
        }
        if let Some(hash) = named {
            if !eligible.contains(&sri_key(hash.algorithm, &hash.digest)) {
                return Ok(None);
            }
        }
        let bytes = match self.blobs.get(content) {
            Ok(bytes) => bytes,
            Err(err) if err.is_lost_body() => return Ok(None),
            Err(err) => return Err(err),
        };
        // The content address was re-verified by the read; the named hash is
        // checked too, because it costs one digest and a poisoned index row
        // costs more.
        if let Some(hash) = named {
            if !hash.matches(&bytes) {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    pub fn has_content(&self, id: ContentId) -> bool {
        self.blobs.contains(id)
    }

    // ---- write path --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn store(
        &self,
        partition: Option<&str>,
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
            partition,
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
        partition: Option<&str>,
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

        if let Storability::Reject(reason) = policy::storability(
            method,
            request_headers,
            &meta,
            body.len() as u64,
            &self.options,
        ) {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(reason));
        }

        let fields = vary::vary_fields(response_headers);
        let vkey = vary::vary_key(&fields, request_headers);

        if status == 206 {
            return self.store_partial(
                partition,
                method,
                &url,
                response_headers,
                body,
                request_time,
                response_time,
                provenance,
                fields,
                vkey,
            );
        }

        let gate = self.storing();
        let receipt = self.blobs.put(body)?;
        self.pause_point("store:after-put");
        if !receipt.newly_written {
            self.index.bump(counters::BYTES_DEDUPED, receipt.len)?;
        }

        let now = self.now();
        let record = EntryRecord {
            partition: partition.map(str::to_owned),
            url: url.clone(),
            method: method.to_ascii_uppercase(),
            status,
            headers: StoredHeaders::for_storage(response_headers),
            vary_fields: fields,
            vary_key: vkey,
            body: StoredBody::Complete {
                content: receipt.id.0,
                len: receipt.len,
            },
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
        let orphaned = self.index.put_entry(&record, &[(receipt.id, blob)])?;
        drop(gate);
        self.delete_orphans(&orphaned)?;
        self.index.index_sri(&sri_digests(body, receipt.id))?;
        self.index.bump(counters::STORES, 1)?;

        // RFC 9111 §4.3.5: a HEAD response says something about the stored GET,
        // and the point of storing it is to act on that rather than to sit
        // beside a GET it may have just contradicted.
        if method.eq_ignore_ascii_case("HEAD") {
            self.reconcile_head(partition, &url, request_headers, response_headers)?;
        }

        self.enforce_budget()?;

        Ok(StoreOutcome::Stored {
            content: receipt.id,
            deduped: !receipt.newly_written,
        })
    }

    /// Begin a body that will arrive in pieces.
    ///
    /// The caller writes chunks as they come and hands the writer back to
    /// [`finish_streamed`]. Nothing is visible under a content address until
    /// that happens, so an interrupted transfer leaves the store as it was.
    ///
    /// [`finish_streamed`]: Cache::finish_streamed
    pub fn begin_streamed(&self) -> Result<crate::blob::BlobWriter> {
        self.blobs.writer()
    }

    /// Store a body that arrived in pieces, now that it is all here.
    ///
    /// The same policy decisions as [`store`] apply, but the body is already on
    /// disk and its hashes were computed on the way past. A body the policy
    /// declines, or one past the size bound, is discarded rather than kept —
    /// the caller has already been given the bytes either way, which is the
    /// point: `max_body_bytes` bounds what is *cached*, not what can be fetched.
    ///
    /// [`store`]: Cache::store
    #[allow(clippy::too_many_arguments)]
    pub fn finish_streamed(
        &self,
        partition: Option<&str>,
        method: &str,
        url: &str,
        request_headers: &HeaderMap,
        status: u16,
        response_headers: &HeaderMap,
        writer: crate::blob::BlobWriter,
        request_time: u64,
        response_time: u64,
        provenance: Provenance,
    ) -> Result<StoreOutcome> {
        let url = Self::normalize_url(url);
        let len = writer.len();
        self.index.bump(counters::BYTES_FROM_ORIGIN, len)?;

        let meta = StoredMeta {
            status,
            headers: response_headers.clone(),
            request_time,
            response_time,
        };

        if let Storability::Reject(reason) =
            policy::storability(method, request_headers, &meta, len, &self.options)
        {
            writer.abandon();
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(reason));
        }
        // A partial response arriving as a stream is not merged here: combining
        // ranges needs the stored segments, and this path deliberately does not
        // read them back. It falls through to the buffered path instead.
        if status == 206 {
            writer.abandon();
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored("a streamed 206 is not combined"));
        }

        let fields = vary::vary_fields(response_headers);
        let vkey = vary::vary_key(&fields, request_headers);
        let gate = self.storing();
        let (receipt, digests) = self.blobs.commit(writer)?;
        if !receipt.newly_written {
            self.index.bump(counters::BYTES_DEDUPED, receipt.len)?;
        }

        let now = self.now();
        let record = EntryRecord {
            partition: partition.map(str::to_owned),
            url: url.clone(),
            method: method.to_ascii_uppercase(),
            status,
            headers: StoredHeaders::for_storage(response_headers),
            vary_fields: fields,
            vary_key: vkey,
            body: StoredBody::Complete {
                content: receipt.id.0,
                len: receipt.len,
            },
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
        let orphaned = self.index.put_entry(&record, &[(receipt.id, blob)])?;
        drop(gate);
        self.delete_orphans(&orphaned)?;

        let rows: Vec<(Vec<u8>, [u8; 32])> = digests
            .each()
            .into_iter()
            .map(|(algorithm, digest)| (sri_key(algorithm, digest), receipt.id.0))
            .collect();
        self.index.index_sri(&rows)?;
        self.index.bump(counters::STORES, 1)?;

        if method.eq_ignore_ascii_case("HEAD") {
            self.reconcile_head(partition, &url, request_headers, response_headers)?;
        }
        self.enforce_budget()?;

        Ok(StoreOutcome::Stored {
            content: receipt.id,
            deduped: !receipt.newly_written,
        })
    }

    /// A 206, folded into whatever partial representation is already stored.
    ///
    /// The rule that keeps this honest is RFC 9111 §3.3: ranges may only be
    /// combined when a strong validator says they come from the same
    /// representation. Without one, the new range replaces what was there rather
    /// than being stitched onto bytes that may be from a different file.
    #[allow(clippy::too_many_arguments)]
    fn store_partial(
        &self,
        partition: Option<&str>,
        method: &str,
        url: &str,
        response_headers: &HeaderMap,
        body: &[u8],
        request_time: u64,
        response_time: u64,
        provenance: Provenance,
        fields: Vec<String>,
        vkey: String,
    ) -> Result<StoreOutcome> {
        if !method.eq_ignore_ascii_case("GET") {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(
                "only a GET is stored as partial content",
            ));
        }
        if range::is_multipart(response_headers) {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(
                "multipart ranges are passed through",
            ));
        }
        let parsed = response_headers
            .get(http::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(range::parse_content_range);
        let Some(content_range) = parsed else {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(
                "206 without a usable Content-Range",
            ));
        };
        if content_range.len() != body.len() as u64 {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored(
                "206 body does not match its Content-Range",
            ));
        }

        let key = entry_key(partition, "GET", url, &vkey);
        let existing = self.index.get_entry(&key)?;

        // Is what we already hold the same representation as this range?
        let combinable = match &existing {
            Some(record) => same_representation(&record.headers.to_header_map(), response_headers),
            None => false,
        };
        if existing.is_some() && !combinable {
            let orphaned = self.index.remove_entry(&key)?;
            self.delete_orphans(&orphaned)?;
        }
        let existing = if combinable { existing } else { None };

        if let Some(record) = &existing {
            if record.body.is_complete() {
                return Ok(StoreOutcome::NotStored("the whole body is already stored"));
            }
        }

        let mut segments: Vec<Segment> = match existing.as_ref().map(|r| &r.body) {
            Some(StoredBody::Partial { segments, .. }) => segments.clone(),
            _ => Vec::new(),
        };
        let complete_len = content_range
            .complete_len
            .or_else(|| existing.as_ref().and_then(|r| r.body.complete_len()));

        // Only the bytes we do not already hold get written. That is what makes
        // a re-fetched range add to the entry rather than replace it.
        let coverage = Coverage::from_sorted(segments.iter().map(|s| (s.start, s.end)).collect());
        let (start, end) = content_range.half_open();
        let gate = self.storing();
        let mut new_blobs: Vec<(ContentId, BlobRecord)> = Vec::new();
        let now = self.now();
        for (from, to) in coverage.missing(start, end) {
            let slice = &body[(from - start) as usize..(to - start) as usize];
            let receipt = self.blobs.put(slice)?;
            if !receipt.newly_written {
                self.index.bump(counters::BYTES_DEDUPED, receipt.len)?;
            }
            segments.push(Segment {
                start: from,
                end: to,
                content: receipt.id.0,
            });
            new_blobs.push((
                receipt.id,
                BlobRecord {
                    len: receipt.len,
                    stored_len: receipt.stored_len,
                    compression: receipt.compression,
                    refcount: 0,
                    created: now,
                },
            ));
        }
        segments.sort_by_key(|s| s.start);

        // The headers describe the whole representation, not this range of it.
        let mut stored_headers = response_headers.clone();
        stored_headers.remove(http::header::CONTENT_RANGE);
        match complete_len {
            Some(total) => {
                if let Ok(value) = total.to_string().parse() {
                    stored_headers.insert(http::header::CONTENT_LENGTH, value);
                }
            }
            None => {
                stored_headers.remove(http::header::CONTENT_LENGTH);
            }
        }

        let mut record = EntryRecord {
            partition: partition.map(str::to_owned),
            url: url.to_string(),
            method: "GET".to_string(),
            // A stored partial is a stored *representation*; the 206 status
            // belongs to the exchange, and we synthesise it again on serve.
            status: 200,
            headers: StoredHeaders::for_storage(&stored_headers),
            vary_fields: fields,
            vary_key: vkey,
            body: StoredBody::Partial {
                complete_len,
                segments: segments.clone(),
            },
            request_time,
            response_time,
            stored_at: existing.as_ref().map(|r| r.stored_at).unwrap_or(now),
            last_used: now,
            hits: existing.as_ref().map(|r| r.hits).unwrap_or(0),
            provenance,
        };

        // The runs met: the entry becomes an ordinary complete one, and the
        // segment blobs fall away with their last reference.
        let mut completed: Option<ContentId> = None;
        if let Some(total) = complete_len {
            let filled = Coverage::from_sorted(segments.iter().map(|s| (s.start, s.end)).collect());
            if filled.covers(0, total) {
                let whole = self.read_span(&record, 0, total)?;
                let receipt = self.blobs.put(&whole)?;
                record.body = StoredBody::Complete {
                    content: receipt.id.0,
                    len: receipt.len,
                };
                new_blobs.push((
                    receipt.id,
                    BlobRecord {
                        len: receipt.len,
                        stored_len: receipt.stored_len,
                        compression: receipt.compression,
                        refcount: 0,
                        created: now,
                    },
                ));
                self.index.index_sri(&sri_digests(&whole, receipt.id))?;
                completed = Some(receipt.id);
            }
        }

        let orphaned = self.index.put_entry(&record, &new_blobs)?;
        drop(gate);
        self.delete_orphans(&orphaned)?;
        self.index.bump(counters::STORES, 1)?;
        if completed.is_none() {
            self.index.bump(counters::PARTIAL_STORES, 1)?;
        }
        self.enforce_budget()?;

        match completed {
            Some(content) => Ok(StoreOutcome::Stored {
                content,
                deduped: false,
            }),
            None => Ok(StoreOutcome::StoredPartial {
                held: record.body.held_len(),
                complete_len,
            }),
        }
    }

    /// RFC 9111 §4.3.5. A HEAD's headers are the same headers a GET would carry,
    /// so they either refresh the stored GET or prove it is out of date.
    fn reconcile_head(
        &self,
        partition: Option<&str>,
        url: &str,
        request_headers: &HeaderMap,
        head_headers: &HeaderMap,
    ) -> Result<()> {
        let Some((key, mut record)) =
            self.select_variant(partition, "GET", url, request_headers)?
        else {
            return Ok(());
        };
        let mut stored = record.headers.to_header_map();

        if !same_representation(&stored, head_headers) {
            let orphaned = self.index.remove_entry(&key)?;
            self.delete_orphans(&orphaned)?;
            tracing::debug!(url, "a HEAD contradicted the stored GET; invalidated it");
            return Ok(());
        }

        policy::apply_304(&mut stored, head_headers);
        record.headers = StoredHeaders::for_storage(&stored);
        self.index.refresh_entry(&record)?;
        Ok(())
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
        let mut headers = record.headers.to_header_map();
        policy::apply_304(&mut headers, fresh_headers);
        record.headers = StoredHeaders::for_storage(&headers);
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
        let want = Want {
            range: None,
            omit_body: false,
        };
        let response = match self.materialize(key.to_string(), &record, &meta, age, &want) {
            Ok(response) => response,
            // The origin confirmed a body we no longer hold. The entry goes,
            // and the error says so: the caller asks the origin again, once,
            // without a validator.
            Err(err) if err.is_lost_body() => {
                self.drop_broken(key, &record, &err)?;
                return Err(err);
            }
            Err(err) => return Err(err),
        };
        self.index
            .bump(counters::BYTES_FROM_CACHE, response.body.len() as u64)?;
        self.index.bump(counters::HITS, 1)?;
        Ok(Some(response))
    }

    /// Check a body offered by a peer. The rule is absolute: without an
    /// independent hash to check it against, the body is refused.
    ///
    /// Checked, counted, and not kept. A peer hands over bytes, not a
    /// response: there are no headers to decide freshness or storability by,
    /// and nothing an entry could be written from. Written to the blob store
    /// anyway, the bytes had no record, no entry and no reference, so garbage
    /// collection could never find them and no grant could ever make them
    /// shareable. The caller uses them for the request in hand; the next
    /// request asks again.
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
        self.index.bump(counters::PEER_ACCEPTED, 1)?;
        Ok(())
    }

    /// Unsafe methods invalidate the target URI (RFC 9111 §4.4).
    pub fn invalidate(&self, partition: Option<&str>, method: &str, url: &str) -> Result<usize> {
        if !policy::invalidates(method) {
            return Ok(0);
        }
        let url = Self::normalize_url(url);
        let mut removed = 0;
        for m in ["GET", "HEAD"] {
            let orphaned = self.index.invalidate(partition, m, &url)?;
            removed += orphaned.len();
            self.delete_orphans(&orphaned)?;
        }
        Ok(removed)
    }

    pub fn purge(&self, partition: Option<&str>, method: &str, url: &str) -> Result<()> {
        let url = Self::normalize_url(url);
        let orphaned = self.index.invalidate(partition, method, &url)?;
        self.delete_orphans(&orphaned)?;
        Ok(())
    }

    /// Delete blobs nothing points at any more, and the integrity rows that
    /// pointed at them.
    ///
    /// This is garbage collection, not eviction: it only removes what is already
    /// unreferenced. Keeping the cache inside its budget is [`enforce_budget`],
    /// which runs on every store.
    ///
    /// [`enforce_budget`]: Cache::enforce_budget
    pub fn collect_garbage(&self) -> Result<usize> {
        let orphans = self.index.orphaned_blobs()?;
        let deleted = self.delete_orphans(&orphans)?;
        let pruned = self.index.prune_sri()?;
        if pruned > 0 {
            tracing::debug!(pruned, "dropped integrity rows whose body is gone");
        }
        Ok(deleted)
    }

    /// Bring the store back under its size budget by dropping entries.
    ///
    /// Entries, never blobs: two URLs that share a body each hold a reference to
    /// it, and deleting the blob under one would take the other's storage with
    /// it. So an entry is dropped, its references are released, and only a blob
    /// whose last reference has gone is actually deleted — which is also why the
    /// running total is only reduced when that happens.
    pub fn enforce_budget(&self) -> Result<usize> {
        let Some(capacity) = self.options.capacity_bytes else {
            return Ok(0);
        };
        let (_, _, mut on_disk, _) = self.index.blob_totals()?;
        if on_disk <= capacity {
            return Ok(0);
        }
        let target = (capacity as f64 * self.options.evict_to_fraction.clamp(0.1, 1.0)) as u64;

        let mut candidates = self.index.all_entries()?;
        order_for_eviction(&mut candidates, self.options.eviction);

        let mut evicted = 0;
        for (key, _) in candidates {
            if on_disk <= target {
                break;
            }
            let orphaned = self.index.remove_entry(&key)?;
            for id in &orphaned {
                if let Some(blob) = self.index.get_blob(*id)? {
                    on_disk = on_disk.saturating_sub(blob.stored_len);
                }
            }
            self.delete_orphans(&orphaned)?;
            evicted += 1;
        }
        if evicted > 0 {
            self.index.bump(counters::EVICTIONS, evicted as u64)?;
            tracing::debug!(
                evicted,
                on_disk,
                capacity,
                "evicted to stay inside the budget"
            );
        }
        Ok(evicted)
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
        stats.sri_rows = self.index.sri_row_count()?;
        for (name, value) in self.index.counters()? {
            stats.apply_counter(&name, value);
        }
        Ok(stats)
    }
}

/// How a peer names the body it wants.
#[derive(Debug, Clone, Copy)]
pub enum PeerAsk<'a> {
    Content(ContentId),
    Integrity(&'a crate::sri::Hash),
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

/// The hash an [`sri_key`] was made from.
fn hash_from_sri_key(key: &[u8]) -> Option<crate::sri::Hash> {
    use crate::sri::Algorithm;
    let (tag, digest) = key.split_first()?;
    let algorithm = match tag {
        1 => Algorithm::Sha256,
        2 => Algorithm::Sha384,
        3 => Algorithm::Sha512,
        _ => return None,
    };
    Some(crate::sri::Hash {
        algorithm,
        digest: digest.to_vec(),
    })
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

/// Whether two sets of headers describe the same representation.
///
/// Combining ranges from different representations produces a body that never
/// existed, under a hash that says it did. RFC 9111 §3.3 wants a strong
/// validator before that is allowed, so a weak `ETag`, a mismatch, or no
/// validator at all all mean "not the same".
fn same_representation(stored: &HeaderMap, fresh: &HeaderMap) -> bool {
    let etag = |h: &HeaderMap| {
        h.get(http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.starts_with("W/"))
    };
    if let (Some(a), Some(b)) = (etag(stored), etag(fresh)) {
        return a == b;
    }
    match (
        crate::headers::header_date(stored, http::header::LAST_MODIFIED),
        crate::headers::header_date(fresh, http::header::LAST_MODIFIED),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Sort entries worst-first for eviction, by the configured policy.
fn order_for_eviction(entries: &mut [(String, EntryRecord)], policy: crate::policy::Eviction) {
    use crate::policy::Eviction;
    match policy {
        Eviction::LeastRecentlyUsed => entries.sort_by_key(|(_, r)| r.last_used),
        Eviction::LeastFrequentlyUsed => entries.sort_by_key(|(_, r)| (r.hits, r.last_used)),
        Eviction::Cost => entries.sort_by(|(_, a), (_, b)| {
            let value = |r: &EntryRecord| (r.hits + 1) as f64 / r.body.held_len().max(1) as f64;
            value(a)
                .partial_cmp(&value(b))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.last_used.cmp(&b.last_used))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(cache: &Cache, url: &str, body: &[u8]) {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CACHE_CONTROL, "max-age=600".parse().unwrap());
        let now = cache.now();
        cache
            .store(
                None,
                "GET",
                url,
                &HeaderMap::new(),
                200,
                &headers,
                body,
                now,
                now,
            )
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

    fn with_cookie() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CACHE_CONTROL, "max-age=600".parse().unwrap());
        headers.append(http::header::SET_COOKIE, "a=1".parse().unwrap());
        headers.append(http::header::SET_COOKIE, "b=2".parse().unwrap());
        headers
    }

    fn only_entry(cache: &Cache) -> (String, EntryRecord) {
        let mut all = cache.index.all_entries().unwrap();
        assert_eq!(all.len(), 1);
        all.remove(0)
    }

    #[test]
    fn what_is_written_to_the_index_carries_no_cookie() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let now = cache.now();
        cache
            .store(
                None,
                "GET",
                "https://a.test/c",
                &HeaderMap::new(),
                200,
                &with_cookie(),
                b"x",
                now,
                now,
            )
            .unwrap();
        let (_, record) = only_entry(&cache);
        assert!(!record
            .headers
            .pairs()
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("set-cookie")));

        // The streamed path writes through the same boundary.
        let mut writer = cache.begin_streamed().unwrap();
        writer.write(b"streamed").unwrap();
        cache
            .finish_streamed(
                None,
                "GET",
                "https://a.test/streamed",
                &HeaderMap::new(),
                200,
                &with_cookie(),
                writer,
                now,
                now,
                Provenance::Origin,
            )
            .unwrap();
        for (_, record) in cache.index.all_entries().unwrap() {
            assert!(!record
                .headers
                .pairs()
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("set-cookie")));
        }
    }

    #[test]
    fn an_entry_written_before_the_rule_is_served_without_its_cookie() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://a.test/legacy", b"old");

        // What v0.1.3 wrote: the same bincode layout, cookie included.
        let (_, mut record) = only_entry(&cache);
        let legacy: Vec<(String, String)> = vec![
            ("cache-control".into(), "max-age=600".into()),
            ("set-cookie".into(), "stale=1".into()),
        ];
        record.headers = bincode::deserialize(&bincode::serialize(&legacy).unwrap()).unwrap();
        cache.index.refresh_entry(&record).unwrap();

        match cache
            .lookup(None, "GET", "https://a.test/legacy", &HeaderMap::new())
            .unwrap()
        {
            Lookup::Fresh(response) => {
                assert!(response.headers.get(http::header::SET_COOKIE).is_none());
                assert!(response
                    .meta
                    .headers
                    .get(http::header::SET_COOKIE)
                    .is_none());
                assert_eq!(response.body, b"old");
            }
            other => panic!("expected a fresh hit, got {other:?}"),
        }
    }

    // ------------------------------------------- bodies that went missing

    fn get(cache: &Cache, url: &str) -> Lookup {
        cache.lookup(None, "GET", url, &HeaderMap::new()).unwrap()
    }

    fn blob_path(cache: &Cache, body: &[u8]) -> std::path::PathBuf {
        cache.blobs.path_for(ContentId::of(body))
    }

    fn assert_miss_for_lost_body(lookup: Lookup) {
        match lookup {
            Lookup::Miss(reason) => assert_eq!(reason, "stored body missing or corrupt"),
            other => panic!("expected a miss for a lost body, got {other:?}"),
        }
    }

    #[test]
    fn a_deleted_body_is_a_miss_and_the_entry_goes_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://a.test/gone", b"vanishing");
        std::fs::remove_file(blob_path(&cache, b"vanishing")).unwrap();

        assert_miss_for_lost_body(get(&cache, "https://a.test/gone"));
        assert_eq!(cache.index.entry_count().unwrap(), 0);
        assert!(cache
            .index
            .get_blob(ContentId::of(b"vanishing"))
            .unwrap()
            .is_none());
        assert!(cache
            .index
            .variant_keys(None, "GET", "https://a.test/gone")
            .unwrap()
            .is_empty());
        assert_eq!(cache.stats().unwrap().corrupt_entries, 1);

        // And it can be stored again, and served.
        stored(&cache, "https://a.test/gone", b"vanishing");
        assert!(matches!(
            get(&cache, "https://a.test/gone"),
            Lookup::Fresh(_)
        ));
    }

    #[test]
    fn a_corrupt_body_is_a_miss_and_the_entry_goes_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://a.test/rot", b"original bytes");
        std::fs::write(blob_path(&cache, b"original bytes"), [0u8, b'x']).unwrap();

        assert_miss_for_lost_body(get(&cache, "https://a.test/rot"));
        assert_eq!(cache.index.entry_count().unwrap(), 0);
    }

    #[test]
    fn a_partial_entry_missing_a_segment_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let url = "https://a.test/video";
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CACHE_CONTROL, "max-age=600".parse().unwrap());
        headers.insert(http::header::ETAG, "\"v1\"".parse().unwrap());
        headers.insert(
            http::header::CONTENT_RANGE,
            "bytes 0-9/100".parse().unwrap(),
        );
        let now = cache.now();
        let outcome = cache
            .store(
                None,
                "GET",
                url,
                &HeaderMap::new(),
                206,
                &headers,
                b"0123456789",
                now,
                now,
            )
            .unwrap();
        assert!(matches!(outcome, StoreOutcome::StoredPartial { .. }));
        std::fs::remove_file(blob_path(&cache, b"0123456789")).unwrap();

        let mut request = HeaderMap::new();
        request.insert(http::header::RANGE, "bytes=0-4".parse().unwrap());
        assert_miss_for_lost_body(cache.lookup(None, "GET", url, &request).unwrap());
        assert_eq!(cache.index.entry_count().unwrap(), 0);
    }

    #[test]
    fn dropping_one_broken_entry_leaves_another_that_shares_the_blob() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://a.test/one", b"shared body");
        stored(&cache, "https://a.test/two", b"shared body");
        let id = ContentId::of(b"shared body");
        std::fs::remove_file(blob_path(&cache, b"shared body")).unwrap();

        // Only /one is asked for. It goes, and only its reference goes with it.
        assert_miss_for_lost_body(get(&cache, "https://a.test/one"));
        assert_eq!(cache.index.get_blob(id).unwrap().unwrap().refcount, 1);
        assert_eq!(cache.index.entry_count().unwrap(), 1);

        // /two is untouched. Once the bytes exist again it is served.
        stored(&cache, "https://a.test/three", b"shared body");
        match get(&cache, "https://a.test/two") {
            Lookup::Fresh(response) => assert_eq!(response.body, b"shared body"),
            other => panic!("the other entry should still be served, got {other:?}"),
        }
    }

    #[test]
    fn a_replacement_stored_meanwhile_is_not_dropped_for_the_old_body() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://a.test/page", b"old body");
        let (key, old) = only_entry(&cache);

        // The old body is found broken, but before the entry is dropped a new
        // response replaces it.
        stored(&cache, "https://a.test/page", b"new body");
        cache
            .drop_broken(&key, &old, &CacheError::MissingBlob("old".into()))
            .unwrap();

        match get(&cache, "https://a.test/page") {
            Lookup::Fresh(response) => assert_eq!(response.body, b"new body"),
            other => panic!("the replacement should survive, got {other:?}"),
        }
        assert_eq!(cache.stats().unwrap().corrupt_entries, 0);
    }

    #[test]
    fn a_replacement_with_the_same_bytes_and_newer_headers_is_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let url = "https://a.test/same-bytes";
        let store_with_etag = |etag: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(http::header::CACHE_CONTROL, "max-age=600".parse().unwrap());
            headers.insert(http::header::ETAG, etag.parse().unwrap());
            let now = cache.now();
            cache
                .store(
                    None,
                    "GET",
                    url,
                    &HeaderMap::new(),
                    200,
                    &headers,
                    b"unchanged bytes",
                    now,
                    now,
                )
                .unwrap();
        };
        store_with_etag("\"v1\"");
        let (key, old) = only_entry(&cache);

        // The old entry is found broken; before it is dropped, the origin's
        // newer response replaces it — the same body, different metadata.
        store_with_etag("\"v2\"");
        let (_, current) = only_entry(&cache);
        assert_eq!(current.body, old.body, "the same bytes, so the same body");
        assert_ne!(current, old);

        cache
            .drop_broken(&key, &old, &CacheError::MissingBlob("old".into()))
            .unwrap();
        match get(&cache, url) {
            Lookup::Fresh(response) => {
                assert_eq!(response.headers.get(http::header::ETAG).unwrap(), "\"v2\"")
            }
            other => panic!("the replacement should survive, got {other:?}"),
        }
        assert_eq!(cache.stats().unwrap().corrupt_entries, 0);
    }

    #[test]
    fn many_lookups_of_one_broken_entry_drop_it_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(dir.path()).unwrap());
        stored(&cache, "https://a.test/busy", b"popular");
        std::fs::remove_file(blob_path(&cache, b"popular")).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    get(&cache, "https://a.test/busy")
                })
            })
            .collect();
        for thread in threads {
            match thread.join().unwrap() {
                Lookup::Miss(_) => {}
                other => panic!("every lookup should miss, got {other:?}"),
            }
        }

        assert_eq!(cache.stats().unwrap().corrupt_entries, 1);
        assert_eq!(cache.index.entry_count().unwrap(), 0);
        assert!(cache
            .index
            .get_blob(ContentId::of(b"popular"))
            .unwrap()
            .is_none());
        assert!(cache.index.orphaned_blobs().unwrap().is_empty());
    }

    #[test]
    fn a_store_that_reuses_a_file_is_never_left_pointing_at_a_deleted_one() {
        // The interleaving this guards against, made to happen on purpose: a
        // blob whose last reference has gone but whose file is still on disk;
        // a store of the same bytes finds the file and pauses before taking
        // its reference; garbage collection runs. Without the gate the file is
        // deleted under the store, and the entry it then writes names bytes
        // that are gone.
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(dir.path()).unwrap());
        stored(&cache, "https://a.test/first", b"reused");
        let (key, _) = only_entry(&cache);
        // Release the reference without deleting the file.
        let released = cache.index.remove_entry(&key).unwrap();
        assert_eq!(released, vec![ContentId::of(b"reused")]);
        assert!(blob_path(&cache, b"reused").exists());

        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();
        let resume_rx = std::sync::Mutex::new(resume_rx);
        *cache.pause.lock().unwrap() = Some(Arc::new(move |point| {
            if point == "store:after-put" {
                paused_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
            }
        }));

        let storing = {
            let cache = cache.clone();
            std::thread::spawn(move || stored(&cache, "https://a.test/second", b"reused"))
        };
        paused_rx.recv().unwrap();
        *cache.pause.lock().unwrap() = None;

        let (collected_tx, collected_rx) = std::sync::mpsc::channel::<usize>();
        let collecting = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                collected_tx.send(cache.collect_garbage().unwrap()).unwrap();
            })
        };
        // With the gate, collection waits for the store and this times out;
        // without it, collection finishes here and deletes the file. Either
        // way the store is then let go, and the outcome is what is checked.
        let early = collected_rx.recv_timeout(std::time::Duration::from_millis(500));
        resume_tx.send(()).unwrap();
        storing.join().unwrap();
        collecting.join().unwrap();
        let deleted = early.or_else(|_| collected_rx.recv()).unwrap();

        assert_eq!(
            deleted, 0,
            "the blob was deleted while a store was using it"
        );
        assert!(blob_path(&cache, b"reused").exists());
        match get(&cache, "https://a.test/second") {
            Lookup::Fresh(response) => assert_eq!(response.body, b"reused"),
            other => panic!("the new entry should be served, got {other:?}"),
        }
    }

    #[test]
    fn a_304_for_a_body_that_is_gone_drops_the_entry_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://a.test/confirmed", b"no longer here");
        let (key, _) = only_entry(&cache);
        std::fs::remove_file(blob_path(&cache, b"no longer here")).unwrap();

        let err = cache
            .record_not_modified(&key, &HeaderMap::new(), cache.now(), cache.now())
            .unwrap_err();
        assert!(err.is_lost_body(), "{err}");
        assert_eq!(cache.index.entry_count().unwrap(), 0);
        assert_eq!(cache.stats().unwrap().corrupt_entries, 1);
    }

    // ------------------------------------------------- peer eligibility

    use crate::sri::{Algorithm, Hash};

    fn declared(hashes: &[Hash]) -> Integrity {
        Integrity {
            hashes: hashes.to_vec(),
        }
    }

    #[test]
    fn a_grant_records_only_matching_hashes_at_the_strongest_level() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = b"library code";
        stored(&cache, "https://cdn.test/lib.js", body);
        let id = ContentId::of(body);

        let weak = Hash::compute(Algorithm::Sha256, body);
        let strong = Hash::compute(Algorithm::Sha384, body);
        let wrong = Hash {
            algorithm: Algorithm::Sha384,
            digest: vec![9; 48],
        };
        let granted = cache
            .grant_peer_eligibility(
                id,
                &declared(&[weak.clone(), wrong.clone(), strong.clone()]),
            )
            .unwrap();
        assert_eq!(granted, vec![strong.clone()]);
        assert_eq!(cache.peer_eligibility(id).unwrap(), vec![strong.clone()]);

        assert_eq!(
            cache.body_for_peer(PeerAsk::Content(id)).unwrap().unwrap(),
            body
        );
        assert!(cache
            .body_for_peer(PeerAsk::Integrity(&strong))
            .unwrap()
            .is_some());
        // Valid, declared, and weaker: never recorded, never served under.
        assert!(cache
            .body_for_peer(PeerAsk::Integrity(&weak))
            .unwrap()
            .is_none());
        assert!(cache
            .body_for_peer(PeerAsk::Integrity(&wrong))
            .unwrap()
            .is_none());
    }

    #[test]
    fn nothing_is_granted_for_bytes_that_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        stored(&cache, "https://cdn.test/lib.js", b"what was served");
        let id = ContentId::of(b"what was served");

        let other = Hash::compute(Algorithm::Sha512, b"what the page declared");
        assert!(cache
            .grant_peer_eligibility(id, &declared(&[other]))
            .unwrap()
            .is_empty());
        assert!(cache
            .grant_peer_eligibility(id, &Integrity::default())
            .unwrap()
            .is_empty());
        assert!(cache.peer_eligibility(id).unwrap().is_empty());
        assert!(cache.body_for_peer(PeerAsk::Content(id)).unwrap().is_none());
    }

    /// Every file under the blob store, temporary or not.
    fn every_blob_file(cache: &Cache) -> Vec<std::path::PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(cache.blobs.root(), &mut out);
        out
    }

    #[test]
    fn an_accepted_peer_body_is_not_written_anywhere() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = b"from a peer, verified";
        let hash = Hash::compute(Algorithm::Sha384, body);
        cache
            .accept_peer_body(&PeerProof::Integrity(declared(&[hash])), body)
            .unwrap();
        cache
            .accept_peer_body(&PeerProof::Content(ContentId::of(body)), body)
            .unwrap();
        assert!(
            every_blob_file(&cache).is_empty(),
            "{:?}",
            every_blob_file(&cache)
        );
        assert!(!cache.has_content(ContentId::of(body)));
        assert_eq!(cache.stats().unwrap().peer_accepted, 2);
    }

    #[test]
    fn a_body_no_entry_refers_to_is_never_granted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = b"from a peer";
        let hash = Hash::compute(Algorithm::Sha384, body);
        cache
            .accept_peer_body(
                &PeerProof::Integrity(declared(std::slice::from_ref(&hash))),
                body,
            )
            .unwrap();
        assert!(cache
            .grant_peer_eligibility(ContentId::of(body), &declared(&[hash]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_stored_body_is_not_shareable_until_granted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = b"an ordinary page";
        stored(&cache, "https://a.test/page", body);
        let id = ContentId::of(body);
        // Indexed under every SRI digest, as every stored body is.
        let sha = Hash::compute(Algorithm::Sha384, body);
        assert_eq!(cache.content_for_integrity(&sha).unwrap(), Some(id));

        assert!(cache.body_for_peer(PeerAsk::Content(id)).unwrap().is_none());
        assert!(cache
            .body_for_peer(PeerAsk::Integrity(&sha))
            .unwrap()
            .is_none());
    }

    #[test]
    fn eligibility_goes_with_the_blob() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = b"shared then evicted";
        stored(&cache, "https://cdn.test/a.js", body);
        let id = ContentId::of(body);
        let hash = Hash::compute(Algorithm::Sha384, body);
        assert!(!cache
            .grant_peer_eligibility(id, &declared(std::slice::from_ref(&hash)))
            .unwrap()
            .is_empty());

        cache.purge(None, "GET", "https://cdn.test/a.js").unwrap();
        assert!(cache.peer_eligibility(id).unwrap().is_empty());

        // Stored again, it is not shareable again until it is verified again.
        stored(&cache, "https://cdn.test/a.js", body);
        assert!(cache.body_for_peer(PeerAsk::Content(id)).unwrap().is_none());
    }

    #[test]
    fn a_lost_body_stops_being_shareable_even_while_still_referenced() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = b"two entries, one blob";
        stored(&cache, "https://cdn.test/one.js", body);
        stored(&cache, "https://cdn.test/two.js", body);
        let id = ContentId::of(body);
        let hash = Hash::compute(Algorithm::Sha384, body);
        assert!(!cache
            .grant_peer_eligibility(id, &declared(&[hash]))
            .unwrap()
            .is_empty());

        std::fs::remove_file(blob_path(&cache, body)).unwrap();
        assert_miss_for_lost_body(get(&cache, "https://cdn.test/one.js"));
        assert_eq!(cache.index.get_blob(id).unwrap().unwrap().refcount, 1);
        assert!(cache.peer_eligibility(id).unwrap().is_empty());
    }

    #[test]
    fn eligibility_is_gone_before_the_file_is() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(dir.path()).unwrap());
        let body = b"deleted in order";
        stored(&cache, "https://cdn.test/order.js", body);
        let id = ContentId::of(body);
        let hash = Hash::compute(Algorithm::Sha384, body);
        cache
            .grant_peer_eligibility(id, &declared(&[hash]))
            .unwrap();

        let observed = Arc::new(std::sync::Mutex::new(None));
        {
            let observed = observed.clone();
            let path = blob_path(&cache, body);
            let cache_for_hook = Arc::downgrade(&cache);
            *cache.pause.lock().unwrap() = Some(Arc::new(move |point| {
                if point == "delete:forgotten" {
                    let cache = cache_for_hook.upgrade().unwrap();
                    *observed.lock().unwrap() = Some((
                        cache.index.eligible_hashes(id).unwrap().is_empty(),
                        path.exists(),
                    ));
                }
            }));
        }
        cache
            .purge(None, "GET", "https://cdn.test/order.js")
            .unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            Some((true, true)),
            "at the moment before the file goes, eligibility must already be gone"
        );
        assert!(!blob_path(&cache, body).exists());
    }

    #[test]
    fn opening_the_cache_sweeps_what_interrupted_writes_left() {
        let dir = tempfile::tempdir().unwrap();
        {
            let cache = Cache::open(dir.path()).unwrap();
            stored(&cache, "https://a.test/kept", b"kept body");
            let path = blob_path(&cache, b"kept body");
            let hex = ContentId::of(b"kept body").to_hex();
            std::fs::write(
                path.with_file_name(format!(".{hex}.tmp-1-0102030405060708")),
                b"torn",
            )
            .unwrap();
            std::fs::write(path.with_file_name(format!("{hex}.tmp77")), b"torn").unwrap();
        }
        let reopened = Cache::open(dir.path()).unwrap();
        assert_eq!(reopened.stats().unwrap().swept_temporaries, 2);
        match get(&reopened, "https://a.test/kept") {
            Lookup::Fresh(response) => assert_eq!(response.body, b"kept body"),
            other => panic!("the real blob should survive the sweep, got {other:?}"),
        }
    }
}
