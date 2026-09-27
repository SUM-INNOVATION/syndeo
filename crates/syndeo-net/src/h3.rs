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

/// The longest an advertisement is believed, whatever `ma` says.
///
/// `ma` is whatever number the response carried. Unbounded, a value such as
/// 18446744073709551615 made the deadline overflow, and the panic that caused
/// happened with the map locked, which poisoned it for every HTTPS request
/// after. Thirty days is longer than any real deployment asks for.
const MAX_ALT_SVC_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

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
    /// The map, even if something panicked while holding it. Every entry is a
    /// hint that is written whole, so one left by a panic is still a hint, and
    /// a poisoned lock must not take every later request down with it.
    fn known(&self) -> std::sync::MutexGuard<'_, HashMap<String, Advertisement>> {
        self.known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record what an `Alt-Svc` header said. Anything that does not name `h3` is
    /// ignored: this is not a general alternative-service implementation, and
    /// pretending otherwise would mean honouring advertisements we cannot use.
    pub fn observe(&self, authority: &str, headers: &HeaderMap) {
        let Some(value) = headers.get("alt-svc").and_then(|v| v.to_str().ok()) else {
            return;
        };
        // `clear` withdraws every advertisement for this origin.
        if value.trim().eq_ignore_ascii_case("clear") {
            self.known().remove(authority);
            return;
        }
        let Some(lifetime) = parse_h3_advertisement(value) else {
            return;
        };
        // Computed before the lock is taken, and checked, so nothing that can
        // go wrong here happens while the map is held.
        let Some(until) = Instant::now().checked_add(lifetime) else {
            return;
        };
        self.known()
            .insert(authority.to_string(), Advertisement::Available { until });
    }

    /// Should this request go over QUIC?
    pub fn should_try(&self, authority: &str) -> bool {
        let mut known = self.known();
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
        let now = Instant::now();
        let until = now.checked_add(FAILURE_COOLDOWN).unwrap_or(now);
        self.known()
            .insert(authority.to_string(), Advertisement::Failed { until });
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
                        lifetime = Duration::from_secs(secs).min(MAX_ALT_SVC_LIFETIME);
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
        let mut endpoint =
            quinn::Endpoint::client("0.0.0.0:0".parse().expect("a valid address"))
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
    ///
    /// A failure says how far it got, because that decides whether the same
    /// request may be sent again over TCP: see [`Failed`].
    pub async fn request(
        &self,
        uri: &Uri,
        method: &http::Method,
        headers: &HeaderMap,
        body: Bytes,
    ) -> std::result::Result<(u16, HeaderMap, BoxStream<'static, Result<Bytes>>), Failed> {
        let nothing_sent = |error| Failed {
            reached: Reached::Nothing,
            error,
        };
        let request_sent = |error| Failed {
            reached: Reached::Request,
            error,
        };

        let authority = uri
            .authority()
            .map(|a| a.to_string())
            .ok_or_else(|| nothing_sent(NetError::InvalidUrl(uri.to_string())))?;

        let mut sender = self
            .sender_for(uri, &authority)
            .await
            .map_err(nothing_sent)?;

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
        let request = builder
            .body(())
            .map_err(|e| nothing_sent(NetError::Http(e)))?;

        // From here on the origin may have received some or all of the
        // request, even if what follows fails.
        let mut stream = sender
            .send_request(request)
            .await
            .map_err(|e| request_sent(NetError::Transport(format!("h3 request: {e}"))))?;
        if !body.is_empty() {
            stream
                .send_data(body)
                .await
                .map_err(|e| request_sent(NetError::Transport(format!("h3 body: {e}"))))?;
        }
        stream
            .finish()
            .await
            .map_err(|e| request_sent(NetError::Transport(format!("h3 finish: {e}"))))?;

        let response = stream
            .recv_response()
            .await
            .map_err(|e| request_sent(NetError::Transport(format!("h3 response: {e}"))))?;
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
                Err(err) => Some((Err(crate::body::truncated(format!("h3: {err}"))), None)),
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
        let host = uri
            .host()
            .ok_or_else(|| NetError::InvalidUrl(uri.to_string()))?;
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
        let host = uri
            .host()
            .ok_or_else(|| NetError::InvalidUrl(uri.to_string()))?;
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

/// How far a failed HTTP/3 attempt got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reached {
    /// Nothing reached the origin: resolving, connecting or the handshake
    /// failed. Sending the request another way cannot repeat it.
    Nothing,
    /// The request was being sent, or had been. The origin may have acted on
    /// it, so sending it again can make it happen twice.
    Request,
}

#[derive(Debug)]
pub struct Failed {
    pub reached: Reached,
    pub error: NetError,
}

/// Whether a request whose HTTP/3 attempt failed may be sent again over TCP.
///
/// Always when nothing reached the origin. Otherwise only for a method RFC
/// 9110 §9.2.2 calls idempotent, for which a repeat is harmless by definition;
/// a POST that may already have been received is not sent a second time.
pub fn may_retry_over_tcp(method: &http::Method, reached: Reached) -> bool {
    use http::Method;
    reached == Reached::Nothing
        || [
            Method::GET,
            Method::HEAD,
            Method::OPTIONS,
            Method::TRACE,
            Method::PUT,
            Method::DELETE,
        ]
        .contains(method)
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

/// QUIC endpoints for tests, on loopback, and a client that trusts them.
#[cfg(test)]
pub(crate) mod test_quic {
    use super::*;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        (certified.cert.der().clone(), key.into())
    }

    /// An endpoint that turns every connection away before anything is sent.
    pub(crate) async fn refusing_endpoint() -> SocketAddr {
        crate::tls::install_crypto_provider();
        let (cert, key) = self_signed();
        let config = quinn::ServerConfig::with_single_cert(vec![cert], key).unwrap();
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                incoming.refuse();
            }
        });
        address
    }

    /// An HTTP/3 origin that reads each request whole — headers and body — and
    /// then drops the connection without answering. Counts what it received.
    pub(crate) async fn origin_that_drops_after_the_request(
    ) -> (SocketAddr, Arc<AtomicUsize>, CertificateDer<'static>) {
        crate::tls::install_crypto_provider();
        let (cert, key) = self_signed();
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
        let config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let received = Arc::new(AtomicUsize::new(0));
        let seen = received.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let seen = seen.clone();
                tokio::spawn(async move {
                    let Ok(connection) = incoming.await else {
                        return;
                    };
                    let raw = connection.clone();
                    let Ok(mut h3) = h3::server::Connection::<_, Bytes>::new(
                        h3_quinn::Connection::new(connection),
                    )
                    .await
                    else {
                        return;
                    };
                    if let Ok(Some(resolver)) = h3.accept().await {
                        if let Ok((_request, mut stream)) = resolver.resolve_request().await {
                            while let Ok(Some(_)) = stream.recv_data().await {}
                            seen.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    raw.close(0u32.into(), b"gone");
                });
            }
        });
        (address, received, cert)
    }

    /// A QUIC client that trusts `cert`, and nothing else.
    pub(crate) fn client_trusting(cert: CertificateDer<'static>) -> QuicClient {
        crate::tls::install_crypto_provider();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        QuicClient::new(
            crate::dns::Dns::new(&crate::dns::DnsMode::System).unwrap(),
            Arc::new(tls),
        )
        .unwrap()
    }
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

    fn advertise(alt: &AltSvc, authority: &str, value: &str) {
        let mut headers = HeaderMap::new();
        headers.insert("alt-svc", value.parse().unwrap());
        alt.observe(authority, &headers);
    }

    #[test]
    fn a_huge_max_age_is_clamped_rather_than_overflowing() {
        assert_eq!(
            parse_h3_advertisement("h3=\":443\"; ma=18446744073709551615"),
            Some(MAX_ALT_SVC_LIFETIME)
        );

        let alt = AltSvc::default();
        advertise(&alt, "huge.test", "h3=\":443\"; ma=18446744073709551615");
        assert!(alt.should_try("huge.test"));

        // The map is still whole: other origins, failures and `clear` all work.
        advertise(&alt, "next.test", "h3=\":443\"; ma=60");
        assert!(alt.should_try("next.test"));
        assert!(!alt.should_try("never-advertised.test"));
        alt.failed("next.test");
        assert!(!alt.should_try("next.test"));
        advertise(&alt, "huge.test", "clear");
        assert!(!alt.should_try("huge.test"));
    }

    #[test]
    fn a_panic_while_the_map_was_held_does_not_poison_every_request_after() {
        let alt = Arc::new(AltSvc::default());
        advertise(&alt, "before.test", "h3=\":443\"");
        let holder = alt.clone();
        let panicked = std::thread::spawn(move || {
            let _held = holder.known.lock().unwrap();
            panic!("something went wrong with the map held");
        })
        .join();
        assert!(panicked.is_err());
        assert!(alt.known.is_poisoned());

        assert!(alt.should_try("before.test"));
        advertise(&alt, "after.test", "h3=\":443\"; ma=60");
        assert!(alt.should_try("after.test"));
        alt.failed("after.test");
        assert!(!alt.should_try("after.test"));
        advertise(&alt, "before.test", "clear");
        assert!(!alt.should_try("before.test"));
    }

    #[test]
    fn a_request_that_may_have_been_received_is_repeated_only_if_idempotent() {
        use http::Method;
        let idempotent = [
            Method::GET,
            Method::HEAD,
            Method::OPTIONS,
            Method::TRACE,
            Method::PUT,
            Method::DELETE,
        ];
        let not_idempotent = [
            Method::POST,
            Method::PATCH,
            Method::CONNECT,
            Method::from_bytes(b"PURGE").unwrap(),
        ];
        for method in idempotent.iter().chain(not_idempotent.iter()) {
            assert!(
                may_retry_over_tcp(method, Reached::Nothing),
                "{method}: nothing was sent, so nothing can be repeated"
            );
        }
        for method in &idempotent {
            assert!(may_retry_over_tcp(method, Reached::Request), "{method}");
        }
        for method in &not_idempotent {
            assert!(
                !may_retry_over_tcp(method, Reached::Request),
                "{method} would be sent twice"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_connection_is_reported_as_nothing_having_been_sent() {
        crate::tls::install_crypto_provider();
        let address = test_quic::refusing_endpoint().await;
        let client = QuicClient::new(
            crate::dns::Dns::new(&crate::dns::DnsMode::System).unwrap(),
            crate::tls::client_config().unwrap(),
        )
        .unwrap();
        let uri: Uri = format!("https://127.0.0.1:{}/submit", address.port())
            .parse()
            .unwrap();

        let failed = tokio::time::timeout(
            Duration::from_secs(10),
            client.request(
                &uri,
                &http::Method::POST,
                &HeaderMap::new(),
                Bytes::from_static(b"order=1"),
            ),
        )
        .await
        .expect("a refused connection should fail promptly")
        .err()
        .expect("a refused connection cannot succeed");
        assert_eq!(failed.reached, Reached::Nothing, "{}", failed.error);
        assert!(may_retry_over_tcp(&http::Method::POST, failed.reached));
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
