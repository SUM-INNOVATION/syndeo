//! The network process.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use syndeo_ipc::transport::{Endpoint, Server};
use syndeo_net::{DnsMode, Net, NetConfig};

#[derive(Parser)]
#[command(name = "syndeo-net", version, about = "The only process that opens a socket")]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    cache: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, default_value = "system")]
    dns: String,
    #[arg(long)]
    home: Option<PathBuf>,
    /// Join the peer swarm. A peer is only ever asked for a body the caller can
    /// already name by hash.
    #[arg(long)]
    peers: bool,
    /// Multiaddresses to dial on start, repeatable.
    #[arg(long = "bootstrap")]
    bootstrap: Vec<String>,
    /// Multiaddress to listen on for peers, repeatable. Defaults to an
    /// ephemeral port on every interface.
    #[arg(long = "listen")]
    listen: Vec<String>,
    /// Join the swarm and answer, but never ask. For a node whose job is to
    /// seed rather than to browse.
    #[arg(long)]
    serve_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_net=info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(|| {
        std::env::var_os("SYNDEO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".syndeo")
            })
    });

    let dns: DnsMode = cli
        .dns
        .parse()
        .map_err(|e: String| anyhow::anyhow!("--dns: {e}"))?;

    let peers = if cli.peers || cli.serve_only {
        let mut config = syndeo_peer::PeerConfig::default();
        for address in &cli.bootstrap {
            match address.parse() {
                Ok(parsed) => config.bootstrap.push(parsed),
                Err(err) => anyhow::bail!("--bootstrap {address}: {err}"),
            }
        }
        if !cli.listen.is_empty() {
            config.listen.clear();
            for address in &cli.listen {
                match address.parse() {
                    Ok(parsed) => config.listen.push(parsed),
                    Err(err) => anyhow::bail!("--listen {address}: {err}"),
                }
            }
        }
        Some(config)
    } else {
        None
    };

    let net = Arc::new(Net::new(NetConfig {
        cache_root: cli.cache.unwrap_or_else(|| home.join("cache")),
        dns,
        peers,
        ..NetConfig::default()
    })?);
    if let Some(peer_id) = net.peer_id() {
        tracing::info!(%peer_id, "peer fetch is on");
    }

    let endpoint = match cli.socket {
        Some(path) => Endpoint::new(path),
        None => Endpoint::in_runtime_dir(syndeo_ipc::transport::runtime_dir_for(&home), "net")?,
    };
    syndeo_net::service::serve(net, Server::bind(endpoint)?).await;
    Ok(())
}
