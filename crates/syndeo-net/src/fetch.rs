//! The fetch API.
//!
//! Everything above this line — renderer, agent, shell — asks for a URL and gets
//! bytes. None of them open a socket, resolve a name, or see a certificate. That
//! boundary is the reason the cache can be swapped, shared, or fed from a peer
//! without anything upstream noticing.

use crate::config::NetConfig;
use crate::dns::Dns;
use crate::error::{NetError, Result};
use crate::tls;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use syndeo_cache::headers::now_secs;
use syndeo_cache::{Cache, CacheOptions, Lookup, StoreOutcome};

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
}

impl FetchRequest {
    pub fn get(url: impl Into<String>) -> Self {
        FetchRequest {
            method: Method::GET,
            url: url.into(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            integrity: None,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
            self.headers.append(n, v);
        }
        self
    }
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub source: Source,
    pub elapsed_ms: u64,
    /// The BLAKE3 content address, when the body passed through the cache.
    pub content: Option<syndeo_cache::ContentId>,
    /// Where the response actually came from, after redirects.
    pub final_url: String,
    /// How many redirects were followed to get here.
    pub redirects: u8,
}

type HttpsClient = Client<
    hyper_rustls::HttpsConnector<HttpConnector<Dns>>,
    Full<Bytes>,
>;

/// The network process, as a library. The binary is a thin wrapper around this.
pub struct Net {
    cache: Arc<Cache>,
    client: HttpsClient,
    config: NetConfig,
    peers: Option<syndeo_peer::PeerHandle>,
    /// Entry keys with a background revalidation already in flight.
    ///
    /// A popular entry going stale is exactly when many requests arrive at once,
    /// and one refresh per request would turn `stale-while-revalidate` from a
    /// saving into a stampede. They collapse onto the first.
    refreshing: Arc<Mutex<HashSet<String>>>,
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
            Some(peer_config) => match syndeo_peer::PeerNode::start(cache.clone(), peer_config.clone()) {
                Ok(handle) => {
                    tracing::info!(peer = %handle.peer_id(), "joined the peer swarm");
                    Some(handle)
                }
                Err(err) => {
                    tracing::warn!(%err, "could not join the peer swarm; continuing without it");
                    None
                }
            },
            None => None,
        };

        Ok(Net {
            cache,
            client,
            config,
            peers,
            refreshing: Arc::new(Mutex::new(HashSet::new())),
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
        let mut current = request;
        let mut redirects = 0u8;
        let mut visited = vec![Cache::normalize_url(&current.url)];

        loop {
            let mut response = self.fetch_once(current.clone()).await?;
            response.final_url = current.url.clone();
            response.redirects = redirects;

            let Some(location) = redirect_target(&response) else {
                return Ok(response);
            };
            if redirects >= self.config.max_redirects {
                return Err(NetError::TooManyRedirects {
                    limit: self.config.max_redirects,
                });
            }

            let next = match url::Url::parse(&current.url).and_then(|base| base.join(&location)) {
                Ok(u) => u,
                Err(_) => return Ok(response),
            };
            // Only http and https; a redirect is not a way to reach another
            // scheme's handler.
            if !matches!(next.scheme(), "http" | "https") {
                return Ok(response);
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
                method: if preserve {
                    current.method.clone()
                } else {
                    Method::GET
                },
                url: next.to_string(),
                headers: forwardable_headers(&current.headers, &current.url, next.as_str()),
                body: if preserve { current.body.clone() } else { Bytes::new() },
                // Integrity was declared for the resource, not for a redirect
                // hop, and it still describes whatever finally answers.
                integrity: current.integrity.clone(),
            };
            redirects += 1;
        }
    }

    async fn fetch_once(&self, request: FetchRequest) -> Result<FetchResponse> {
        let started = Instant::now();
        let method = request.method.as_str().to_string();

        // An unsafe method invalidates whatever we hold for the target.
        if syndeo_cache::policy::invalidates(&method) {
            let _ = self.cache.invalidate(&method, &request.url);
            let response = self.origin(&request, &[]).await?;
            return Ok(self.finish(response.0, response.1, response.2, Source::PassThrough, None, started));
        }

        match self.cache.lookup(&method, &request.url, &request.headers)? {
            Lookup::Fresh(stored) => Ok(self.finish(
                stored.status,
                stored.headers.clone(),
                Bytes::from(stored.body.clone()),
                Source::Cache,
                stored.content,
                started,
            )),

            Lookup::Stale {
                response,
                refresh_in_background,
                ..
            } => {
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
                let attempt = self.origin(&request, &conditional).await;
                match attempt {
                    Ok((304, headers, _)) => {
                        let now = now_secs();
                        match self.cache.record_not_modified(&response.key, &headers, now, now)? {
                            Some(refreshed) => Ok(self.finish(
                                refreshed.status,
                                refreshed.headers.clone(),
                                Bytes::from(refreshed.body.clone()),
                                Source::Revalidated,
                                refreshed.content,
                                started,
                            )),
                            None => Ok(self.finish(
                                response.status,
                                response.headers.clone(),
                                Bytes::from(response.body.clone()),
                                Source::CacheStale,
                                response.content,
                                started,
                            )),
                        }
                    }
                    Ok((status, headers, body)) => {
                        let content = self.store(&request, status, &headers, &body, started);
                        Ok(self.finish(status, headers, body, Source::Origin, content, started))
                    }
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
                let (status, headers, body) = self.origin(&request, &[]).await?;
                let content = self.store(&request, status, &headers, &body, started);
                if content.is_some() {
                    self.announce(&request);
                }
                let source = if content.is_some() { Source::Origin } else { Source::PassThrough };
                Ok(self.finish(status, headers, body, source, content, started))
            }
        }
    }

    /// Tell the swarm we hold a body someone else could ask for.
    ///
    /// Only bodies a page declared an integrity hash for: those are exactly the
    /// ones another node can name and check, and announcing anything else would
    /// be disclosing what we have been reading for no one's benefit. Every
    /// declared digest is published, not just the strongest, because a different
    /// page may name the same body by a different algorithm.
    fn announce(&self, request: &FetchRequest) {
        let (Some(peers), Some(integrity)) = (&self.peers, &request.integrity) else {
            return;
        };
        let hashes = integrity.hashes.clone();
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
        if let Err(err) = self
            .cache
            .accept_peer_body(&syndeo_cache::PeerProof::Integrity(integrity.clone()), &body)
        {
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

        // We hold it now, so the next node to want it has one more place to ask.
        self.announce(request);

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

        tokio::spawn(async move {
            let outcome = refresh_entry(&client, &config, &cache, &request, &key, &conditional).await;
            match outcome {
                Ok(true) => tracing::debug!(url = %request.url, "refreshed a stale entry"),
                Ok(false) => tracing::debug!(url = %request.url, "the refresh was not storable"),
                Err(err) => tracing::debug!(url = %request.url, %err, "background refresh failed; the stored entry stands"),
            }
            inflight.lock().expect("refresh set is not poisoned").remove(&key);
        });
    }

    fn finish(
        &self,
        status: u16,
        headers: HeaderMap,
        body: Bytes,
        source: Source,
        content: Option<syndeo_cache::ContentId>,
        started: Instant,
    ) -> FetchResponse {
        FetchResponse {
            status,
            headers,
            body,
            source,
            elapsed_ms: started.elapsed().as_millis() as u64,
            content,
            final_url: String::new(),
            redirects: 0,
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

/// The origin request, as a free function, so a background refresh can make one
/// without borrowing the whole [`Net`].
async fn origin_request(
    client: &HttpsClient,
    config: &NetConfig,
    request: &FetchRequest,
    extra: &[(HeaderName, String)],
) -> Result<(u16, HeaderMap, Bytes)> {
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
        // We buffer whole bodies, so never invite a chunked stream we then
        // have to reassemble differently.
        headers.remove(http::header::ACCEPT_ENCODING);
    }

    let req = builder
        .body(Full::new(request.body.clone()))
        .map_err(NetError::Http)?;

    let response = client
        .request(req)
        .await
        .map_err(|e| NetError::Transport(e.to_string()))?;

    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let limit = config.max_body_bytes;
    let collected = response
        .into_body()
        .collect()
        .await
        .map_err(|e| NetError::Transport(e.to_string()))?;
    let body = collected.to_bytes();
    if body.len() as u64 > limit {
        return Err(NetError::BodyTooLarge { limit });
    }

    Ok((status, headers, body))
}

/// Revalidate one stored entry, out of band. Returns whether the store changed.
async fn refresh_entry(
    client: &HttpsClient,
    config: &NetConfig,
    cache: &Cache,
    request: &FetchRequest,
    key: &str,
    conditional: &[(HeaderName, String)],
) -> Result<bool> {
    let (status, headers, body) = origin_request(client, config, request, conditional).await?;
    let now = now_secs();

    if status == 304 {
        return Ok(cache
            .record_not_modified(key, &headers, now, now)?
            .is_some());
    }

    let outcome = cache.store(
        request.method.as_str(),
        &request.url,
        &request.headers,
        status,
        &headers,
        &body,
        now,
        now,
    )?;
    Ok(!matches!(outcome, StoreOutcome::NotStored(_)))
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
