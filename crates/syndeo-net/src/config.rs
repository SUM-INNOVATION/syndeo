use crate::dns::DnsMode;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct NetConfig {
    pub cache_root: PathBuf,
    /// A browser cache is private. The measuring proxy runs shared.
    pub shared_cache: bool,
    pub dns: DnsMode,
    /// Largest response body we will buffer, and therefore cache.
    pub max_body_bytes: u64,
    pub user_agent: String,
    /// Serve a stale body when the origin is unreachable and `stale-if-error`
    /// still covers it.
    pub honour_stale_if_error: bool,
    /// How many redirects to follow before giving up.
    pub max_redirects: u8,
    /// Join the peer swarm. A peer is only ever asked for a body the caller can
    /// already name by hash, so this is off by default and harmless when on.
    pub peers: Option<syndeo_peer::PeerConfig>,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            cache_root: default_cache_root(),
            shared_cache: false,
            dns: DnsMode::System,
            max_body_bytes: 64 * 1024 * 1024,
            user_agent: concat!("Syndeo/", env!("CARGO_PKG_VERSION")).to_string(),
            honour_stale_if_error: true,
            max_redirects: 10,
            peers: None,
        }
    }
}

pub fn default_cache_root() -> PathBuf {
    std::env::var_os("SYNDEO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".syndeo")
        })
        .join("cache")
}
