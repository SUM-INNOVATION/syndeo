//! Secondary cache keys (RFC 9111 §4.1).
//!
//! A stored response records which request headers the origin said it varies on.
//! A new request selects that response only when those headers match.

use http::HeaderMap;

/// The header names a response varies on, normalised and sorted so the key is
/// stable regardless of the order the origin listed them in.
pub fn vary_fields(response_headers: &HeaderMap) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    for value in response_headers.get_all(http::header::VARY) {
        let Ok(s) = value.to_str() else { continue };
        for field in s.split(',') {
            let field = field.trim().to_ascii_lowercase();
            if field.is_empty() {
                continue;
            }
            if field == "*" {
                return vec!["*".to_string()];
            }
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    fields.sort();
    fields
}

/// `Vary: *` means no request can ever be a match, so such a response is not
/// storable at all.
pub fn varies_on_everything(fields: &[String]) -> bool {
    fields.iter().any(|f| f == "*")
}

/// Build the secondary key: the selecting header values from a request, in the
/// order of the (already sorted) vary field list.
pub fn vary_key(fields: &[String], request_headers: &HeaderMap) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let mut parts = Vec::with_capacity(fields.len());
    for field in fields {
        let joined = request_headers
            .get_all(field.as_str())
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("{field}={joined}"));
    }
    let digest = blake3::hash(parts.join("\u{1}").as_bytes());
    hex::encode(&digest.as_bytes()[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn field_order_does_not_change_the_key() {
        let a = vary_fields(&headers(&[("vary", "Accept-Encoding, Accept")]));
        let b = vary_fields(&headers(&[("vary", "accept"), ("vary", "accept-encoding")]));
        assert_eq!(a, b);
    }

    #[test]
    fn differing_selecting_headers_give_different_keys() {
        let fields = vary_fields(&headers(&[("vary", "Accept-Encoding")]));
        let gzip = vary_key(&fields, &headers(&[("accept-encoding", "gzip")]));
        let br = vary_key(&fields, &headers(&[("accept-encoding", "br")]));
        assert_ne!(gzip, br);
        assert_eq!(gzip, vary_key(&fields, &headers(&[("accept-encoding", "gzip")])));
    }

    #[test]
    fn missing_header_matches_missing_header() {
        let fields = vary_fields(&headers(&[("vary", "accept-language")]));
        assert_eq!(vary_key(&fields, &HeaderMap::new()), vary_key(&fields, &HeaderMap::new()));
        assert_ne!(
            vary_key(&fields, &HeaderMap::new()),
            vary_key(&fields, &headers(&[("accept-language", "en")]))
        );
    }

    #[test]
    fn star_is_detected() {
        assert!(varies_on_everything(&vary_fields(&headers(&[("vary", "*")]))));
        assert!(varies_on_everything(&vary_fields(&headers(&[(
            "vary",
            "accept, *"
        )]))));
    }
}
