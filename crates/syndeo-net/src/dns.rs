//! DNS resolution, over UDP/TCP by default and over TLS or HTTPS when asked.
//!
//! Resolution lives in the network process. A renderer never learns a name, let
//! alone an address.

use crate::error::{NetError, Result};
use hickory_resolver::config::{
    ResolveHosts, ResolverConfig, ServerGroup, CLOUDFLARE, GOOGLE, QUAD9,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;
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
    /// The addresses and the name its certificate has to carry. One group per
    /// provider, and the transport is chosen from it: the same servers answer
    /// over TLS and over HTTPS.
    fn group(self) -> ServerGroup<'static> {
        match self {
            Resolver::Cloudflare => CLOUDFLARE,
            Resolver::Google => GOOGLE,
            Resolver::Quad9 => QUAD9,
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
    inner: Arc<TokioResolver>,
}

impl Dns {
    pub fn new(mode: &DnsMode) -> Result<Self> {
        // The system configuration is read where there is one to read, and a
        // machine whose resolv.conf is missing or unparseable still resolves,
        // through hickory's defaults, rather than failing to start the network
        // process at all.
        let mut builder = match mode {
            DnsMode::System => TokioResolver::builder_tokio().unwrap_or_else(|_| {
                TokioResolver::builder_with_config(
                    ResolverConfig::default(),
                    TokioRuntimeProvider::default(),
                )
            }),
            DnsMode::Tls(r) => TokioResolver::builder_with_config(
                ResolverConfig::from_name_servers(r.group().tls().collect()),
                TokioRuntimeProvider::default(),
            ),
            DnsMode::Https(r) => TokioResolver::builder_with_config(
                ResolverConfig::from_name_servers(r.group().https().collect()),
                TokioRuntimeProvider::default(),
            ),
        };

        let options = builder.options_mut();
        options.cache_size = 256;
        options.use_hosts_file = ResolveHosts::Auto;

        let resolver = builder
            .build()
            .map_err(|e| NetError::Dns(format!("configuring the resolver: {e}")))?;
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
