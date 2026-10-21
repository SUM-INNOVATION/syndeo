//! Header parsing that RFC 9111 depends on: `Cache-Control`, HTTP dates, and the
//! hop-by-hop header set that must never be stored.

use http::{HeaderMap, HeaderName};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parsed `Cache-Control` field value. One type serves both request and response
/// directives; the irrelevant half is simply left at its default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheControl {
    pub no_store: bool,
    pub no_cache: bool,
    /// `no-cache="field, field"` — qualified form, only those fields are unusable.
    pub no_cache_fields: Vec<String>,
    pub no_transform: bool,
    pub only_if_cached: bool,
    pub must_revalidate: bool,
    pub proxy_revalidate: bool,
    pub must_understand: bool,
    pub immutable: bool,
    pub public: bool,
    pub private: bool,
    pub private_fields: Vec<String>,
    pub max_age: Option<u64>,
    pub s_maxage: Option<u64>,
    /// `max-stale` with no value means "any staleness is acceptable".
    pub max_stale: Option<Option<u64>>,
    pub min_fresh: Option<u64>,
    pub stale_while_revalidate: Option<u64>,
    pub stale_if_error: Option<u64>,
}

impl CacheControl {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut cc = CacheControl::default();
        let mut seen = false;
        for value in headers.get_all(http::header::CACHE_CONTROL) {
            if let Ok(s) = value.to_str() {
                seen = true;
                cc.absorb(s);
            }
        }
        // RFC 9111 §5.3: a `Pragma: no-cache` request header is honoured only when
        // no Cache-Control is present at all.
        if !seen {
            for value in headers.get_all(http::header::PRAGMA) {
                if let Ok(s) = value.to_str() {
                    if s.split(',')
                        .any(|d| d.trim().eq_ignore_ascii_case("no-cache"))
                    {
                        cc.no_cache = true;
                    }
                }
            }
        }
        cc
    }

    pub fn parse(value: &str) -> Self {
        let mut cc = CacheControl::default();
        cc.absorb(value);
        cc
    }

    fn absorb(&mut self, value: &str) {
        for directive in split_directives(value) {
            let (name, arg) = match directive.split_once('=') {
                Some((n, v)) => (n.trim(), Some(unquote(v.trim()))),
                None => (directive.trim(), None),
            };
            let lname = name.to_ascii_lowercase();
            let secs = || arg.as_deref().and_then(|v| v.trim().parse::<u64>().ok());
            let fields = || {
                arg.as_deref()
                    .map(|v| {
                        v.split(',')
                            .map(|f| f.trim().to_ascii_lowercase())
                            .filter(|f| !f.is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            match lname.as_str() {
                "no-store" => self.no_store = true,
                "no-cache" => {
                    self.no_cache = true;
                    self.no_cache_fields.extend(fields());
                }
                "no-transform" => self.no_transform = true,
                "only-if-cached" => self.only_if_cached = true,
                "must-revalidate" => self.must_revalidate = true,
                "proxy-revalidate" => self.proxy_revalidate = true,
                "must-understand" => self.must_understand = true,
                "immutable" => self.immutable = true,
                "public" => self.public = true,
                "private" => {
                    self.private = true;
                    self.private_fields.extend(fields());
                }
                // A malformed delta-seconds is treated as if the directive were
                // absent, except that `max-age` with garbage must not be read as
                // "fresh forever" — callers see `None` and fall through to Expires.
                "max-age" => self.max_age = secs(),
                "s-maxage" => self.s_maxage = secs(),
                "max-stale" => self.max_stale = Some(secs()),
                "min-fresh" => self.min_fresh = secs(),
                "stale-while-revalidate" => self.stale_while_revalidate = secs(),
                "stale-if-error" => self.stale_if_error = secs(),
                _ => {}
            }
        }
    }

    /// True when the response carries any directive that makes it explicitly
    /// cacheable, which is what lifts non-heuristically-cacheable statuses.
    pub fn has_explicit_expiry(&self) -> bool {
        self.max_age.is_some() || self.s_maxage.is_some() || self.public
    }
}

/// Split on commas that are not inside a quoted-string.
fn split_directives(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                cur.push(ch);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                cur.push(ch);
            }
            ',' if !in_quotes => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].replace("\\\"", "\"")
    } else {
        v.to_string()
    }
}

/// Seconds since the Unix epoch for an HTTP-date header, if it parses.
pub fn header_date(headers: &HeaderMap, name: HeaderName) -> Option<u64> {
    let raw = headers.get(name)?.to_str().ok()?;
    parse_http_date(raw)
}

pub fn parse_http_date(raw: &str) -> Option<u64> {
    let t = httpdate::parse_http_date(raw.trim()).ok()?;
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

pub fn format_http_date(secs: u64) -> String {
    httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(secs))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Headers that describe the connection rather than the message, plus the ones
/// named by a `Connection` header. None of these may be stored or forwarded.
pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub fn is_hop_by_hop(name: &str, connection_tokens: &[String]) -> bool {
    let lname = name.to_ascii_lowercase();
    HOP_BY_HOP.contains(&lname.as_str()) || connection_tokens.contains(&lname)
}

pub fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    let mut out = Vec::new();
    for v in headers.get_all(http::header::CONNECTION) {
        if let Ok(s) = v.to_str() {
            out.extend(
                s.split(',')
                    .map(|t| t.trim().to_ascii_lowercase())
                    .filter(|t| !t.is_empty()),
            );
        }
    }
    out
}

/// Strip hop-by-hop headers so what we store is a real end-to-end message.
pub fn sanitize(headers: &HeaderMap) -> Vec<(String, String)> {
    let tokens = connection_tokens(headers);
    headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name.as_str(), &tokens))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Response fields this cache never keeps and never replays.
///
/// Syndeo's own privacy rule, not something RFC 9111 asks for. A cookie is
/// addressed to the one client whose request produced it, at the moment it was
/// produced. Replayed from a store it reaches whoever asks next — every client
/// of a shared proxy — or comes back after the user has logged out and the
/// site has cleared it. So a response's cookies go to the client that caused
/// the fetch, and are left out of everything written to disk.
pub const NEVER_STORED: &[&str] = &["set-cookie", "set-cookie2"];

pub fn is_never_stored(name: &str) -> bool {
    NEVER_STORED
        .iter()
        .any(|never| name.eq_ignore_ascii_case(never))
}

/// Remove every [`NEVER_STORED`] field from a header map.
pub fn strip_never_stored(headers: &mut HeaderMap) {
    for name in NEVER_STORED {
        headers.remove(*name);
    }
}

/// The headers of a stored response, as they are written to the index.
///
/// The only constructor is [`StoredHeaders::for_storage`], which drops the
/// hop-by-hop fields and the [`NEVER_STORED`] ones. That makes the storage
/// boundary one function rather than a rule every store path has to remember.
///
/// Encoded exactly as the `Vec` it wraps, so an index written before this type
/// existed reads back unchanged. Such an index can hold a `Set-Cookie`, which
/// is why serving strips the same fields again.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct StoredHeaders(Vec<(String, String)>);

impl StoredHeaders {
    pub fn for_storage(headers: &HeaderMap) -> Self {
        StoredHeaders(
            sanitize(headers)
                .into_iter()
                .filter(|(name, _)| !is_never_stored(name))
                .collect(),
        )
    }

    pub fn pairs(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn to_header_map(&self) -> HeaderMap {
        crate::cache::to_header_map(&self.0)
    }
}

/// Whether a client whose request said `accept` (its `Accept-Encoding`, every
/// field joined, or None without one) can take a body encoded as `content`
/// (the stored response's `Content-Encoding`, likewise): RFC 9110 §12.5.3.
///
/// A request without `Accept-Encoding` takes any coding, and the identity
/// coding is always taken. Otherwise each coding the body is in has to be
/// named, or covered by `*`, with a weight above zero; `x-gzip` is `gzip`.
pub fn coding_acceptable(content: Option<&str>, accept: Option<&str>) -> bool {
    let Some(accept) = accept else {
        return true;
    };
    let weights: Vec<(String, bool)> = accept
        .split(',')
        .filter_map(|item| {
            let mut parts = item.split(';');
            let name = coding_name(parts.next()?)?;
            let wanted = parts
                .filter_map(|param| param.split_once('='))
                .find(|(key, _)| key.trim().eq_ignore_ascii_case("q"))
                .map(|(_, q)| q.trim().parse::<f32>().map(|q| q > 0.0).unwrap_or(false))
                .unwrap_or(true);
            Some((name, wanted))
        })
        .collect();
    let named = |coding: &str| {
        weights
            .iter()
            .find(|(name, _)| name == coding)
            .map(|(_, w)| *w)
    };
    content
        .unwrap_or("")
        .split(',')
        .filter_map(coding_name)
        .filter(|coding| coding != "identity")
        .all(|coding| named(&coding).or_else(|| named("*")).unwrap_or(false))
}

/// A content coding's name as compared: trimmed, lower case, `x-gzip` as
/// `gzip` and `x-compress` as `compress`; None for an empty one.
fn coding_name(raw: &str) -> Option<String> {
    let name = raw.trim().to_ascii_lowercase();
    match name.as_str() {
        "" => None,
        "x-gzip" => Some("gzip".to_string()),
        "x-compress" => Some("compress".to_string()),
        _ => Some(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coding_is_acceptable_when_the_request_names_it_or_says_nothing() {
        // No Accept-Encoding: anything (RFC 9110 §12.5.3).
        assert!(coding_acceptable(Some("br"), None));
        // Identity, or no coding at all, always.
        assert!(coding_acceptable(None, Some("gzip")));
        assert!(coding_acceptable(Some("identity"), Some("gzip")));
        // Named, with any weight above zero, in any case and spacing.
        assert!(coding_acceptable(Some("br"), Some("gzip, deflate, br")));
        assert!(coding_acceptable(Some("BR"), Some(" Gzip ,br;q=0.5")));
        assert!(coding_acceptable(Some("gzip"), Some("x-gzip")));
        assert!(coding_acceptable(Some("x-gzip"), Some("gzip")));
        // Covered by *.
        assert!(coding_acceptable(Some("zstd"), Some("*")));
        // Every coding of a body in more than one.
        assert!(coding_acceptable(Some("gzip, br"), Some("br, gzip")));

        // Not named, or refused.
        assert!(!coding_acceptable(Some("br"), Some("gzip")));
        assert!(!coding_acceptable(Some("br"), Some("identity")));
        assert!(!coding_acceptable(Some("br"), Some("")));
        assert!(!coding_acceptable(Some("br"), Some("gzip, br;q=0")));
        assert!(!coding_acceptable(Some("br"), Some("br;q=0.0")));
        assert!(!coding_acceptable(Some("br"), Some("*, br;q=0")));
        assert!(!coding_acceptable(Some("zstd"), Some("*;q=0")));
        assert!(!coding_acceptable(Some("gzip, br"), Some("gzip")));
        // A weight that does not parse is no weight above zero.
        assert!(!coding_acceptable(Some("br"), Some("br;q=lots")));
    }

    #[test]
    fn parses_common_directives() {
        let cc = CacheControl::parse("max-age=600, public, stale-while-revalidate=30");
        assert_eq!(cc.max_age, Some(600));
        assert!(cc.public);
        assert_eq!(cc.stale_while_revalidate, Some(30));
    }

    #[test]
    fn quoted_field_lists_do_not_split_on_inner_commas() {
        let cc = CacheControl::parse(r#"no-cache="set-cookie, x-token", max-age=5"#);
        assert!(cc.no_cache);
        assert_eq!(cc.no_cache_fields, vec!["set-cookie", "x-token"]);
        assert_eq!(cc.max_age, Some(5));
    }

    #[test]
    fn max_stale_without_value_means_unbounded() {
        assert_eq!(CacheControl::parse("max-stale").max_stale, Some(None));
        assert_eq!(
            CacheControl::parse("max-stale=10").max_stale,
            Some(Some(10))
        );
    }

    #[test]
    fn garbage_delta_seconds_is_ignored_not_treated_as_forever() {
        assert_eq!(CacheControl::parse("max-age=oops").max_age, None);
    }

    #[test]
    fn pragma_only_applies_without_cache_control() {
        let mut h = HeaderMap::new();
        h.insert(http::header::PRAGMA, "no-cache".parse().unwrap());
        assert!(CacheControl::from_headers(&h).no_cache);

        h.insert(http::header::CACHE_CONTROL, "max-age=100".parse().unwrap());
        let cc = CacheControl::from_headers(&h);
        assert!(!cc.no_cache);
        assert_eq!(cc.max_age, Some(100));
    }

    #[test]
    fn connection_named_headers_are_hop_by_hop() {
        let mut h = HeaderMap::new();
        h.insert(
            http::header::CONNECTION,
            "X-Custom, keep-alive".parse().unwrap(),
        );
        h.insert("x-custom", "1".parse().unwrap());
        h.insert("x-kept", "1".parse().unwrap());
        let kept = sanitize(&h);
        assert!(kept.iter().any(|(n, _)| n == "x-kept"));
        assert!(!kept.iter().any(|(n, _)| n == "x-custom"));
        assert!(!kept.iter().any(|(n, _)| n == "connection"));
    }

    #[test]
    fn cookies_never_reach_storage() {
        let mut h = HeaderMap::new();
        h.append("set-cookie", "a=1".parse().unwrap());
        h.append("Set-Cookie2", "b=2".parse().unwrap());
        h.append("content-type", "text/plain".parse().unwrap());
        let stored = StoredHeaders::for_storage(&h);
        assert_eq!(
            stored.pairs(),
            &[("content-type".to_string(), "text/plain".to_string())]
        );
    }

    #[test]
    fn stored_headers_encode_exactly_as_the_list_they_replace() {
        // An index written before `StoredHeaders` holds a bare list. Reading it
        // back must not need a schema change, and must see every field that
        // was written, cookies included: those are removed on serve.
        let legacy: Vec<(String, String)> = vec![
            ("set-cookie".into(), "old=1".into()),
            ("etag".into(), "\"v1\"".into()),
        ];
        let bytes = bincode::serialize(&legacy).unwrap();
        let read: StoredHeaders = bincode::deserialize(&bytes).unwrap();
        assert_eq!(read.pairs(), legacy.as_slice());
        assert_eq!(bincode::serialize(&read).unwrap(), bytes);
    }
}
