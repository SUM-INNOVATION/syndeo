//! The network process.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use syndeo_ipc::transport::{Endpoint, Server};
use syndeo_net::{DnsMode, Net, NetConfig};

#[derive(Parser)]
#[command(
    name = "syndeo-net",
    version,
    about = "The only process that opens a socket"
)]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    cache: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    /// Serve one site's stored resources to another.
    ///
    /// Off by default: a cache keyed on the URL alone lets one site time a
    /// fetch and learn where you have been. This raises the hit rate on
    /// third-party resources and gives that away, and exists so the two can be
    /// measured against each other.
    #[arg(long)]
    unpartitioned_cache: bool,
    #[arg(long, default_value = "doh:cloudflare")]
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

    // A shell that was force-quit runs no destructors, so `kill_on_drop` never
    // fires and this process would outlive it holding a socket.
    syndeo_ipc::exit_when_parent_does();
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
        partition_cache: !cli.unpartitioned_cache,
        ..NetConfig::default()
    })?);
    if let Some(peer_id) = net.peer_id() {
        tracing::info!(%peer_id, "peer fetch is on");
    }

    // Blobs nothing points at any more.
    //
    // `enforce_budget` runs on every store and keeps the cache inside its size
    // limit, but it only deletes a blob when the last entry referring to it is
    // evicted. A blob can lose its last reference another way — a process
    // killed between writing the body and committing the entry, an entry
    // dropped by a schema migration — and nothing was collecting those: the
    // sweep existed and had no caller outside its own test, so a long-lived
    // cache accumulated bytes that no page could ever be served from.
    //
    // Once at startup, then hourly. Unreferenced blobs are not urgent, and the
    // sweep walks the whole index, so doing it on a store would pay a linear
    // cost for a rare event.
    {
        let net = net.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
            loop {
                every.tick().await;
                match net.cache().collect_garbage() {
                    Ok(0) => tracing::debug!("no unreferenced blobs"),
                    Ok(n) => tracing::info!(blobs = n, "collected unreferenced blobs"),
                    Err(err) => tracing::warn!(%err, "collecting unreferenced blobs"),
                }
            }
        });
    }

    let endpoint = match cli.socket {
        Some(path) => Endpoint::new(path),
        None => Endpoint::in_runtime_dir(syndeo_ipc::transport::runtime_dir_for(&home), "net")?,
    };
    // Stop on SIGTERM rather than only on SIGKILL, so the cache gets to close.
    // Its index commits statistics and eviction stamps with relaxed durability
    // and flushes them on the way out; a process that is killed outright takes
    // the record of everything it served with it.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::select! {
        _ = syndeo_net::service::serve(net.clone(), Server::bind(endpoint)?) => {}
        _ = terminate.recv() => tracing::debug!("asked to stop"),
        _ = interrupt.recv() => tracing::debug!("interrupted"),
    }
    // Everything the cache is holding, on disk, before this returns.
    drop(net);
    Ok(())
}
