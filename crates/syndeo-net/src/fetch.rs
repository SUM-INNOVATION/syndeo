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
use std::sync::Arc;
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
            Source::PassThrough => "pass-through",
        }
    }

    /// True when no body had to cross the network.
    pub fn avoided_transfer(self) -> bool {
        matches!(
            self,
            Source::Cache | Source::CacheStale | Source::Revalidated | Source::StaleOnError
        )
    }
}

#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl FetchRequest {
    pub fn get(url: impl Into<String>) -> Self {
        FetchRequest {
            method: Method::GET,
            url: url.into(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
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

        Ok(Net {
            cache,
            client,
            config,
        })
    }

    pub fn cache(&self) -> &Arc<Cache> {
        &self.cache
    }

    pub fn config(&self) -> &NetConfig {
        &self.config
    }

    /// Fetch a URL, consulting the cache first.
    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse> {
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
                Some(stored.content),
                started,
            )),

            Lookup::Stale {
                response,
                refresh_in_background,
                ..
            } => {
                if refresh_in_background {
                    self.spawn_refresh(request.clone());
                }
                Ok(self.finish(
                    response.status,
                    response.headers.clone(),
                    Bytes::from(response.body.clone()),
                    Source::CacheStale,
                    Some(response.content),
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
                                Some(refreshed.content),
                                started,
                            )),
                            None => Ok(self.finish(
                                response.status,
                                response.headers.clone(),
                                Bytes::from(response.body.clone()),
                                Source::CacheStale,
                                Some(response.content),
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
                                Some(response.content),
                                started,
                            ))
                        } else {
                            Err(err)
                        }
                    }
                }
            }

            Lookup::Miss(_) => {
                let (status, headers, body) = self.origin(&request, &[]).await?;
                let content = self.store(&request, status, &headers, &body, started);
                let source = if content.is_some() { Source::Origin } else { Source::PassThrough };
                Ok(self.finish(status, headers, body, source, content, started))
            }
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

    fn spawn_refresh(&self, _request: FetchRequest) {
        // A background refresh needs an owned handle; the proxy drives this via
        // its own task so the network process stays a plain request/response
        // surface. Left as an explicit no-op rather than a silent one.
        tracing::debug!("stale-while-revalidate refresh deferred to the caller");
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
        }
    }

    /// One request to the origin. This is the only place in the tree that opens
    /// a socket.
    async fn origin(
        &self,
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
                if let Ok(v) = HeaderValue::from_str(&self.config.user_agent) {
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

        let response = self
            .client
            .request(req)
            .await
            .map_err(|e| NetError::Transport(e.to_string()))?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let limit = self.config.max_body_bytes;
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
}
