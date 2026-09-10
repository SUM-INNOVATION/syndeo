//! HTTP/3 over QUIC.
//!
//! Layer two of the architecture calls for `quinn` plus `h3`, and this is it.
//! Nothing above [`Net::fetch`] learns which protocol carried a response — that
//! is the same boundary that lets the cache be swapped or fed from a peer — so
//! the only visible difference is [`crate::fetch::Protocol`] on the response,
//! which is reporting rather than control.
//!
//! **When it is used.** Never speculatively. An origin has to have said it
//! speaks HTTP/3, in an `Alt-Svc` header on an earlier HTTP/1.1 or HTTP/2
//! response, before a QUIC packet is sent to it. That is what `Alt-Svc` is for,
//! and racing UDP against TCP on the chance it works would cost every
//! HTTP/3-less origin a timeout.
//!
//! **When it fails.** A failed QUIC attempt falls back to TCP rather than
//! failing the fetch — UDP is blocked on a great many networks, and a browser
//! that could not load a page because of it would be broken. The authority is
//! then put in a cooldown so the next request does not pay the same timeout.
//!
//! [`Net::fetch`]: crate::fetch::Net::fetch

use crate::dns::Dns;
use crate::error::{NetError, Result};
use bytes::{Buf, Bytes};
use futures::stream::{BoxStream, StreamExt};
use h3::client::SendRequest;
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long an `Alt-Svc` advertisement is trusted when it carries no `ma`.
const DEFAULT_ALT_SVC_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// How long an authority is left alone after a QUIC attempt failed.
///
/// Long enough that a network which blocks UDP costs one timeout rather than one
/// per request; short enough that moving to a network which does not is noticed
/// within a browsing session.
const FAILURE_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// What we know about one origin's willingness to speak HTTP/3.
#[derive(Debug, Clone, Copy)]
enum Advertisement {
    /// It said so, and the advertisement is good until this instant.
    Available { until: Instant },
    /// We tried and it did not work. Not before this instant.
    Failed { until: Instant },
}

/// What an origin has told us, and what happened when we believed it.
#[derive(Default)]
pub struct AltSvc {
    known: Mutex<HashMap<String, Advertisement>>,
}

impl AltSvc {
    /// Record what an `Alt-Svc` header said. Anything that does not name `h3` is
    /// ignored: this is not a general alternative-service implementation, and
    /// pretending otherwise would mean honouring advertisements we cannot use.
    pub fn observe(&self, authority: &str, headers: &HeaderMap) {
        let Some(value) = headers.get("alt-svc").and_then(|v| v.to_str().ok()) else {
            return;
        };
        // `clear` withdraws every advertisement for this origin.
        if value.trim().eq_ignore_ascii_case("clear") {
            self.known.lock().expect("alt-svc map").remove(authority);
            return;
        }
        let Some(lifetime) = parse_h3_advertisement(value) else {
            return;
        };
        self.known.lock().expect("alt-svc map").insert(
            authority.to_string(),
            Advertisement::Available {
                until: Instant::now() + lifetime,
            },
        );
    }

    /// Should this request go over QUIC?
    pub fn should_try(&self, authority: &str) -> bool {
        let mut known = self.known.lock().expect("alt-svc map");
        match known.get(authority) {
            Some(Advertisement::Available { until }) if *until > Instant::now() => true,
            Some(Advertisement::Failed { until }) if *until > Instant::now() => false,
            // Expired, either way.
            Some(_) => {
                known.remove(authority);
                false
            }
            None => false,
        }
    }

    /// The attempt did not work. Stop trying for a while.
    pub fn failed(&self, authority: &str) {
        self.known.lock().expect("alt-svc map").insert(
            authority.to_string(),
            Advertisement::Failed {
                until: Instant::now() + FAILURE_COOLDOWN,
            },
        );
    }
}

/// The lifetime of an `h3` advertisement in an `Alt-Svc` field value, if there
/// is one. `None` means the field advertises no `h3` alternative.
fn parse_h3_advertisement(value: &str) -> Option<Duration> {
    for alternative in value.split(',') {
        let mut parts = alternative.split(';');
        let Some(endpoint) = parts.next() else {
            continue;
        };
        let Some((protocol, _)) = endpoint.trim().split_once('=') else {
            continue;
        };
        // `h3` is the standard identifier; the draft ones (`h3-29` and friends)
        // are not interoperable with a final-RFC client and are not accepted.
        if protocol.trim() != "h3" {
            continue;
        }
        let mut lifetime = DEFAULT_ALT_SVC_LIFETIME;
        for parameter in parts {
            if let Some((name, seconds)) = parameter.trim().split_once('=') {
                if name.trim() == "ma" {
                    if let Ok(secs) = seconds.trim().trim_matches('"').parse::<u64>() {
                        lifetime = Duration::from_secs(secs);
                    }
                }
            }
        }
        return Some(lifetime);
    }
    None
}

type Sender = SendRequest<h3_quinn::OpenStreams, Bytes>;

/// A QUIC endpoint and the HTTP/3 connections open on it.
pub struct QuicClient {
    endpoint: quinn::Endpoint,
    dns: Dns,
    /// One connection per authority. RFC 9114 §3.3 asks clients not to open more
    /// than one to a given endpoint, and reusing it is also what makes the
    /// second request to an origin cheap.
    connections: tokio::sync::Mutex<HashMap<String, Sender>>,
}

impl QuicClient {
    /// Build a QUIC endpoint from the same verification policy the TCP client
    /// uses. The only difference is ALPN, which HTTP/3 requires to be `h3`.
    pub fn new(dns: Dns, tls: Arc<rustls::ClientConfig>) -> Result<Self> {
        let mut tls = (*tls).clone();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        // QUIC needs the TLS session tickets and key updates rustls only enables
        // for it explicitly.
        tls.enable_early_data = false;

        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| NetError::Tls(format!("this TLS configuration cannot carry QUIC: {e}")))?;
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(Duration::from_secs(15)));
        transport.max_idle_timeout(Some(
            Duration::from_secs(30)
                .try_into()
                .expect("thirty seconds is a representable idle timeout"),
        ));
        config.transport_config(Arc::new(transport));

        // An unspecified local address, so the operating system picks the port.
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().expect("a valid address"))
            .map_err(|e| NetError::Transport(format!("could not open a QUIC endpoint: {e}")))?;
        endpoint.set_default_client_config(config);

        Ok(QuicClient {
            endpoint,
            dns,
            connections: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// One request over HTTP/3. Same shape as the TCP path, so the caller does
    /// not branch on which one answered.
    pub async fn request(
        &self,
        uri: &Uri,
        method: &http::Method,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<(u16, HeaderMap, BoxStream<'static, Result<Bytes>>)> {
        let authority = uri
            .authority()
            .map(|a| a.to_string())
            .ok_or_else(|| NetError::InvalidUrl(uri.to_string()))?;

        let mut sender = self.sender_for(uri, &authority).await?;

        let mut builder = http::Request::builder().method(method.clone()).uri(uri);
        {
            let out = builder.headers_mut().expect("builder is valid");
            for (name, value) in headers.iter() {
                // HTTP/3 has no connection-level headers and rejects them.
                if syndeo_cache::headers::is_hop_by_hop(name.as_str(), &[]) {
                    continue;
                }
                if name == http::header::HOST {
                    continue;
                }
                out.append(name.clone(), value.clone());
            }
        }
        let request = builder.body(()).map_err(NetError::Http)?;

        let mut stream = sender
            .send_request(request)
            .await
            .map_err(|e| NetError::Transport(format!("h3 request: {e}")))?;
        if !body.is_empty() {
            stream
                .send_data(body)
                .await
                .map_err(|e| NetError::Transport(format!("h3 body: {e}")))?;
        }
        stream
            .finish()
            .await
            .map_err(|e| NetError::Transport(format!("h3 finish: {e}")))?;

        let response = stream
            .recv_response()
            .await
            .map_err(|e| NetError::Transport(format!("h3 response: {e}")))?;
        let status = response.status().as_u16();
        let mut response_headers = response.headers().clone();
        // HTTP/3 carries no reason phrase and no `Connection`, but it does carry
        // trailers; those are not part of the representation we cache.
        response_headers.remove(http::header::TRANSFER_ENCODING);

        let body = futures::stream::unfold(Some(stream), |stream| async move {
            let mut stream = stream?;
            match stream.recv_data().await {
                Ok(Some(mut chunk)) => {
                    let bytes = chunk.copy_to_bytes(chunk.remaining());
                    Some((Ok(bytes), Some(stream)))
                }
                Ok(None) => None,
                Err(err) => Some((
                    Err(crate::body::truncated(format!("h3: {err}"))),
                    None,
                )),
            }
        })
        .boxed();

        Ok((status, response_headers, body))
    }

    /// An open connection to this authority, or a new one.
    async fn sender_for(&self, uri: &Uri, authority: &str) -> Result<Sender> {
        {
            let open = self.connections.lock().await;
            if let Some(sender) = open.get(authority) {
                return Ok(sender.clone());
            }
        }

        let address = self.resolve(uri).await?;
        let host = uri.host().ok_or_else(|| NetError::InvalidUrl(uri.to_string()))?;
        let connecting = self
            .endpoint
            .connect(address, host)
            .map_err(|e| NetError::Transport(format!("quic connect: {e}")))?;
        let connection = connecting
            .await
            .map_err(|e| NetError::Transport(format!("quic handshake: {e}")))?;

        let (driver, sender) = h3::client::new(h3_quinn::Connection::new(connection))
            .await
            .map_err(|e| NetError::Transport(format!("h3 handshake: {e}")))?;

        // The connection has to be driven for anything on it to progress. It
        // ends when the peer closes it, and takes its entry with it.
        let authority_owned = authority.to_string();
        tokio::spawn(async move {
            let mut driver = driver;
            let outcome = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
            tracing::debug!(authority = %authority_owned, ?outcome, "h3 connection closed");
        });

        let mut open = self.connections.lock().await;
        Ok(open.entry(authority.to_string()).or_insert(sender).clone())
    }

    /// Forget a connection that has stopped working, so the next request opens
    /// a fresh one rather than failing on a dead handle.
    pub async fn forget(&self, authority: &str) {
        self.connections.lock().await.remove(authority);
    }

    async fn resolve(&self, uri: &Uri) -> Result<SocketAddr> {
        let host = uri.host().ok_or_else(|| NetError::InvalidUrl(uri.to_string()))?;
        let port = uri.port_u16().unwrap_or(443);
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return Ok(SocketAddr::new(ip, port));
        }
        // The same resolver the TCP path uses, so DNS policy does not depend on
        // which transport happens to be chosen.
        let addresses = self.dns.lookup(host).await?;
        addresses
            .into_iter()
            .next()
            .map(|ip| SocketAddr::new(ip, port))
            .ok_or_else(|| NetError::Dns(format!("no address for {host}")))
    }
}

/// Rebuild a header map from pairs, used when a caller supplies extras.
pub fn with_extra(headers: &HeaderMap, extra: &[(HeaderName, String)]) -> HeaderMap {
    let mut out = headers.clone();
    for (name, value) in extra {
        if let Ok(v) = HeaderValue::from_str(value) {
            out.insert(name.clone(), v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alt_svc_field_is_read_only_for_final_h3() {
        assert_eq!(
            parse_h3_advertisement("h3=\":443\"; ma=86400"),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_h3_advertisement("h3=\":443\""),
            Some(DEFAULT_ALT_SVC_LIFETIME)
        );
        assert_eq!(
            parse_h3_advertisement("h2=\"alt.example:443\", h3=\":443\"; ma=3600"),
            Some(Duration::from_secs(3600))
        );

        // Draft versions are not interoperable with a final-RFC client.
        assert_eq!(parse_h3_advertisement("h3-29=\":443\"; ma=86400"), None);
        assert_eq!(parse_h3_advertisement("h2=\":443\""), None);
        assert_eq!(parse_h3_advertisement(""), None);
    }

    #[test]
    fn quic_is_only_tried_where_an_origin_said_it_would_work() {
        let alt = AltSvc::default();
        assert!(
            !alt.should_try("example.test"),
            "QUIC must never be tried speculatively"
        );

        let mut headers = HeaderMap::new();
        headers.insert("alt-svc", "h3=\":443\"; ma=3600".parse().unwrap());
        alt.observe("example.test", &headers);
        assert!(alt.should_try("example.test"));

        // And `clear` withdraws it.
        let mut cleared = HeaderMap::new();
        cleared.insert("alt-svc", "clear".parse().unwrap());
        alt.observe("example.test", &cleared);
        assert!(!alt.should_try("example.test"));
    }

    #[test]
    fn a_failed_attempt_stops_the_next_request_paying_for_it_too() {
        let alt = AltSvc::default();
        let mut headers = HeaderMap::new();
        headers.insert("alt-svc", "h3=\":443\"".parse().unwrap());
        alt.observe("blocked.test", &headers);
        assert!(alt.should_try("blocked.test"));

        alt.failed("blocked.test");
        assert!(
            !alt.should_try("blocked.test"),
            "a network that blocks UDP should cost one timeout, not one per request"
        );
    }

    #[test]
    fn an_expired_advertisement_is_forgotten() {
        let alt = AltSvc::default();
        let mut headers = HeaderMap::new();
        headers.insert("alt-svc", "h3=\":443\"; ma=0".parse().unwrap());
        alt.observe("brief.test", &headers);
        std::thread::sleep(Duration::from_millis(5));
        assert!(!alt.should_try("brief.test"));
    }
}
