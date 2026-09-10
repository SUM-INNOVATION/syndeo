//! RFC 9111 freshness, storability and revalidation, as pure functions.
//!
//! Nothing here touches disk. That is deliberate: the semantics are the product,
//! so they get to be tested exhaustively without a store in the way.

use crate::headers::{header_date, CacheControl};
use crate::vary;
use http::HeaderMap;

/// Statuses a cache may store without explicit origin permission (RFC 9110 §15,
/// "heuristically cacheable").
pub const HEURISTICALLY_CACHEABLE: &[u16] = &[
    200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 501,
];

/// Which entry to drop first when the store is over its budget.
///
/// Eviction works on *entries*, never on blobs: two URLs sharing one body each
/// hold a reference to it, and dropping the blob under either would take the
/// other's storage with it. Blobs go when their last reference goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eviction {
    /// Oldest `last_used` first. The default: cheap, predictable, and hard to
    /// argue with when a user asks why something was dropped.
    LeastRecentlyUsed,
    /// Fewest hits first, ties broken by `last_used`.
    LeastFrequentlyUsed,
    /// Lowest `(hits + 1) / bytes` first, ties broken by `last_used`: a large
    /// body has to earn its space, a small one barely has to.
    Cost,
}

#[derive(Debug, Clone)]
pub struct CacheOptions {
    /// A shared cache honours `s-maxage`, refuses `private`, and is strict about
    /// `Authorization`. A browser cache is private; the measuring proxy is shared.
    pub shared: bool,
    /// Fraction of (Date - Last-Modified) used when no explicit expiry exists.
    pub heuristic_fraction: f64,
    /// Ceiling on heuristic freshness, in seconds.
    pub max_heuristic_lifetime: u64,
    /// Largest body we are willing to store, in bytes.
    pub max_body_bytes: u64,
    /// Ceiling on what the blob store may occupy on disk. `None` means the cache
    /// grows without limit, which is only ever right for a test or a
    /// measurement run.
    pub capacity_bytes: Option<u64>,
    /// Which entry goes first when the ceiling is passed.
    pub eviction: Eviction,
    /// How far under the ceiling one eviction pass takes us. Evicting back to
    /// exactly the ceiling would mean evicting again on the very next store.
    pub evict_to_fraction: f64,
}

impl Default for CacheOptions {
    fn default() -> Self {
        CacheOptions {
            shared: false,
            heuristic_fraction: 0.1,
            max_heuristic_lifetime: 24 * 60 * 60,
            max_body_bytes: 256 * 1024 * 1024,
            capacity_bytes: Some(2 * 1024 * 1024 * 1024),
            eviction: Eviction::LeastRecentlyUsed,
            evict_to_fraction: 0.9,
        }
    }
}

/// The response metadata a freshness decision needs, independent of storage.
#[derive(Debug, Clone)]
pub struct StoredMeta {
    pub status: u16,
    pub headers: HeaderMap,
    /// Unix seconds at which we sent the request that produced this response.
    pub request_time: u64,
    /// Unix seconds at which we finished receiving it.
    pub response_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Serve directly from cache.
    Fresh { age: u64, lifetime: u64 },
    /// Stale, but serving it is permitted. `refresh_in_background` distinguishes
    /// `stale-while-revalidate` from a client that asked for staleness itself.
    ServeStale {
        age: u64,
        lifetime: u64,
        refresh_in_background: bool,
        reason: &'static str,
    },
    /// Contact the origin, conditionally. If that fails, `stale_if_error` says
    /// for how many more seconds the stored body may still be served.
    Revalidate {
        age: u64,
        lifetime: u64,
        stale_if_error: Option<u64>,
        reason: &'static str,
    },
    /// This entry cannot answer this request at all.
    Unusable(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storability {
    Store,
    Reject(&'static str),
}

pub fn is_cacheable_method(method: &str) -> bool {
    matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD")
}

/// Unsafe methods invalidate any stored entry for the target URI (RFC 9111 §4.4).
pub fn invalidates(method: &str) -> bool {
    !matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "TRACE"
    )
}

pub fn is_heuristically_cacheable(status: u16) -> bool {
    HEURISTICALLY_CACHEABLE.contains(&status)
}

/// Explicit freshness lifetime, in seconds, or `None` when only a heuristic
/// applies. Returns `(lifetime, was_heuristic)`.
pub fn freshness_lifetime(meta: &StoredMeta, opts: &CacheOptions) -> (u64, bool) {
    let cc = CacheControl::from_headers(&meta.headers);

    if opts.shared {
        if let Some(s) = cc.s_maxage {
            return (s, false);
        }
    }
    if let Some(m) = cc.max_age {
        return (m, false);
    }
    // Expires is relative to the origin's Date, not to our clock.
    if let Some(expires) = header_date(&meta.headers, http::header::EXPIRES) {
        let date = header_date(&meta.headers, http::header::DATE).unwrap_or(meta.response_time);
        return (expires.saturating_sub(date), false);
    }
    // An unparsable Expires (including the common `Expires: 0`) means "already
    // expired", which httpdate rejects; treat presence-but-unparsable as stale.
    if meta.headers.contains_key(http::header::EXPIRES) {
        return (0, false);
    }

    // A permanent redirect with no directives is exactly what heuristic
    // freshness is for: the origin has said the move is permanent, and a cache
    // that refetches it on every navigation is doing nothing useful.
    if matches!(meta.status, 301 | 308) {
        return (opts.max_heuristic_lifetime, true);
    }

    if let Some(last_modified) = header_date(&meta.headers, http::header::LAST_MODIFIED) {
        let date = header_date(&meta.headers, http::header::DATE).unwrap_or(meta.response_time);
        let delta = date.saturating_sub(last_modified) as f64;
        let heuristic = (delta * opts.heuristic_fraction) as u64;
        return (heuristic.min(opts.max_heuristic_lifetime), true);
    }

    (0, true)
}

/// RFC 9111 §4.2.3.
pub fn current_age(meta: &StoredMeta, now: u64) -> u64 {
    let date = header_date(&meta.headers, http::header::DATE).unwrap_or(meta.response_time);
    let age_value = meta
        .headers
        .get(http::header::AGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let apparent_age = meta.response_time.saturating_sub(date);
    let response_delay = meta.response_time.saturating_sub(meta.request_time);
    let corrected_age_value = age_value.saturating_add(response_delay);
    let corrected_initial_age = apparent_age.max(corrected_age_value);
    let resident_time = now.saturating_sub(meta.response_time);
    corrected_initial_age.saturating_add(resident_time)
}

/// May this response be written to the store?
pub fn storability(
    method: &str,
    request_headers: &HeaderMap,
    meta: &StoredMeta,
    body_len: u64,
    opts: &CacheOptions,
) -> Storability {
    if !is_cacheable_method(method) {
        return Storability::Reject("method is not cacheable");
    }
    if body_len > opts.max_body_bytes {
        return Storability::Reject("body exceeds max_body_bytes");
    }

    let req_cc = CacheControl::from_headers(request_headers);
    if req_cc.no_store {
        return Storability::Reject("request no-store");
    }

    let cc = CacheControl::from_headers(&meta.headers);
    // `must-understand` overrides `no-store` for statuses whose caching rules we
    // do implement (RFC 9111 §5.2.2.3).
    let understood = is_heuristically_cacheable(meta.status);
    if cc.no_store && !(cc.must_understand && understood) {
        return Storability::Reject("response no-store");
    }
    if opts.shared && cc.private {
        return Storability::Reject("private response in a shared cache");
    }
    if opts.shared
        && request_headers.contains_key(http::header::AUTHORIZATION)
        && !(cc.public || cc.s_maxage.is_some() || cc.must_revalidate)
    {
        return Storability::Reject("authorized request without shared-cache permission");
    }
    if opts.shared
        && meta.headers.contains_key(http::header::SET_COOKIE)
        && !cc.public
    {
        return Storability::Reject("set-cookie in a shared cache");
    }

    let fields = vary::vary_fields(&meta.headers);
    if vary::varies_on_everything(&fields) {
        return Storability::Reject("Vary: *");
    }

    // A 304 carries no representation of its own; it updates one we hold.
    if meta.status == 304 {
        return Storability::Reject("a conditional response is folded in, not stored");
    }
    // A 206 is storable, but only when it says which bytes it is. Everything
    // that decides whether it can be *combined* with what we already hold is in
    // `Cache::store_partial`, which is the only place that can see the entry.
    if meta.status == 206 {
        let usable = meta
            .headers
            .get(http::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(crate::range::parse_content_range)
            .is_some();
        if !usable {
            return Storability::Reject("206 without a usable Content-Range");
        }
    }

    let (lifetime, heuristic) = freshness_lifetime(meta, opts);
    let explicit = !heuristic;
    if !is_heuristically_cacheable(meta.status) && !explicit {
        return Storability::Reject("status needs explicit freshness information");
    }
    // A response that is born stale is still worth storing when it can be
    // revalidated — that is where the conditional-request win comes from.
    if lifetime == 0
        && !meta.headers.contains_key(http::header::ETAG)
        && !meta.headers.contains_key(http::header::LAST_MODIFIED)
    {
        return Storability::Reject("no freshness lifetime and no validator");
    }

    Storability::Store
}

/// Can this stored entry answer this request, and under what conditions?
///
/// This answers "is the stored representation usable", not "which bytes of it".
/// A `Range` is deliberately not consulted: resolving one needs the stored
/// length, which this layer does not have and should not learn. `Cache::lookup`
/// decides that separately, and a range it cannot satisfy becomes a miss.
pub fn evaluate(
    request_headers: &HeaderMap,
    meta: &StoredMeta,
    now: u64,
    opts: &CacheOptions,
) -> Freshness {
    // A client running its own validation wants the origin's answer, not ours.
    // `If-Range` is not in this set: it qualifies a range rather than replacing
    // it, and the cache checks it against the stored validator itself.
    if request_headers.contains_key(http::header::IF_NONE_MATCH)
        || request_headers.contains_key(http::header::IF_MODIFIED_SINCE)
        || request_headers.contains_key(http::header::IF_MATCH)
        || request_headers.contains_key(http::header::IF_UNMODIFIED_SINCE)
    {
        return Freshness::Unusable("client-supplied conditional");
    }

    let req_cc = CacheControl::from_headers(request_headers);
    if req_cc.no_store {
        return Freshness::Unusable("request no-store");
    }

    let cc = CacheControl::from_headers(&meta.headers);
    let age = current_age(meta, now);
    let (lifetime, _) = freshness_lifetime(meta, opts);
    let stale_if_error = cc.stale_if_error;

    if req_cc.no_cache {
        return Freshness::Revalidate {
            age,
            lifetime,
            stale_if_error,
            reason: "request no-cache",
        };
    }
    // Unqualified `no-cache` on the response forces validation every time. The
    // qualified form only makes the named fields unusable, which we handle by
    // dropping those headers on serve.
    if cc.no_cache && cc.no_cache_fields.is_empty() {
        return Freshness::Revalidate {
            age,
            lifetime,
            stale_if_error,
            reason: "response no-cache",
        };
    }
    if let Some(max_age) = req_cc.max_age {
        if age > max_age {
            return Freshness::Revalidate {
                age,
                lifetime,
                stale_if_error,
                reason: "request max-age exceeded",
            };
        }
    }

    let min_fresh = req_cc.min_fresh.unwrap_or(0);
    if age.saturating_add(min_fresh) < lifetime {
        return Freshness::Fresh { age, lifetime };
    }
    if min_fresh > 0 && age < lifetime {
        return Freshness::Revalidate {
            age,
            lifetime,
            stale_if_error,
            reason: "request min-fresh not satisfied",
        };
    }

    let staleness = age.saturating_sub(lifetime);
    let pinned = cc.must_revalidate || (opts.shared && cc.proxy_revalidate);

    if !pinned {
        if let Some(max_stale) = req_cc.max_stale {
            let acceptable = max_stale.map(|s| staleness <= s).unwrap_or(true);
            if acceptable {
                return Freshness::ServeStale {
                    age,
                    lifetime,
                    refresh_in_background: false,
                    reason: "request max-stale",
                };
            }
        }
        if let Some(window) = cc.stale_while_revalidate {
            if staleness <= window {
                return Freshness::ServeStale {
                    age,
                    lifetime,
                    refresh_in_background: true,
                    reason: "stale-while-revalidate",
                };
            }
        }
    }

    Freshness::Revalidate {
        age,
        lifetime,
        stale_if_error: if pinned { None } else { stale_if_error },
        reason: if pinned { "must-revalidate" } else { "stale" },
    }
}

/// Headers to add to an outbound request so the origin can answer 304.
pub fn conditional_headers(meta: &StoredMeta) -> Vec<(http::HeaderName, String)> {
    let mut out = Vec::new();
    if let Some(etag) = meta.headers.get(http::header::ETAG).and_then(|v| v.to_str().ok()) {
        out.push((http::header::IF_NONE_MATCH, etag.to_string()));
    }
    if let Some(lm) = meta
        .headers
        .get(http::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
    {
        out.push((http::header::IF_MODIFIED_SINCE, lm.to_string()));
    }
    out
}

pub fn has_validator(meta: &StoredMeta) -> bool {
    meta.headers.contains_key(http::header::ETAG)
        || meta.headers.contains_key(http::header::LAST_MODIFIED)
}

/// Headers a 304 must not overwrite in the stored response (RFC 9111 §4.3.4).
const NOT_UPDATED_BY_304: &[&str] = &["content-length", "content-encoding", "content-range"];

/// Fold a 304's headers into the stored response.
pub fn apply_304(stored: &mut HeaderMap, fresh: &HeaderMap) {
    let tokens = crate::headers::connection_tokens(fresh);
    for (name, value) in fresh.iter() {
        let lname = name.as_str().to_ascii_lowercase();
        if crate::headers::is_hop_by_hop(&lname, &tokens) {
            continue;
        }
        if NOT_UPDATED_BY_304.contains(&lname.as_str()) {
            continue;
        }
        stored.remove(name);
        stored.append(name.clone(), value.clone());
    }
}

/// Headers that must be dropped when serving a response stored with a qualified
/// `no-cache="field"`.
pub fn suppressed_fields(meta: &StoredMeta) -> Vec<String> {
    let cc = CacheControl::from_headers(&meta.headers);
    let mut fields = cc.no_cache_fields;
    if !cc.private_fields.is_empty() {
        fields.extend(cc.private_fields);
    }
    fields
}
