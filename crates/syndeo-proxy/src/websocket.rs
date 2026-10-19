//! WebSockets through the proxy, `ws://` and `wss://`.
//!
//! A WebSocket opens with an HTTP/1.1 GET that asks to switch protocols, and
//! the headers that ask are hop-by-hop: `Upgrade`, and `Connection` naming it.
//! Every other request the proxy forwards loses them, which is right for every
//! other request and broke every WebSocket: in Syndeo.app (CI 39198465745) the
//! origin saw the handshake with both empty, and refused it.
//!
//! So a WebSocket handshake goes its own way. It is recognised strictly, and
//! one that names WebSocket without being a well-formed handshake is refused
//! before anything is sent anywhere. It goes on to its origin over a
//! connection of its own: the end-to-end headers, as for any request, and the
//! two this upgrade needs, said by the proxy rather than passed along, so no
//! other hop-by-hop header leaves. Once the origin has switched as RFC 6455
//! says it must — `Upgrade: websocket`, `Connection: Upgrade`, and the accept
//! value for this handshake's key — the two connections are joined, byte for
//! byte, until either closes. An origin that answers anything but 101 is
//! relayed as an ordinary answer. None of it goes near the cache: not the
//! handshake, not the answer, not what the connection then carries.

use crate::{append_our_via, forwardable, text, whole, Body, Proxy};
use base64::Engine;
use bytes::Bytes;
use http::header::{
    CONNECTION, CONTENT_LENGTH, HOST, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY,
    SEC_WEBSOCKET_VERSION, TRANSFER_ENCODING, UPGRADE,
};
use http::{HeaderMap, HeaderValue, Method, Version};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use syndeo_cache::headers::{connection_tokens, is_hop_by_hop};
use syndeo_cache::redact::Redacted;

/// RFC 6455's GUID, appended to a key to make the value that accepts it.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// How long reaching the origin, and its answer to the handshake, may take.
const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(30);

/// What a request's `Upgrade` asks of the proxy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Upgrading {
    /// Nothing that names WebSocket. The request goes the ordinary way, and
    /// any other `Upgrade` is dropped with the other hop-by-hop headers.
    No,
    /// A well-formed WebSocket opening handshake, with its key.
    WebSocket { key: String },
    /// An `Upgrade` naming WebSocket on a request that is not a well-formed
    /// opening handshake; why, for the 400 that refuses it.
    Refused(&'static str),
}

/// Whether a request is a WebSocket opening handshake: exactly one `Upgrade`,
/// saying `websocket` and nothing else, `Connection` naming `upgrade`, a GET
/// over HTTP/1.1 with no body, exactly one `Sec-WebSocket-Key` that is 16
/// bytes in base64, and exactly one `Sec-WebSocket-Version`, 13 (RFC 6455
/// section 4.1). Anything that names WebSocket and is short of that is
/// refused rather than guessed at.
pub(crate) fn upgrading(method: &Method, version: Version, headers: &HeaderMap) -> Upgrading {
    let upgrades: Vec<&HeaderValue> = headers.get_all(UPGRADE).iter().collect();
    let names_websocket = upgrades.iter().any(|value| {
        value.to_str().is_ok_and(|v| {
            v.split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("websocket"))
        })
    });
    if !names_websocket {
        return Upgrading::No;
    }
    let [upgrade] = upgrades.as_slice() else {
        return Upgrading::Refused("more than one Upgrade field");
    };
    if !upgrade
        .to_str()
        .is_ok_and(|v| v.trim().eq_ignore_ascii_case("websocket"))
    {
        return Upgrading::Refused("an Upgrade that names more than WebSocket");
    }
    if method != Method::GET {
        return Upgrading::Refused("a WebSocket handshake is a GET");
    }
    if version != Version::HTTP_11 {
        return Upgrading::Refused("a WebSocket handshake is HTTP/1.1");
    }
    if !connection_tokens(headers).iter().any(|t| t == "upgrade") {
        return Upgrading::Refused("a Connection that does not name Upgrade");
    }
    let keys: Vec<&HeaderValue> = headers.get_all(SEC_WEBSOCKET_KEY).iter().collect();
    let key = match keys.as_slice() {
        [one] => one.to_str().ok().map(str::trim).filter(|key| {
            base64::engine::general_purpose::STANDARD
                .decode(key)
                .is_ok_and(|bytes| bytes.len() == 16)
        }),
        _ => None,
    };
    let Some(key) = key else {
        return Upgrading::Refused("not exactly one Sec-WebSocket-Key of 16 bytes");
    };
    let versions: Vec<&HeaderValue> = headers.get_all(SEC_WEBSOCKET_VERSION).iter().collect();
    if !matches!(versions.as_slice(), [one] if one.to_str().is_ok_and(|v| v.trim() == "13")) {
        return Upgrading::Refused("not exactly Sec-WebSocket-Version 13");
    }
    if headers.contains_key(TRANSFER_ENCODING)
        || headers
            .get_all(CONTENT_LENGTH)
            .iter()
            .any(|v| v.to_str().map_or(true, |len| len.trim() != "0"))
    {
        return Upgrading::Refused("a WebSocket handshake with a body");
    }
    Upgrading::WebSocket {
        key: key.to_string(),
    }
}

/// The `Sec-WebSocket-Accept` that answers `key`.
pub(crate) fn accept(key: &str) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::new()
        .chain_update(key.as_bytes())
        .chain_update(GUID.as_bytes())
        .finalize();
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// Whether an origin's 101 is the switch this handshake asked for: exactly
/// one `Upgrade`, `websocket`; `Connection` naming `upgrade`; and exactly one
/// `Sec-WebSocket-Accept`, the one for `key`. A 101 short of that is not
/// joined, whatever the origin meant by it.
pub(crate) fn switched(headers: &HeaderMap, key: &str) -> Result<(), &'static str> {
    let upgrades: Vec<&HeaderValue> = headers.get_all(UPGRADE).iter().collect();
    if !matches!(upgrades.as_slice(), [one] if one.to_str().is_ok_and(|v| v.trim().eq_ignore_ascii_case("websocket")))
    {
        return Err("its 101 does not say Upgrade: websocket");
    }
    if !connection_tokens(headers).iter().any(|t| t == "upgrade") {
        return Err("its 101 does not say Connection: Upgrade");
    }
    let accepts: Vec<&HeaderValue> = headers.get_all(SEC_WEBSOCKET_ACCEPT).iter().collect();
    if !matches!(accepts.as_slice(), [one] if one.to_str().is_ok_and(|v| v.trim() == accept(key))) {
        return Err("its 101 does not accept this handshake's key");
    }
    Ok(())
}

/// 400, and the connection closed, for an `Upgrade` naming WebSocket on a
/// request that is not a handshake this proxy can carry.
pub(crate) fn refused(why: &str) -> Response<Body> {
    let mut response = text(
        StatusCode::BAD_REQUEST,
        &format!("not a WebSocket handshake this proxy can carry: {why}"),
    );
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

/// The TLS a WebSocket's own connection to its origin speaks: verified by the
/// operating system, as every other fetch is, and HTTP/1.1, the one protocol
/// a WebSocket opens over here.
pub(crate) fn client_config() -> anyhow::Result<Arc<rustls::ClientConfig>> {
    let mut config = (*syndeo_net::tls::client_config()?).clone();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// A connection a WebSocket can be carried over.
trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

/// A connection of its own to `host`, through the proxy's resolver, and over
/// TLS when `secure`.
async fn reach(proxy: &Proxy, host: &str, port: u16, secure: bool) -> Result<Box<dyn Io>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addresses = match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => proxy
            .dns
            .lookup(bare)
            .await
            .map_err(|err| format!("resolving {bare}: {err}"))?,
    };
    let mut last = format!("{bare} has no address");
    let mut stream = None;
    for ip in addresses {
        match tokio::net::TcpStream::connect((ip, port)).await {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(err) => last = format!("connecting to {bare}: {err}"),
        }
    }
    let stream = stream.ok_or(last)?;
    if !secure {
        return Ok(Box::new(stream));
    }
    let name = rustls::pki_types::ServerName::try_from(bare.to_string())
        .map_err(|err| format!("{bare}: {err}"))?;
    let tls = tokio_rustls::TlsConnector::from(proxy.upstream_tls.clone())
        .connect(name, stream)
        .await
        .map_err(|err| format!("TLS with {bare}: {err}"))?;
    Ok(Box::new(tls))
}

/// The headers of an origin's answer that go on to the client: its
/// end-to-end headers, and never a length the body will be framed by anew.
fn end_to_end(headers: &HeaderMap) -> HeaderMap {
    let tokens = connection_tokens(headers);
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if is_hop_by_hop(name.as_str(), &tokens) || name == CONTENT_LENGTH {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Carry the WebSocket `req` opens, to `url`, with handshake key `key`.
pub(crate) async fn open(
    proxy: Arc<Proxy>,
    mut req: Request<Incoming>,
    url: String,
    key: String,
) -> Response<Body> {
    let Ok(uri) = url.parse::<hyper::Uri>() else {
        return text(
            StatusCode::BAD_REQUEST,
            "a WebSocket address that cannot be read",
        );
    };
    let secure = match uri.scheme_str().map(str::to_ascii_lowercase).as_deref() {
        Some("https" | "wss") => true,
        Some("http" | "ws") => false,
        _ => {
            return text(
                StatusCode::BAD_REQUEST,
                "a WebSocket address under a scheme this proxy does not carry",
            )
        }
    };
    let (Some(host), Some(authority)) = (uri.host(), uri.authority()) else {
        return text(StatusCode::BAD_REQUEST, "a WebSocket address with no host");
    };
    let Ok(host_header) = HeaderValue::from_str(authority.as_str()) else {
        return text(StatusCode::BAD_REQUEST, "a WebSocket address with no host");
    };
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let target = uri.path_and_query().map_or("/", |p| p.as_str()).to_string();

    // On to the origin: the end-to-end headers, our Via, and the two this
    // upgrade needs, said by us rather than passed along.
    let mut headers = forwardable(req.headers());
    append_our_via(&mut headers);
    headers.insert(HOST, host_header);
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));

    // The client's side of the connection, once our 101 has gone out to it.
    let client = hyper::upgrade::on(&mut req);
    drop(req);

    let answered = tokio::time::timeout(HANDSHAKE, async {
        let io = reach(&proxy, host, port, secure).await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(io))
                .await
                .map_err(|err| format!("{err}"))?;
        tokio::spawn(async move {
            if let Err(err) = connection.with_upgrades().await {
                tracing::debug!(%err, "a WebSocket's connection to its origin ended");
            }
        });
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(target)
            .version(Version::HTTP_11)
            .body(Empty::new())
            .map_err(|err| format!("{err}"))?;
        *request.headers_mut() = headers;
        sender
            .send_request(request)
            .await
            .map_err(|err| format!("{err}"))
    })
    .await;
    let mut response = match answered {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            tracing::warn!(url = %Redacted(&url), %err, "a WebSocket could not reach its origin");
            return text(
                StatusCode::BAD_GATEWAY,
                &format!("this WebSocket's origin could not be reached: {err}"),
            );
        }
        Err(_) => {
            tracing::warn!(url = %Redacted(&url), "a WebSocket's origin did not answer in time");
            return text(
                StatusCode::GATEWAY_TIMEOUT,
                "this WebSocket's origin did not answer in time",
            );
        }
    };

    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        // Not switched: an ordinary answer, relayed as one, and kept nowhere.
        let (parts, body) = response.into_parts();
        let mut builder = Response::builder().status(parts.status);
        *builder.headers_mut().expect("builder is valid") = end_to_end(&parts.headers);
        return builder
            .body(body.map_err(std::io::Error::other).boxed_unsync())
            .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "malformed response"));
    }
    if let Err(why) = switched(response.headers(), &key) {
        tracing::warn!(url = %Redacted(&url), why, "an origin's switch to WebSocket did not answer the handshake");
        return text(
            StatusCode::BAD_GATEWAY,
            &format!("this WebSocket's origin did not complete the handshake: {why}"),
        );
    }

    let mut builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    {
        let out = builder.headers_mut().expect("builder is valid");
        *out = end_to_end(response.headers());
        out.insert(UPGRADE, HeaderValue::from_static("websocket"));
        out.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    }
    let origin = hyper::upgrade::on(&mut response);
    let shown = Redacted(&url).to_string();
    tokio::spawn(async move {
        match tokio::join!(client, origin) {
            (Ok(client), Ok(origin)) => {
                let (mut client, mut origin) = (TokioIo::new(client), TokioIo::new(origin));
                match tokio::io::copy_bidirectional(&mut client, &mut origin).await {
                    Ok((up, down)) => {
                        tracing::debug!(at = %shown, up, down, "a WebSocket closed");
                    }
                    Err(err) => tracing::debug!(at = %shown, %err, "a WebSocket ended"),
                }
            }
            (Err(err), _) | (_, Err(err)) => {
                tracing::debug!(at = %shown, %err, "a WebSocket could not be joined");
            }
        }
    });
    builder
        .body(whole(Bytes::new()))
        .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "malformed response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6455 section 1.3's own example handshake.
    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    fn headers(fields: &[(&str, &str)]) -> HeaderMap {
        let mut out = HeaderMap::new();
        for (name, value) in fields {
            out.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        out
    }

    fn handshake(changed: &[(&str, Option<&str>)]) -> HeaderMap {
        let mut fields = vec![
            ("upgrade", "websocket"),
            ("connection", "Upgrade"),
            ("sec-websocket-key", KEY),
            ("sec-websocket-version", "13"),
        ];
        for (name, value) in changed {
            fields.retain(|(n, _)| n != name);
            if let Some(value) = value {
                fields.push((name, value));
            }
        }
        headers(&fields)
    }

    #[test]
    fn websocket_accept_is_rfc_6455s_own_example() {
        assert_eq!(accept(KEY), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn websocket_a_well_formed_handshake_is_one_whatever_the_case() {
        let want = Upgrading::WebSocket {
            key: KEY.to_string(),
        };
        assert_eq!(
            upgrading(&Method::GET, Version::HTTP_11, &handshake(&[])),
            want
        );
        let shouted = handshake(&[
            ("upgrade", Some("WebSocket")),
            ("connection", Some("keep-alive, UPGRADE")),
        ]);
        assert_eq!(upgrading(&Method::GET, Version::HTTP_11, &shouted), want);
    }

    #[test]
    fn websocket_nothing_naming_it_is_not_one() {
        for fields in [
            headers(&[("accept", "text/html")]),
            headers(&[("upgrade", "h2c"), ("connection", "Upgrade")]),
        ] {
            assert_eq!(
                upgrading(&Method::GET, Version::HTTP_11, &fields),
                Upgrading::No
            );
        }
    }

    #[test]
    fn websocket_a_handshake_short_of_well_formed_is_refused() {
        let two_upgrades = {
            let mut h = handshake(&[]);
            h.append(UPGRADE, "websocket".parse().unwrap());
            h
        };
        let two_keys = {
            let mut h = handshake(&[]);
            h.append(SEC_WEBSOCKET_KEY, KEY.parse().unwrap());
            h
        };
        let two_versions = {
            let mut h = handshake(&[]);
            h.append(SEC_WEBSOCKET_VERSION, "13".parse().unwrap());
            h
        };
        for (what, method, version, fields) in [
            (
                "two Upgrade fields",
                Method::GET,
                Version::HTTP_11,
                two_upgrades,
            ),
            (
                "an Upgrade naming another protocol too",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("upgrade", Some("websocket, h2c"))]),
            ),
            ("a POST", Method::POST, Version::HTTP_11, handshake(&[])),
            ("HTTP/1.0", Method::GET, Version::HTTP_10, handshake(&[])),
            (
                "a Connection not naming Upgrade",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("connection", Some("keep-alive"))]),
            ),
            (
                "no key",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("sec-websocket-key", None)]),
            ),
            (
                "a key of 5 bytes",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("sec-websocket-key", Some("c2hvcnQ="))]),
            ),
            (
                "a key that is not base64",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("sec-websocket-key", Some("not base64 at all!!!!!!"))]),
            ),
            ("two keys", Method::GET, Version::HTTP_11, two_keys),
            (
                "no version",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("sec-websocket-version", None)]),
            ),
            (
                "version 8",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("sec-websocket-version", Some("8"))]),
            ),
            ("two versions", Method::GET, Version::HTTP_11, two_versions),
            (
                "a body by length",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("content-length", Some("5"))]),
            ),
            (
                "a body by chunks",
                Method::GET,
                Version::HTTP_11,
                handshake(&[("transfer-encoding", Some("chunked"))]),
            ),
        ] {
            assert!(
                matches!(upgrading(&method, version, &fields), Upgrading::Refused(_)),
                "{what} was not refused"
            );
        }
        // A declared length of nothing is no body.
        assert!(matches!(
            upgrading(
                &Method::GET,
                Version::HTTP_11,
                &handshake(&[("content-length", Some("0"))])
            ),
            Upgrading::WebSocket { .. }
        ));
    }

    fn answer(fields: &[(&str, &str)]) -> HeaderMap {
        headers(fields)
    }

    #[test]
    fn websocket_only_a_switch_that_accepts_this_key_is_joined() {
        let good = accept(KEY);
        assert_eq!(
            switched(
                &answer(&[
                    ("upgrade", "websocket"),
                    ("connection", "Upgrade"),
                    ("sec-websocket-accept", &good)
                ]),
                KEY
            ),
            Ok(())
        );
        for (what, fields) in [
            (
                "no Upgrade",
                answer(&[("connection", "Upgrade"), ("sec-websocket-accept", &good)]),
            ),
            (
                "another protocol",
                answer(&[
                    ("upgrade", "h2c"),
                    ("connection", "Upgrade"),
                    ("sec-websocket-accept", &good),
                ]),
            ),
            (
                "no Connection: Upgrade",
                answer(&[("upgrade", "websocket"), ("sec-websocket-accept", &good)]),
            ),
            (
                "no accept",
                answer(&[("upgrade", "websocket"), ("connection", "Upgrade")]),
            ),
            (
                "another key's accept",
                answer(&[
                    ("upgrade", "websocket"),
                    ("connection", "Upgrade"),
                    ("sec-websocket-accept", &accept("AAAAAAAAAAAAAAAAAAAAAA==")),
                ]),
            ),
            (
                "two accepts",
                answer(&[
                    ("upgrade", "websocket"),
                    ("connection", "Upgrade"),
                    ("sec-websocket-accept", &good),
                    ("sec-websocket-accept", &good),
                ]),
            ),
        ] {
            assert!(switched(&fields, KEY).is_err(), "{what} was joined");
        }
    }
}
