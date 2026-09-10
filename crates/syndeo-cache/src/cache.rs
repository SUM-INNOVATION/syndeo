//! The cache facade: index plus blob store plus RFC 9111 policy.

use crate::blob::{BlobStore, ContentId};
use crate::error::{CacheError, Result};
use crate::headers::{now_secs, sanitize};
use crate::index::{entry_key, BlobRecord, EntryRecord, Index, Provenance, Segment, StoredBody};
use crate::policy::{self, CacheOptions, Freshness, StoredMeta, Storability};
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
    Stored { content: ContentId, deduped: bool },
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

        // RFC 9111 §4: a stored GET can answer a HEAD, because the GET's headers
        // are exactly what a HEAD response is. Its own stored variant is tried
        // first; the fallback is what saves the origin round trip.
        let head = method.eq_ignore_ascii_case("HEAD");
        let selected = match self.select_variant(method, &url, request_headers)? {
            Some(found) => Some(found),
            None if head => self.select_variant("GET", &url, request_headers)?,
            None => None,
        };
        let Some((key, record)) = selected else {
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

        // Which bytes this request wants out of what we hold. Ranges are decided
        // here rather than in `policy::evaluate` because resolving one needs the
        // stored length, which the policy layer deliberately does not know.
        let want = match self.wanted_bytes(request_headers, &record, &meta) {
            Wanted::Unusable(reason) => {
                self.index.bump(counters::MISSES, 1)?;
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
                let response = self.materialize(key.clone(), &record, &meta, age, &want)?;
                self.index.touch(&key, now)?;
                self.index.bump(counters::HITS, 1)?;
                if want.range.is_some() {
                    self.index.bump(counters::RANGE_HITS, 1)?;
                }
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
                let response = self.materialize(key.clone(), &record, &meta, age, &want)?;
                self.index.touch(&key, now)?;
                self.index.bump(counters::STALE_HITS, 1)?;
                if want.range.is_some() {
                    self.index.bump(counters::RANGE_HITS, 1)?;
                }
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
                // A stale partial has no whole body to serve if revalidation
                // succeeds and nothing to fall back on if it does not. Refetch.
                if !record.body.is_complete() {
                    self.index.bump(counters::MISSES, 1)?;
                    return Ok(Lookup::Miss("a stale partial entry is refetched, not revalidated"));
                }
                let response = self.materialize(key.clone(), &record, &meta, age, &want)?;
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
            meta: meta.clone(),
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

        if status == 206 {
            return self.store_partial(
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

        let receipt = self.blobs.put(body)?;
        if !receipt.newly_written {
            self.index.bump(counters::BYTES_DEDUPED, receipt.len)?;
        }

        let now = self.now();
        let record = EntryRecord {
            url: url.clone(),
            method: method.to_ascii_uppercase(),
            status,
            headers: sanitize(response_headers),
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
        self.drop_blobs(&orphaned)?;
        self.index.index_sri(&sri_digests(body, receipt.id))?;
        self.index.bump(counters::STORES, 1)?;

        // RFC 9111 §4.3.5: a HEAD response says something about the stored GET,
        // and the point of storing it is to act on that rather than to sit
        // beside a GET it may have just contradicted.
        if method.eq_ignore_ascii_case("HEAD") {
            self.reconcile_head(&url, request_headers, response_headers)?;
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
        let (receipt, digests) = self.blobs.commit(writer)?;
        if !receipt.newly_written {
            self.index.bump(counters::BYTES_DEDUPED, receipt.len)?;
        }

        let now = self.now();
        let record = EntryRecord {
            url: url.clone(),
            method: method.to_ascii_uppercase(),
            status,
            headers: sanitize(response_headers),
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
        self.drop_blobs(&orphaned)?;

        let rows: Vec<(Vec<u8>, [u8; 32])> = digests
            .each()
            .into_iter()
            .map(|(algorithm, digest)| (sri_key(algorithm, digest), receipt.id.0))
            .collect();
        self.index.index_sri(&rows)?;
        self.index.bump(counters::STORES, 1)?;

        if method.eq_ignore_ascii_case("HEAD") {
            self.reconcile_head(&url, request_headers, response_headers)?;
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
            return Ok(StoreOutcome::NotStored("only a GET is stored as partial content"));
        }
        if range::is_multipart(response_headers) {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored("multipart ranges are passed through"));
        }
        let parsed = response_headers
            .get(http::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(range::parse_content_range);
        let Some(content_range) = parsed else {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored("206 without a usable Content-Range"));
        };
        if content_range.len() != body.len() as u64 {
            self.index.bump(counters::REJECTS, 1)?;
            return Ok(StoreOutcome::NotStored("206 body does not match its Content-Range"));
        }

        let key = entry_key("GET", url, &vkey);
        let existing = self.index.get_entry(&key)?;

        // Is what we already hold the same representation as this range?
        let combinable = match &existing {
            Some(record) => {
                same_representation(&to_header_map(&record.headers), response_headers)
            }
            None => false,
        };
        if existing.is_some() && !combinable {
            let orphaned = self.index.remove_entry(&key)?;
            self.drop_blobs(&orphaned)?;
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
            url: url.to_string(),
            method: "GET".to_string(),
            // A stored partial is a stored *representation*; the 206 status
            // belongs to the exchange, and we synthesise it again on serve.
            status: 200,
            headers: sanitize(&stored_headers),
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
        self.drop_blobs(&orphaned)?;
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
        url: &str,
        request_headers: &HeaderMap,
        head_headers: &HeaderMap,
    ) -> Result<()> {
        let Some((key, mut record)) = self.select_variant("GET", url, request_headers)? else {
            return Ok(());
        };
        let mut stored = to_header_map(&record.headers);

        if !same_representation(&stored, head_headers) {
            let orphaned = self.index.remove_entry(&key)?;
            self.drop_blobs(&orphaned)?;
            tracing::debug!(url, "a HEAD contradicted the stored GET; invalidated it");
            return Ok(());
        }

        policy::apply_304(&mut stored, head_headers);
        record.headers = sanitize(&stored);
        self.index.refresh_entry(&record)?;
        Ok(())
    }

    /// Delete blobs whose last reference has gone, and forget their records.
    fn drop_blobs(&self, orphaned: &[ContentId]) -> Result<()> {
        for id in orphaned {
            self.blobs.remove(*id)?;
            self.index.forget_blob(*id)?;
        }
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
        let want = Want {
            range: None,
            omit_body: false,
        };
        let response = self.materialize(key.to_string(), &record, &meta, age, &want)?;
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
            let orphaned = self.index.invalidate(m, &url)?;
            removed += orphaned.len();
            self.drop_blobs(&orphaned)?;
        }
        Ok(removed)
    }

    pub fn purge(&self, method: &str, url: &str) -> Result<()> {
        let url = Self::normalize_url(url);
        let orphaned = self.index.invalidate(method, &url)?;
        self.drop_blobs(&orphaned)
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
        for id in &orphans {
            self.blobs.remove(*id)?;
            self.index.forget_blob(*id)?;
        }
        let pruned = self.index.prune_sri()?;
        if pruned > 0 {
            tracing::debug!(pruned, "dropped integrity rows whose body is gone");
        }
        Ok(orphans.len())
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
            for id in self.index.remove_entry(&key)? {
                if let Some(blob) = self.index.get_blob(id)? {
                    on_disk = on_disk.saturating_sub(blob.stored_len);
                }
                self.blobs.remove(id)?;
                self.index.forget_blob(id)?;
            }
            evicted += 1;
        }
        if evicted > 0 {
            self.index.bump(counters::EVICTIONS, evicted as u64)?;
            tracing::debug!(evicted, on_disk, capacity, "evicted to stay inside the budget");
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
