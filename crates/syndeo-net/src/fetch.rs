//! The fetch API.
//!
//! Everything above this line — renderer, agent, shell — asks for a URL and gets
//! bytes. None of them open a socket, resolve a name, or see a certificate. That
//! boundary is the reason the cache can be swapped, shared, or fed from a peer
//! without anything upstream noticing.

use crate::body::{FetchBody, Tee};
use crate::config::NetConfig;
use crate::dns::Dns;
use crate::error::{NetError, Result};
use crate::h3::{AltSvc, QuicClient};
use crate::tls;
use bytes::Bytes;
use futures::stream::StreamExt;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri};
use http_body_util::Full;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use syndeo_cache::headers::now_secs;
use syndeo_cache::{Cache, CacheOptions, Lookup, Provenance, StoreOutcome};

/// Where the bytes actually came from. The proxy reports this per request, and
/// it is the whole point of the measurement step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Source {
    /// Served from cache, fresh.
    Cache,
    /// Served from cache while stale, permitted by policy.
    CacheStale,
    /// Origin confirmed the stored body with a 304; no body crossed the wire.
    Revalidated,
    /// Origin sent a body.
    Origin,
    /// Origin was unreachable and `stale-if-error` covered it.
    StaleOnError,
    /// A peer supplied the body, and it hashed to what the page declared.
    Peer,
    /// Not cacheable; passed straight through.
    PassThrough,
}

/// Which protocol carried a response.
///
/// Reporting, not control. `Source` says whether the bytes had to cross the
/// network at all, which is the question the cache exists to answer; this says
/// how they crossed it when they did, which is a different question and belongs
/// in a different field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Protocol {
    /// Served from the store; nothing carried it.
    None,
    Http1,
    Http2,
    Http3,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::None => "-",
            Protocol::Http1 => "http/1.1",
            Protocol::Http2 => "h2",
            Protocol::Http3 => "h3",
        }
    }

    fn from_version(version: http::Version) -> Self {
        match version {
            http::Version::HTTP_2 => Protocol::Http2,
            http::Version::HTTP_3 => Protocol::Http3,
            _ => Protocol::Http1,
        }
    }
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Cache => "cache",
            Source::CacheStale => "cache-stale",
            Source::Revalidated => "revalidated",
            Source::Origin => "origin",
            Source::StaleOnError => "stale-on-error",
            Source::Peer => "peer",
            Source::PassThrough => "pass-through",
        }
    }

    /// True when no body had to cross the network.
    pub fn avoided_transfer(self) -> bool {
        matches!(
            self,
            Source::Cache
                | Source::CacheStale
                | Source::Revalidated
                | Source::StaleOnError
                | Source::Peer
        )
    }
}

#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// What the page says this resource's bytes must hash to. Without it, a peer
    /// is never asked, and the origin is the only source.
    pub integrity: Option<syndeo_cache::Integrity>,
    /// The top-level document's origin, which is the cache partition. See
    /// `syndeo_cache::index::primary_key` for what it buys and what it costs.
    pub partition: Option<String>,
    /// Whether a redirect is followed here or handed back to the caller.
    pub redirect: RedirectMode,
}

/// Who follows a redirect: this process, or whoever asked.
///
/// Following is right for a caller that only wants the resource — the shell,
/// the agent, the renderers — and is the default. It is wrong for a proxy. A
/// browser behind a proxy has to see the redirect itself: it is what moves the
/// address bar, what decides which origin the final page runs as, and what
/// carries any cookie the redirecting response set. A proxy that followed on
/// the browser's behalf would hand back the destination's page as the answer
/// to the first URL, so a script from the destination would run as the site
/// that redirected to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RedirectMode {
    /// Follow redirects here, up to `NetConfig::max_redirects`.
    #[default]
    Follow,
    /// Return the first response as it is, redirect or not: status, every
    /// header and the body.
    Manual,
}

impl FetchRequest {
    pub fn get(url: impl Into<String>) -> Self {
        FetchRequest {
            method: Method::GET,
            url: url.into(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            partition: None,
            integrity: None,
            redirect: RedirectMode::Follow,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            self.headers.append(n, v);
        }
        self
    }
}

#[derive(Debug)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: HeaderMap,
    /// The body, which may not have arrived yet. Nothing above this line has to
    /// wait for the last byte before it can act on the first.
    pub body: FetchBody,
    pub source: Source,
    pub elapsed_ms: u64,
    /// The BLAKE3 content address, when the body passed through the cache and
    /// was complete before this response was built. A streamed body does not
    /// have one yet: it is not addressable until its last byte has arrived.
    pub content: Option<syndeo_cache::ContentId>,
    /// Where the response actually came from, after redirects.
    pub final_url: String,
    /// How many redirects were followed to get here.
    pub redirects: u8,
    /// Which protocol carried it, when one did.
    pub protocol: Protocol,
}

type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector<Dns>>, Full<Bytes>>;

/// The network process, as a library. The binary is a thin wrapper around this.
pub struct Net {
    cache: Arc<Cache>,
    client: HttpsClient,
    config: NetConfig,
    peers: Option<syndeo_peer::PeerHandle>,
    /// The QUIC endpoint, when one could be opened. A machine with no usable
    /// UDP socket is not a broken browser; it is a browser that speaks TCP.
    quic: Option<Arc<QuicClient>>,
    /// Which origins have said they speak HTTP/3, and which have disappointed us.
    alt_svc: Arc<AltSvc>,
    /// Entry keys with a background revalidation already in flight.
    ///
    /// A popular entry going stale is exactly when many requests arrive at once,
    /// and one refresh per request would turn `stale-while-revalidate` from a
    /// saving into a stampede. They collapse onto the first.
    refreshing: Arc<Mutex<HashSet<String>>>,
    /// Hashes handed to the swarm to announce, over this process's life.
    announcements: Arc<std::sync::atomic::AtomicU64>,
}

impl Net {
    pub fn new(config: NetConfig) -> Result<Self> {
        tls::install_crypto_provider();

        let cache = Arc::new(Cache::with_options(
            &config.cache_root,
            CacheOptions {
                shared: config.shared_cache,
                max_body_bytes: config.max_body_bytes,
                ..CacheOptions::default()
            },
        )?);

        let dns = Dns::new(&config.dns)?;
        let mut http = HttpConnector::new_with_resolver(dns);
        http.enforce_http(false);
        http.set_nodelay(true);
        http.set_connect_timeout(Some(std::time::Duration::from_secs(15)));

        let https = HttpsConnectorBuilder::new()
            .with_tls_config((*tls::client_config()?).clone())
            .https_or_http()
            .enable_all_versions()
            .wrap_connector(http);

        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build(https);

        let peers = match &config.peers {
            Some(peer_config) => {
                match syndeo_peer::PeerNode::start(cache.clone(), peer_config.clone()) {
                    Ok(handle) => {
                        tracing::info!(peer = %handle.peer_id(), "joined the peer swarm");
                        Some(handle)
                    }
                    Err(err) => {
                        tracing::warn!(%err, "could not join the peer swarm; continuing without it");
                        None
                    }
                }
            }
            None => None,
        };

        let quic = match QuicClient::new(Dns::new(&config.dns)?, tls::client_config()?) {
            Ok(client) => Some(Arc::new(client)),
            Err(err) => {
                tracing::info!(%err, "no QUIC endpoint; HTTP/3 is unavailable on this host");
                None
            }
        };

        Ok(Net {
            cache,
            client,
            config,
            peers,
            quic,
            alt_svc: Arc::new(AltSvc::default()),
            refreshing: Arc::new(Mutex::new(HashSet::new())),
            announcements: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    pub fn peer_id(&self) -> Option<syndeo_peer::swarm::PeerId> {
        self.peers.as_ref().map(|p| p.peer_id())
    }

    pub fn cache(&self) -> &Arc<Cache> {
        &self.cache
    }

    pub fn config(&self) -> &NetConfig {
        &self.config
    }

    /// Fetch a URL, following redirects and consulting the cache at every hop.
    ///
    /// Each hop is a cache lookup in its own right, so a permanent redirect is
    /// answered from the store on the second visit and the destination is too.
    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse> {
        refuse_unusable_integrity(&request)?;
        let mut current = request;
        let mut redirects = 0u8;
        let mut visited = vec![Cache::normalize_url(&current.url)];

        loop {
            let mut response = self.fetch_once(current.clone()).await?;
            response.final_url = current.url.clone();
            response.redirects = redirects;

            if current.redirect == RedirectMode::Manual {
                return Ok(response);
            }

            let Some(location) = redirect_target(&response) else {
                return Ok(response);
            };
            // A redirect's own body is not wanted by anyone. Dropping the stream
            // closes it rather than reading it to the end.
            drop(std::mem::replace(&mut response.body, FetchBody::empty()));
            if redirects >= self.config.max_redirects {
                return Err(NetError::TooManyRedirects {
                    limit: self.config.max_redirects,
                });
            }

            let next = match url::Url::parse(&current.url).and_then(|base| base.join(&location)) {
                Ok(u) => u,
                Err(_) => return final_redirect(&current, response),
            };
            // Only http and https; a redirect is not a way to reach another
            // scheme's handler.
            if !matches!(next.scheme(), "http" | "https") {
                return final_redirect(&current, response);
            }
            let normalized = Cache::normalize_url(next.as_str());
            if visited.contains(&normalized) {
                return Err(NetError::RedirectLoop(normalized));
            }
            visited.push(normalized);

            // 303, and 301/302 in practice, turn anything into a GET and drop
            // the body. 307 and 308 preserve both.
            let preserve = matches!(response.status, 307 | 308);
            current = FetchRequest {
                // The partition follows the document that started the chain,
                // not whatever host it was bounced through.
                partition: current.partition.clone(),
                method: if preserve {
                    current.method.clone()
                } else {
                    Method::GET
                },
                url: next.to_string(),
                headers: forwardable_headers(&current.headers, &current.url, next.as_str()),
                body: if preserve {
                    current.body.clone()
                } else {
                    Bytes::new()
                },
                // Integrity was declared for the resource, not for a redirect
                // hop, and it still describes whatever finally answers.
                integrity: current.integrity.clone(),
                redirect: current.redirect,
            };
            redirects += 1;
        }
    }

    async fn fetch_once(&self, request: FetchRequest) -> Result<FetchResponse> {
        let started = Instant::now();
        let method = request.method.as_str().to_string();

        // An unsafe method invalidates whatever we hold for the target.
        if syndeo_cache::policy::invalidates(&method) {
            let _ = self
                .cache
                .invalidate(request.partition.as_deref(), &method, &request.url);
            let response = self.origin(&request, &[]).await?;
            return Ok(self.finish(
                response.0,
                response.1,
                response.2,
                Source::PassThrough,
                None,
                started,
            ));
        }

        match self.cache.lookup(
            request.partition.as_deref(),
            &method,
            &request.url,
            &request.headers,
        )? {
            Lookup::Fresh(stored) => {
                if !self.stored_satisfies(&request, &stored) {
                    return self.fetch_from_origin(&request, &[], started).await;
                }
                Ok(self.finish(
                    stored.status,
                    stored.headers.clone(),
                    Bytes::from(stored.body.clone()),
                    Source::Cache,
                    stored.content,
                    started,
                ))
            }

            Lookup::Stale {
                response,
                refresh_in_background,
                ..
            } => {
                if !self.stored_satisfies(&request, &response) {
                    return self.fetch_from_origin(&request, &[], started).await;
                }
                if refresh_in_background {
                    // The whole point of the directive: move the revalidation
                    // off this request's critical path, having already answered
                    // it from the store.
                    let conditional = syndeo_cache::policy::conditional_headers(&response.meta);
                    self.spawn_refresh(response.key.clone(), request.clone(), conditional);
                }
                Ok(self.finish(
                    response.status,
                    response.headers.clone(),
                    Bytes::from(response.body.clone()),
                    Source::CacheStale,
                    response.content,
                    started,
                ))
            }

            Lookup::Revalidate {
                response,
                conditional,
                stale_if_error,
                ..
            } => {
                // Asking whether bytes that do not satisfy the declaration are
                // still current would be a wasted round trip; ask for the
                // representation instead, once.
                if !self.stored_satisfies(&request, &response) {
                    return self.fetch_from_origin(&request, &[], started).await;
                }
                let attempt = self.origin(&request, &conditional).await;
                match attempt {
                    Ok((304, headers, _)) => {
                        let now = now_secs();
                        match self
                            .cache
                            .record_not_modified(&response.key, &headers, now, now)
                        {
                            Ok(Some(refreshed)) => {
                                if !self.stored_satisfies(&request, &refreshed) {
                                    return self.fetch_from_origin(&request, &[], started).await;
                                }
                                Ok(self.finish(
                                    refreshed.status,
                                    with_cookies_of(refreshed.headers.clone(), &headers),
                                    Bytes::from(refreshed.body.clone()),
                                    Source::Revalidated,
                                    refreshed.content,
                                    started,
                                ))
                            }
                            Ok(None) => Ok(self.finish(
                                response.status,
                                with_cookies_of(response.headers.clone(), &headers),
                                Bytes::from(response.body.clone()),
                                Source::CacheStale,
                                response.content,
                                started,
                            )),
                            // The origin confirmed a body the store no longer
                            // holds; the cache has dropped the entry.
                            Err(err) if err.is_lost_body() => {
                                tracing::warn!(url = %request.url, %err, "the confirmed body was lost; fetching it again");
                                self.refetch_lost(&request, started).await
                            }
                            Err(err) => Err(err.into()),
                        }
                    }
                    Ok((status, headers, body)) => match &request.integrity {
                        Some(integrity) => self.accept_declared(
                            &request,
                            integrity,
                            status,
                            headers,
                            body,
                            started,
                            Protocol::None,
                        ),
                        None => {
                            let content = self.store(&request, status, &headers, &body, started);
                            Ok(
                                self.finish(
                                    status,
                                    headers,
                                    body,
                                    Source::Origin,
                                    content,
                                    started,
                                ),
                            )
                        }
                    },
                    Err(err) => {
                        // The origin is unreachable. `stale-if-error` is the only
                        // licence to answer anyway.
                        if self.config.honour_stale_if_error && stale_if_error.is_some() {
                            tracing::warn!(%err, url = %request.url, "origin unreachable, serving stale");
                            Ok(self.finish(
                                response.status,
                                response.headers.clone(),
                                Bytes::from(response.body.clone()),
                                Source::StaleOnError,
                                response.content,
                                started,
                            ))
                        } else {
                            Err(err)
                        }
                    }
                }
            }

            Lookup::Miss(_) => {
                if let Some(response) = self.try_peers(&request, started).await {
                    return Ok(response);
                }
                self.fetch_from_origin(&request, &[], started).await
            }
        }
    }

    /// Ask the origin once more, without a validator, for a body the store lost.
    ///
    /// Exactly once. The request is the one that was being revalidated, and it
    /// carries no validator of its own — `policy::evaluate` sends a client's
    /// conditional request straight to the origin, so one never reaches
    /// revalidation — so the origin has no reason to answer 304. If it does
    /// anyway there is nothing to serve, and that is an error rather than
    /// another round trip. Nothing here consults the store again, so this
    /// cannot loop.
    async fn refetch_lost(
        &self,
        request: &FetchRequest,
        started: Instant,
    ) -> Result<FetchResponse> {
        let response = self.fetch_from_origin(request, &[], started).await?;
        if response.status == 304 {
            return Err(NetError::LostBody(request.url.clone()));
        }
        Ok(response)
    }

    /// Fetch from the origin and answer with it, streaming where we can.
    ///
    /// **Where we cannot** is when the caller declared an integrity hash. A hash
    /// is checked against all of the bytes, and bytes already handed to the
    /// caller cannot be taken back — so a resource with declared integrity is
    /// buffered, checked, and only then returned. Those are subresources named
    /// in markup, which is a bounded set; everything else streams.
    async fn fetch_from_origin(
        &self,
        request: &FetchRequest,
        extra: &[(HeaderName, String)],
        started: Instant,
    ) -> Result<FetchResponse> {
        let (status, headers, incoming, protocol) = self.origin_any(request, extra).await?;

        if let Some(integrity) = &request.integrity {
            let body = collect_body(incoming, self.config.max_body_bytes).await?;
            return self
                .accept_declared(request, integrity, status, headers, body, started, protocol);
        }

        let body = stream_body(
            self.cache.clone(),
            request.clone(),
            status,
            headers.clone(),
            incoming,
            self.config.max_body_bytes,
        );
        // The address is not known yet — a body is not addressable until its
        // last byte — so `content` is None and the entry appears when it lands.
        Ok(self.finish_with(
            status,
            headers,
            body,
            Source::Origin,
            None,
            started,
            protocol,
        ))
    }

    /// One origin request over whichever protocol applies.
    ///
    /// HTTP/3 only where the origin has said it speaks it, and never at the cost
    /// of the fetch: a QUIC attempt that fails falls back to TCP, because UDP is
    /// blocked on a great many networks and a browser that could not load a page
    /// because of that would be broken.
    async fn origin_any(
        &self,
        request: &FetchRequest,
        extra: &[(HeaderName, String)],
    ) -> Result<(u16, HeaderMap, OriginBody, Protocol)> {
        let uri: Uri = request
            .url
            .parse()
            .map_err(|_| NetError::InvalidUrl(request.url.clone()))?;
        let authority = uri.authority().map(|a| a.to_string()).unwrap_or_default();

        if uri.scheme() == Some(&http::uri::Scheme::HTTPS) && self.alt_svc.should_try(&authority) {
            if let Some(quic) = &self.quic {
                let headers = crate::h3::with_extra(&request.headers, extra);
                match quic
                    .request(&uri, &request.method, &headers, request.body.clone())
                    .await
                {
                    Ok((status, response_headers, body)) => {
                        return Ok((
                            status,
                            response_headers,
                            OriginBody::Quic(body),
                            Protocol::Http3,
                        ));
                    }
                    Err(failed) => {
                        self.alt_svc.failed(&authority);
                        quic.forget(&authority).await;
                        if !crate::h3::may_retry_over_tcp(&request.method, failed.reached) {
                            tracing::debug!(url = %request.url, err = %failed.error, "HTTP/3 attempt failed after the request was sent; not repeating it");
                            return Err(failed.error);
                        }
                        tracing::debug!(url = %request.url, err = %failed.error, "HTTP/3 attempt failed; falling back to TCP");
                    }
                }
            }
        }

        let (status, headers, incoming, protocol) =
            origin_headers(&self.client, &self.config, request, extra).await?;
        // An origin advertises HTTP/3 on a response that came over TCP, which is
        // the only way it could: this is where the next request learns.
        if !authority.is_empty() {
            self.alt_svc.observe(&authority, &headers);
        }
        Ok((status, headers, OriginBody::Tcp(incoming), protocol))
    }

    /// A whole body from the origin, for a request that declared integrity:
    /// checked before anything is stored, shared or returned.
    ///
    /// A mismatch stores nothing, grants nothing, announces nothing and returns
    /// an error; the bytes never reach the caller. A match is stored, and then
    /// granted to peers under exactly the declared hashes it satisfied at the
    /// strongest level, which are then the only ones announced. A redirect this
    /// process will follow is not the representation the integrity describes,
    /// so it is neither checked nor shared — the response it leads to is.
    #[allow(clippy::too_many_arguments)]
    fn accept_declared(
        &self,
        request: &FetchRequest,
        integrity: &syndeo_cache::Integrity,
        status: u16,
        headers: HeaderMap,
        body: Bytes,
        started: Instant,
        protocol: Protocol,
    ) -> Result<FetchResponse> {
        if is_hop(request, status, &headers) {
            let content = self.store(request, status, &headers, &body, started);
            let source = if content.is_some() {
                Source::Origin
            } else {
                Source::PassThrough
            };
            return Ok(self.finish_with(status, headers, body, source, content, started, protocol));
        }
        if let Err(err) = integrity.check(&body) {
            tracing::warn!(url = %request.url, %err, "the origin's body does not satisfy the declared integrity");
            return Err(NetError::Integrity(format!("{}: {err}", request.url)));
        }
        let content = self.store(request, status, &headers, &body, started);
        if let Some(content) = content {
            match self.cache.grant_peer_eligibility(content, integrity) {
                Ok(granted) => self.announce(granted),
                Err(err) => {
                    tracing::warn!(url = %request.url, %err, "could not record a verified body")
                }
            }
        }
        let source = if content.is_some() {
            Source::Origin
        } else {
            Source::PassThrough
        };
        Ok(self.finish_with(status, headers, body, source, content, started, protocol))
    }

    /// Whether a stored response may answer this request.
    ///
    /// Always, unless the request declared integrity and these bytes do not
    /// satisfy it. Then the entry is dropped — only if it still holds these
    /// bytes — and the caller asks the origin once; a representation the
    /// declaration names is stored in its place when that verifies.
    fn stored_satisfies(
        &self,
        request: &FetchRequest,
        stored: &syndeo_cache::StoredResponse,
    ) -> bool {
        let Some(integrity) = &request.integrity else {
            return true;
        };
        if is_hop(request, stored.status, &stored.headers) {
            return true;
        }
        let matching = matching_strongest(integrity, &stored.body);
        if !matching.is_empty() {
            // Verified against a stored entry: as good a reason to share it as
            // a fresh download, and the only one a body first fetched without
            // a declaration will ever get.
            if let Some(content) = stored.content {
                self.share_verified(content, integrity, &matching);
            }
            return true;
        }
        tracing::info!(url = %request.url, "the stored body does not satisfy the declared integrity; asking the origin");
        if let Some(content) = stored.content {
            if let Err(err) = self.cache.discard_representation(&stored.key, content) {
                tracing::warn!(url = %request.url, %err, "could not drop the stored entry");
            }
        }
        false
    }

    /// Tell the swarm we hold a body someone else could ask for, under the
    /// hashes it was verified against.
    ///
    /// Only those: they are exactly the names another node can ask for and
    /// check, and exactly the names `serve` will answer. A weaker algorithm the
    /// page also declared, or a declared hash that did not match, is never
    /// published, even when another hash made the declaration valid.
    fn announce(&self, hashes: Vec<syndeo_cache::sri::Hash>) {
        announce_to(self.peers.as_ref(), &self.announcements, hashes);
    }

    /// Record a stored body a declaration was just verified against as
    /// shareable, and announce what that newly made shareable.
    ///
    /// `matching` is what the caller computed from the bytes in hand. When all
    /// of it is recorded already — every hit after the first — this is one
    /// read and nothing else: no write, and no second announcement.
    fn share_verified(
        &self,
        content: syndeo_cache::ContentId,
        integrity: &syndeo_cache::Integrity,
        matching: &[syndeo_cache::sri::Hash],
    ) {
        if let Ok(recorded) = self.cache.peer_eligibility(content) {
            if matching.iter().all(|hash| recorded.contains(hash)) {
                return;
            }
        }
        match self.cache.grant_peer_eligibility(content, integrity) {
            Ok(new) => self.announce(new),
            Err(err) => tracing::warn!(%err, "could not record a verified body as shareable"),
        }
    }

    /// How many hashes this process has asked the swarm to announce. Each
    /// shareable name is asked for once, however often the body is served.
    pub fn announcements(&self) -> u64 {
        self.announcements
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// What the swarm looks like from here, when we are in one.
    pub async fn peer_status(&self) -> Option<syndeo_peer::swarm::SwarmStatus> {
        match self.peers.as_ref() {
            Some(peers) => peers.status().await.ok(),
            None => None,
        }
    }

    fn store(
        &self,
        request: &FetchRequest,
        status: u16,
        headers: &HeaderMap,
        body: &Bytes,
        _started: Instant,
    ) -> Option<syndeo_cache::ContentId> {
        let now = now_secs();
        match self.cache.store(
            request.partition.as_deref(),
            request.method.as_str(),
            &request.url,
            &request.headers,
            status,
            headers,
            body,
            now,
            now,
        ) {
            Ok(StoreOutcome::Stored { content, .. }) => Some(content),
            // A range landed in the store but the body is still incomplete, so
            // there is no whole-body address to report yet.
            Ok(StoreOutcome::StoredPartial { held, complete_len }) => {
                tracing::debug!(url = %request.url, held, ?complete_len, "stored a partial body");
                None
            }
            Ok(StoreOutcome::NotStored(reason)) => {
                tracing::debug!(url = %request.url, reason, "not cached");
                None
            }
            Err(err) => {
                tracing::warn!(url = %request.url, %err, "cache write failed");
                None
            }
        }
    }

    /// Ask the swarm, but only when the caller can already say what the bytes
    /// must hash to.
    ///
    /// This is the rule, at the one place it could be broken: no declared
    /// integrity, no peer request. A body that comes back is checked against the
    /// declared hash inside the swarm before it is ever returned, and checked
    /// again here before it is stored, because the cost of the second check is
    /// nothing and the cost of being wrong is everything.
    async fn try_peers(&self, request: &FetchRequest, started: Instant) -> Option<FetchResponse> {
        let peers = self.peers.as_ref()?;
        let integrity = request.integrity.as_ref()?;
        let hash = integrity.strongest_hash()?;

        let body = match peers.fetch_integrity(&hash).await {
            Ok(body) => body,
            Err(err) => {
                tracing::debug!(url = %request.url, %err, "no peer had it");
                return None;
            }
        };
        if let Err(err) = self.cache.accept_peer_body(
            &syndeo_cache::PeerProof::Integrity(integrity.clone()),
            &body,
        ) {
            tracing::warn!(url = %request.url, %err, "a peer body failed its own declared hash");
            return None;
        }

        let body = Bytes::from(body);
        let mut headers = HeaderMap::new();
        // A peer hands over bytes, not a response. The only header we can honestly
        // synthesise is a type inferred from the URL.
        if let Some(content_type) = infer_content_type(&request.url) {
            if let Ok(value) = HeaderValue::from_str(content_type) {
                headers.insert(http::header::CONTENT_TYPE, value);
            }
        }
        if let Ok(value) = HeaderValue::from_str(&body.len().to_string()) {
            headers.insert(http::header::CONTENT_LENGTH, value);
        }

        // Not kept, so not announced: a peer hands over bytes without the
        // response metadata an entry needs, and `accept_peer_body` checks them
        // without writing them. Saying we have them would be a claim with
        // nothing behind it.

        Some(self.finish(
            200,
            headers,
            body.clone(),
            Source::Peer,
            Some(syndeo_cache::ContentId::of(&body)),
            started,
        ))
    }

    /// Revalidate a stale entry behind the response that was just served.
    ///
    /// Failures are swallowed on purpose: the stored entry stays exactly as it
    /// was, so `stale-if-error` keeps applying and the next request is no worse
    /// off than if we had never tried.
    fn spawn_refresh(
        &self,
        key: String,
        request: FetchRequest,
        conditional: Vec<(HeaderName, String)>,
    ) {
        {
            let mut inflight = self.refreshing.lock().expect("refresh set is not poisoned");
            if !inflight.insert(key.clone()) {
                tracing::trace!(%key, "a refresh for this entry is already running");
                return;
            }
        }

        let client = self.client.clone();
        let config = self.config.clone();
        let cache = self.cache.clone();
        let inflight = self.refreshing.clone();
        let peers = self.peers.clone();
        let announcements = self.announcements.clone();

        tokio::spawn(async move {
            let outcome =
                refresh_entry(&client, &config, &cache, &request, &key, &conditional).await;
            match outcome {
                Ok(Refreshed::Stored { shared }) => {
                    tracing::debug!(url = %request.url, "refreshed a stale entry");
                    announce_to(peers.as_ref(), &announcements, shared);
                }
                Ok(Refreshed::NotStored) => {
                    tracing::debug!(url = %request.url, "the refresh was not storable")
                }
                Err(err) => {
                    tracing::debug!(url = %request.url, %err, "background refresh failed; the stored entry stands")
                }
            }
            inflight
                .lock()
                .expect("refresh set is not poisoned")
                .remove(&key);
        });
    }

    fn finish(
        &self,
        status: u16,
        headers: HeaderMap,
        body: impl Into<FetchBody>,
        source: Source,
        content: Option<syndeo_cache::ContentId>,
        started: Instant,
    ) -> FetchResponse {
        self.finish_with(
            status,
            headers,
            body,
            source,
            content,
            started,
            Protocol::None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_with(
        &self,
        status: u16,
        headers: HeaderMap,
        body: impl Into<FetchBody>,
        source: Source,
        content: Option<syndeo_cache::ContentId>,
        started: Instant,
        protocol: Protocol,
    ) -> FetchResponse {
        FetchResponse {
            status,
            headers,
            body: body.into(),
            source,
            elapsed_ms: started.elapsed().as_millis() as u64,
            content,
            final_url: String::new(),
            redirects: 0,
            protocol,
        }
    }

    /// One request to the origin. This is the only place in the tree that opens
    /// a socket.
    async fn origin(
        &self,
        request: &FetchRequest,
        extra: &[(HeaderName, String)],
    ) -> Result<(u16, HeaderMap, Bytes)> {
        origin_request(&self.client, &self.config, request, extra).await
    }
}

/// A body from the origin, whichever protocol brought it.
///
/// The point of the enum is that nothing downstream branches on transport: the
/// tee, the buffered path and the caller all see one stream of chunks.
pub enum OriginBody {
    Tcp(hyper::body::Incoming),
    Quic(futures::stream::BoxStream<'static, Result<Bytes>>),
}

impl OriginBody {
    fn into_stream(self) -> futures::stream::BoxStream<'static, Result<Bytes>> {
        match self {
            OriginBody::Tcp(incoming) => http_body_util::BodyStream::new(incoming)
                .filter_map(|frame| async move {
                    match frame {
                        // Trailers carry no representation bytes.
                        Ok(frame) => frame.into_data().ok().map(Ok),
                        Err(err) => Some(Err(crate::body::truncated(err))),
                    }
                })
                .boxed(),
            OriginBody::Quic(stream) => stream,
        }
    }
}

/// The origin request, as a free function, so a background refresh can make one
/// without borrowing the whole [`Net`].
///
/// Returns as soon as the status line and headers are in. The body has not been
/// read at this point and may not have been sent — which is the whole point:
/// deciding what to do with a response should not cost the time it takes to
/// receive it.
async fn origin_headers(
    client: &HttpsClient,
    config: &NetConfig,
    request: &FetchRequest,
    extra: &[(HeaderName, String)],
) -> Result<(u16, HeaderMap, hyper::body::Incoming, Protocol)> {
    let uri: Uri = request
        .url
        .parse()
        .map_err(|_| NetError::InvalidUrl(request.url.clone()))?;
    if uri.host().is_none() {
        return Err(NetError::InvalidUrl(request.url.clone()));
    }

    let mut builder = Request::builder().method(request.method.clone()).uri(uri);
    {
        let headers = builder.headers_mut().expect("builder is valid");
        let tokens = syndeo_cache::headers::connection_tokens(&request.headers);
        for (name, value) in request.headers.iter() {
            if syndeo_cache::headers::is_hop_by_hop(name.as_str(), &tokens) {
                continue;
            }
            headers.append(name.clone(), value.clone());
        }
        for (name, value) in extra {
            if let Ok(v) = HeaderValue::from_str(value) {
                headers.insert(name.clone(), v);
            }
        }
        if !headers.contains_key(http::header::USER_AGENT) {
            if let Ok(v) = HeaderValue::from_str(&config.user_agent) {
                headers.insert(http::header::USER_AGENT, v);
            }
        }
        // What we cache is what the origin sent, so never invite an encoding we
        // would then have to undo before hashing it.
        headers.remove(http::header::ACCEPT_ENCODING);
    }

    let req = builder
        .body(Full::new(request.body.clone()))
        .map_err(NetError::Http)?;

    let response = client.request(req).await.map_err(|e| {
        // hyper's own Display stops at "client error (SendRequest)", which names
        // the call that failed and nothing about why. The cause is one or more
        // links further down, and without it every transport failure looks the
        // same.
        let mut chain = e.to_string();
        let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
        while let Some(cause) = source {
            chain.push_str(": ");
            chain.push_str(&cause.to_string());
            source = cause.source();
        }
        NetError::Transport(chain)
    })?;

    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let protocol = Protocol::from_version(response.version());
    Ok((status, headers, response.into_body(), protocol))
}

/// Read a whole body into memory, refusing one past the ceiling.
///
/// The ceiling is real here, because this is the path that holds the body: it is
/// taken when the caller declared an integrity hash, and a hash cannot be
/// checked against bytes that have already been handed out.
async fn collect_body(body: OriginBody, limit: u64) -> Result<Bytes> {
    let mut stream = body.into_stream();
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if out.len() as u64 + chunk.len() as u64 > limit {
            return Err(NetError::BodyTooLarge { limit });
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(out))
}

/// One request to the origin, buffered. Kept for the paths that need the whole
/// body before they can decide anything.
async fn origin_request(
    client: &HttpsClient,
    config: &NetConfig,
    request: &FetchRequest,
    extra: &[(HeaderName, String)],
) -> Result<(u16, HeaderMap, Bytes)> {
    let (status, headers, incoming, _) = origin_headers(client, config, request, extra).await?;
    let body = collect_body(OriginBody::Tcp(incoming), config.max_body_bytes).await?;
    Ok((status, headers, body))
}

/// The state a teed body carries between chunks.
struct Teed {
    frames: futures::stream::BoxStream<'static, Result<Bytes>>,
    tee: Tee,
    cache: Arc<Cache>,
    request: FetchRequest,
    status: u16,
    headers: HeaderMap,
    request_time: u64,
    done: bool,
}

/// Hand the bytes to the caller and to the cache at the same time.
///
/// The caller gets each chunk as it arrives; the cache gets a copy, hashed on
/// the way past, and the entry is written when the last one lands. A transfer
/// that fails part way leaves nothing behind, because a body is not addressable
/// until it is complete, and an incomplete one is deleted rather than kept.
///
/// A cache failure never fails the transfer. `max_body_bytes` stops the storing,
/// not the download — which is the difference between a size bound on the cache
/// and a size bound on the web.
fn stream_body(
    cache: Arc<Cache>,
    request: FetchRequest,
    status: u16,
    headers: HeaderMap,
    incoming: OriginBody,
    limit: u64,
) -> FetchBody {
    // A declared integrity is checked against the whole body before any of
    // it is handed out, so such a request is buffered and never gets here.
    debug_assert!(request.integrity.is_none());
    let writer = match cache.begin_streamed() {
        Ok(writer) => writer,
        Err(err) => {
            tracing::warn!(%err, "could not open a cache writer; streaming without caching");
            return FetchBody::Stream(incoming.into_stream());
        }
    };

    let state = Teed {
        frames: incoming.into_stream(),
        tee: Tee::new(writer, limit),
        cache,
        request,
        status,
        headers,
        request_time: now_secs(),
        done: false,
    };

    FetchBody::Stream(
        futures::stream::unfold(state, |mut state| async move {
            if state.done {
                return None;
            }
            match state.frames.next().await {
                Some(Ok(chunk)) => {
                    state.tee.observe(&chunk);
                    Some((Ok(chunk), state))
                }
                Some(Err(err)) => {
                    state.tee.discard();
                    state.done = true;
                    Some((Err(err), state))
                }
                None => {
                    // The last byte. This is where the entry appears.
                    finish_streamed(&mut state);
                    None
                }
            }
        })
        .boxed(),
    )
}

/// The last byte has arrived: write the entry.
fn finish_streamed(state: &mut Teed) {
    let Some(writer) = state.tee.take() else {
        return;
    };
    let now = now_secs();
    let outcome = state.cache.finish_streamed(
        state.request.partition.as_deref(),
        state.request.method.as_str(),
        &state.request.url,
        &state.request.headers,
        state.status,
        &state.headers,
        writer,
        state.request_time,
        now,
        Provenance::Origin,
    );
    match outcome {
        Ok(StoreOutcome::Stored { .. }) => {}
        Ok(StoreOutcome::NotStored(reason)) => {
            tracing::debug!(url = %state.request.url, reason, "not cached");
        }
        Ok(StoreOutcome::StoredPartial { .. }) => {}
        Err(err) => tracing::warn!(url = %state.request.url, %err, "cache write failed"),
    }
}

/// What a background refresh did to the store.
enum Refreshed {
    /// The entry was refreshed or replaced. `shared` is what a declared
    /// integrity newly made shareable, to be announced.
    Stored {
        shared: Vec<syndeo_cache::sri::Hash>,
    },
    NotStored,
}

/// Revalidate one stored entry, out of band.
async fn refresh_entry(
    client: &HttpsClient,
    config: &NetConfig,
    cache: &Cache,
    request: &FetchRequest,
    key: &str,
    conditional: &[(HeaderName, String)],
) -> Result<Refreshed> {
    let (status, headers, body) = origin_request(client, config, request, conditional).await?;
    let now = now_secs();

    if status == 304 {
        return Ok(match cache.record_not_modified(key, &headers, now, now)? {
            Some(_) => Refreshed::Stored { shared: Vec::new() },
            None => Refreshed::NotStored,
        });
    }

    // A refresh for a request that declared integrity stores nothing that
    // does not satisfy it; the entry stays as it was.
    let verified = match &request.integrity {
        Some(integrity) if !is_hop(request, status, &headers) => {
            if !integrity.verify(&body) {
                tracing::warn!(url = %request.url, "a refreshed body does not satisfy the declared integrity; not stored");
                return Ok(Refreshed::NotStored);
            }
            Some(integrity)
        }
        _ => None,
    };

    let outcome = cache.store(
        request.partition.as_deref(),
        request.method.as_str(),
        &request.url,
        &request.headers,
        status,
        &headers,
        &body,
        now,
        now,
    )?;
    Ok(match outcome {
        // Verified and stored: shareable, exactly as a foreground download is.
        StoreOutcome::Stored { content, .. } => Refreshed::Stored {
            shared: match verified {
                Some(integrity) => cache.grant_peer_eligibility(content, integrity)?,
                None => Vec::new(),
            },
        },
        StoreOutcome::StoredPartial { .. } => Refreshed::Stored { shared: Vec::new() },
        StoreOutcome::NotStored(_) => Refreshed::NotStored,
    })
}

/// The declared hashes at the strongest level that these bytes satisfy.
/// Empty exactly when the declaration is not satisfied.
fn matching_strongest(
    integrity: &syndeo_cache::Integrity,
    body: &[u8],
) -> Vec<syndeo_cache::sri::Hash> {
    let Some(strongest) = integrity.strongest() else {
        return Vec::new();
    };
    integrity
        .hashes
        .iter()
        .filter(|hash| hash.algorithm == strongest && hash.matches(body))
        .cloned()
        .collect()
}

/// Ask the swarm to announce these names, and count them.
///
/// Only verified names reach here: they are exactly the names another node can
/// ask for and check, and exactly the names `serve` will answer. A weaker
/// algorithm the page also declared, or a declared hash that did not match, is
/// never published, even when another hash made the declaration valid.
fn announce_to(
    peers: Option<&syndeo_peer::PeerHandle>,
    announcements: &std::sync::atomic::AtomicU64,
    hashes: Vec<syndeo_cache::sri::Hash>,
) {
    let Some(peers) = peers else {
        return;
    };
    if hashes.is_empty() {
        return;
    }
    announcements.fetch_add(hashes.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let peers = peers.clone();
    tokio::spawn(async move {
        for hash in hashes {
            if let Err(err) = peers.announce_integrity(&hash).await {
                tracing::debug!(%err, "could not announce a body to the swarm");
                return;
            }
        }
    });
}

/// Refuse a declared integrity that could not constrain anything.
///
/// Stricter than a browser's rule for `integrity` attributes, which treats a
/// value naming only unknown algorithms as no constraint at all. A caller here
/// declared one on purpose, and one that ends up empty is a declaration that
/// would silently accept any bytes, so it fails closed. The same goes for a
/// method or a range the declaration cannot describe: integrity names a whole
/// representation, fetched with GET.
fn refuse_unusable_integrity(request: &FetchRequest) -> Result<()> {
    let Some(integrity) = &request.integrity else {
        return Ok(());
    };
    let refusal = if integrity.is_empty() {
        "no usable hash was declared"
    } else if request.method != Method::GET {
        "declared integrity applies only to a GET"
    } else if request.headers.contains_key(http::header::RANGE) {
        "declared integrity describes a whole body, and this asked for a range"
    } else {
        return Ok(());
    };
    Err(NetError::Integrity(format!("{}: {refusal}", request.url)))
}

/// A redirect this process will follow rather than hand back: not the
/// representation a declared integrity describes, but the way to it.
fn is_hop(request: &FetchRequest, status: u16, headers: &HeaderMap) -> bool {
    request.redirect == RedirectMode::Follow
        && matches!(status, 301 | 302 | 303 | 307 | 308)
        && headers
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| !s.trim().is_empty())
}

/// A redirect that will not be followed after all — a target that does not
/// parse, or another scheme — is the final answer. With integrity declared it
/// is checked like any other, and a redirect satisfies none.
fn final_redirect(request: &FetchRequest, response: FetchResponse) -> Result<FetchResponse> {
    if request.integrity.is_some() {
        return Err(NetError::Integrity(format!(
            "{}: a redirect that cannot be followed does not satisfy the declared integrity",
            request.url
        )));
    }
    Ok(response)
}

/// A stored response with the cookies a 304 just set added back.
///
/// The cache keeps no cookie and serves none (see
/// `syndeo_cache::headers::NEVER_STORED`). A 304 is still a response from the
/// origin to this request, though, and what it sets belongs to the client that
/// made it: delivered here, once, every field in the order it came, and never
/// written anywhere. A background refresh has no client waiting, so its
/// cookies go nowhere.
fn with_cookies_of(mut served: HeaderMap, not_modified: &HeaderMap) -> HeaderMap {
    for name in syndeo_cache::headers::NEVER_STORED {
        let name = HeaderName::from_static(name);
        for value in not_modified.get_all(&name) {
            served.append(name.clone(), value.clone());
        }
    }
    served
}

/// The `Location` of a redirect we should follow, if this is one.
fn redirect_target(response: &FetchResponse) -> Option<String> {
    if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    response
        .headers
        .get(http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Headers that may follow a redirect.
///
/// Credentials do not cross an origin boundary: a redirect to another host must
/// not carry the first host's `Authorization` or `Cookie` with it.
fn forwardable_headers(headers: &HeaderMap, from: &str, to: &str) -> HeaderMap {
    let same_origin = match (url::Url::parse(from), url::Url::parse(to)) {
        (Ok(a), Ok(b)) => {
            a.scheme() == b.scheme()
                && a.host_str() == b.host_str()
                && a.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    };

    let mut out = HeaderMap::new();
    for (name, value) in headers.iter() {
        let sensitive = matches!(
            name.as_str(),
            "authorization" | "cookie" | "proxy-authorization"
        );
        if sensitive && !same_origin {
            continue;
        }
        // The new target has its own host and its own body.
        if matches!(name.as_str(), "host" | "content-length" | "content-type") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// The media type a URL suggests. Used only for a peer-supplied body, where
/// there is no response to take one from.
fn infer_content_type(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "html" | "htm" => "text/html; charset=utf-8",
        _ => return None,
    })
}
