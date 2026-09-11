//! Translating between Servo's resource loads and [`NetRequest`].
//!
//! Kept apart from the Servo glue and free of any dependency on it, so the part
//! that decides *what* crosses the boundary can be read and tested without an
//! hour of compilation in the way. The Servo-shaped half is in
//! [`crate::delegate`] and does nothing but call into this.

use http::{HeaderMap, Method};
use syndeo_ipc::protocol::{Fetched, NetRequest};

/// Headers a renderer must not be allowed to dictate.
///
/// The connection-level ones because HTTP/2 and HTTP/3 reject them outright and
/// the net process may use either; `host` because the URL decides the host and a
/// second opinion is a request-smuggling primitive rather than a feature.
const NOT_FORWARDED: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "host",
    "content-length",
];

/// Turn a resource load into the only thing the renderer is allowed to ask for.
///
/// Note what cannot survive the translation, because the type has nowhere to put
/// it: a socket, an address, a certificate, a DNS answer, a proxy. A URL goes
/// in, bytes come back. That is boundary one, and this function is where it is
/// enforced against a renderer rather than asserted about one.
pub fn to_net_request(method: &Method, url: &url::Url, headers: &HeaderMap) -> NetRequest {
    NetRequest::Fetch {
        method: method.as_str().to_string(),
        url: url.as_str().to_string(),
        headers: forwardable(headers),
        // A renderer's request body would arrive here too; Servo does not hand
        // one to the embedder, so there is none to forward.
        body: Vec::new(),
        // Servo does not tell the embedder what a page declared as a
        // subresource's integrity, so nothing loaded through here is eligible
        // for peer fetch yet. Filed rather than papered over: the net process
        // will simply never ask a peer for these.
        integrity: None,
    }
}

fn forwardable(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| !NOT_FORWARDED.contains(&name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

/// The headers to hand back to the renderer.
///
/// Hop-by-hop headers are dropped again on the way in: what the net process
/// answered with may have come from the cache, from a peer, or from an origin
/// over any of three protocols, and the renderer should not be able to tell
/// which from the headers it sees.
pub fn response_headers(fetched: &Fetched) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in &fetched.headers {
        if NOT_FORWARDED.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            http::HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            out.append(name, value);
        }
    }
    // The renderer is told the length it is actually going to receive, which is
    // not necessarily the one the origin claimed — a range served from the store
    // has its own.
    if let Ok(value) = http::HeaderValue::from_str(&fetched.body.len().to_string()) {
        out.insert(http::header::CONTENT_LENGTH, value);
    }
    out
}

/// What the shell shows about a load that has been answered from the store.
pub fn provenance(fetched: &Fetched) -> String {
    match fetched.protocol.as_str() {
        "-" => fetched.source.clone(),
        protocol => format!("{} over {}", fetched.source, protocol),
    }
}

/// Whether a URL's bytes are already in the renderer's hands.
///
/// `data:` is the payload itself, `about:` is Servo's own pages, `blob:` and
/// `filesystem:` are memory it is already holding. None of them is reachable
/// over a socket, so none of them is the network process's business — and
/// handing them to it produces a cancelled load and a missing image.
///
/// Compared on the parsed scheme rather than on a prefix of the text: the URL
/// parser has already lowercased it and stripped the whitespace that a prefix
/// match would have to guess at.
pub fn is_self_contained_scheme(url: &url::Url) -> bool {
    const SELF_CONTAINED: &[&str] = &["data", "about", "blob", "filesystem", "javascript"];
    SELF_CONTAINED.contains(&url.scheme())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut out = HeaderMap::new();
        for (name, value) in pairs {
            out.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        out
    }

    fn fetched(headers: &[(&str, &str)], body: &[u8]) -> Fetched {
        Fetched {
            status: 200,
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            body: body.to_vec(),
            source: "cache".into(),
            protocol: "-".into(),
            elapsed_ms: 1,
            content: None,
        }
    }

    #[test]
    fn a_resource_load_becomes_a_url_and_nothing_else() {
        let url = url::Url::parse("https://example.test/app.js").unwrap();
        let request = to_net_request(
            &Method::GET,
            &url,
            &headers(&[("accept", "*/*"), ("referer", "https://example.test/")]),
        );

        let NetRequest::Fetch {
            method,
            url: asked,
            headers,
            body,
            ..
        } = request
        else {
            panic!("a resource load must become a fetch");
        };
        assert_eq!(method, "GET");
        assert_eq!(asked, "https://example.test/app.js");
        assert!(body.is_empty());
        assert!(headers.iter().any(|(n, _)| n == "accept"));
        assert!(headers.iter().any(|(n, _)| n == "referer"));
    }

    #[test]
    fn a_renderer_cannot_dictate_the_host_or_the_connection() {
        // The two that matter: `host` disagreeing with the URL is a
        // request-smuggling primitive, and a connection header would be rejected
        // outright by HTTP/2 and HTTP/3, either of which the net process may
        // choose without telling anyone.
        let url = url::Url::parse("https://example.test/").unwrap();
        let request = to_net_request(
            &Method::GET,
            &url,
            &headers(&[
                ("host", "somewhere.else"),
                ("connection", "upgrade"),
                ("upgrade", "websocket"),
                ("transfer-encoding", "chunked"),
                ("content-length", "9999"),
                ("cookie", "session=abc"),
            ]),
        );
        let NetRequest::Fetch { headers, .. } = request else {
            unreachable!()
        };
        let names: Vec<&str> = headers.iter().map(|(n, _)| n.as_str()).collect();
        for refused in [
            "host",
            "connection",
            "upgrade",
            "transfer-encoding",
            "content-length",
        ] {
            assert!(
                !names.contains(&refused),
                "{refused} reached the net process"
            );
        }
        // Cookies are the site's own business and do travel.
        assert!(names.contains(&"cookie"));
    }

    #[test]
    fn the_renderer_cannot_tell_where_the_bytes_came_from() {
        let from_cache = fetched(
            &[("content-type", "text/html"), ("connection", "keep-alive")],
            b"<html></html>",
        );
        let headers = response_headers(&from_cache);
        assert_eq!(headers["content-type"], "text/html");
        assert!(
            !headers.contains_key("connection"),
            "a hop-by-hop header reached the renderer"
        );
        assert_eq!(headers["content-length"], "13");
    }

    #[test]
    fn a_length_the_origin_claimed_does_not_override_the_one_being_sent() {
        // A range out of the store is shorter than the origin's Content-Length,
        // and a renderer told otherwise waits for bytes that are not coming.
        let ranged = fetched(&[("content-length", "100000")], b"0123456789");
        assert_eq!(response_headers(&ranged)["content-length"], "10");
    }

    #[test]
    fn provenance_names_the_protocol_only_when_one_carried_it() {
        let mut from_origin = fetched(&[], b"x");
        from_origin.source = "origin".into();
        from_origin.protocol = "h3".into();
        assert_eq!(provenance(&from_origin), "origin over h3");

        let from_store = fetched(&[], b"x");
        assert_eq!(provenance(&from_store), "cache");
    }

    #[test]
    fn bytes_the_renderer_already_has_are_not_the_network_process_business() {
        let parse = |s: &str| url::Url::parse(s).unwrap();

        // The exact shape that was cancelling every inline icon on rust-lang.org.
        assert!(is_self_contained_scheme(&parse(
            "data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg'/>"
        )));
        assert!(is_self_contained_scheme(&parse(
            "DATA:text/plain;base64,aGk="
        )));
        assert!(is_self_contained_scheme(&parse("about:blank")));
        assert!(is_self_contained_scheme(&parse(
            "blob:https://example.test/abc"
        )));

        // Everything that can reach a socket goes to the network process, and a
        // scheme nobody has thought about is on that side of the line too.
        assert!(!is_self_contained_scheme(&parse("https://example.test/")));
        assert!(!is_self_contained_scheme(&parse("http://example.test/")));
        assert!(!is_self_contained_scheme(&parse(
            "wss://example.test/socket"
        )));
        assert!(!is_self_contained_scheme(&parse("ftp://example.test/file")));
    }
}
