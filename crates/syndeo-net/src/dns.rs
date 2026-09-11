//! DNS resolution, over UDP/TCP by default and over TLS or HTTPS when asked.
//!
//! Resolution lives in the network process. A renderer never learns a name, let
//! alone an address.

use crate::error::{NetError, Result};
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use hyper_util::client::legacy::connect::dns::Name;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DnsMode {
    /// Whatever the operating system is configured to use.
    #[default]
    System,
    /// DNS over TLS to a named resolver.
    Tls(Resolver),
    /// DNS over HTTPS to a named resolver.
    Https(Resolver),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolver {
    Cloudflare,
    Google,
    Quad9,
}

impl Resolver {
    fn tls_group(self) -> NameServerConfigGroup {
        match self {
            Resolver::Cloudflare => NameServerConfigGroup::cloudflare_tls(),
            Resolver::Google => NameServerConfigGroup::google_tls(),
            Resolver::Quad9 => NameServerConfigGroup::quad9_tls(),
        }
    }

    fn https_group(self) -> NameServerConfigGroup {
        match self {
            Resolver::Cloudflare => NameServerConfigGroup::cloudflare_https(),
            Resolver::Google => NameServerConfigGroup::google_https(),
            Resolver::Quad9 => NameServerConfigGroup::quad9_https(),
        }
    }
}

impl std::str::FromStr for DnsMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let (mode, provider) = match s.split_once(':') {
            Some((m, p)) => (m, p),
            None => (s, "cloudflare"),
        };
        let resolver = match provider.to_ascii_lowercase().as_str() {
            "cloudflare" => Resolver::Cloudflare,
            "google" => Resolver::Google,
            "quad9" => Resolver::Quad9,
            other => return Err(format!("unknown resolver {other}")),
        };
        match mode.to_ascii_lowercase().as_str() {
            "system" => Ok(DnsMode::System),
            "dot" | "tls" => Ok(DnsMode::Tls(resolver)),
            "doh" | "https" => Ok(DnsMode::Https(resolver)),
            other => Err(format!("unknown dns mode {other}")),
        }
    }
}

/// A hickory resolver dressed up as the resolver hyper's connector expects.
#[derive(Clone)]
pub struct Dns {
    inner: Arc<TokioAsyncResolver>,
}

impl Dns {
    pub fn new(mode: &DnsMode) -> Result<Self> {
        let mut opts = ResolverOpts::default();
        opts.cache_size = 256;
        opts.use_hosts_file = true;

        let resolver = match mode {
            DnsMode::System => TokioAsyncResolver::tokio_from_system_conf().unwrap_or_else(|_| {
                TokioAsyncResolver::tokio(ResolverConfig::default(), opts.clone())
            }),
            DnsMode::Tls(r) => TokioAsyncResolver::tokio(
                ResolverConfig::from_parts(None, vec![], r.tls_group()),
                opts.clone(),
            ),
            DnsMode::Https(r) => TokioAsyncResolver::tokio(
                ResolverConfig::from_parts(None, vec![], r.https_group()),
                opts.clone(),
            ),
        };
        Ok(Dns {
            inner: Arc::new(resolver),
        })
    }

    pub async fn lookup(&self, host: &str) -> Result<Vec<std::net::IpAddr>> {
        let response = self
            .inner
            .lookup_ip(host)
            .await
            .map_err(|e| NetError::Dns(e.to_string()))?;
        Ok(response.iter().collect())
    }
}

pub struct Addrs(std::vec::IntoIter<SocketAddr>);

impl Iterator for Addrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<SocketAddr> {
        self.0.next()
    }
}

impl tower_service::Service<Name> for Dns {
    type Response = Addrs;
    type Error = NetError;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<Addrs>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let resolver = self.inner.clone();
        Box::pin(async move {
            let response = resolver
                .lookup_ip(name.as_str())
                .await
                .map_err(|e| NetError::Dns(e.to_string()))?;
            let addrs: Vec<SocketAddr> = response.iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            Ok(Addrs(addrs.into_iter()))
        })
    }
}
