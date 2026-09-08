//! Step two of the build order: wrap the cache in a local intercepting proxy,
//! point an ordinary browser at it, and measure hit rate and dedupe ratio on
//! real traffic. That measurement is the go/no-go, and it costs weeks, not
//! quarters. No browser exists yet at this point, deliberately.

mod ca;
mod stats;

use anyhow::{Context, Result};
use bytes::Bytes;
use ca::CertificateAuthority;
use clap::{Parser, Subcommand};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ServerBuilder;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use syndeo_net::{DnsMode, FetchRequest, Net, NetConfig};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(
    name = "syndeo-proxy",
    version,
    about = "Put the Syndeo cache in front of an ordinary browser and measure it"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the proxy.
    Run(RunArgs),
    /// Print the authority certificate path and how to trust it.
    Ca(CaArgs),
    /// Print cache statistics and exit.
    Stats(StatsArgs),
}

#[derive(Parser, Clone)]
struct RunArgs {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:8899")]
    listen: SocketAddr,
    /// Where the cache lives.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, default_value = "system")]
    dns: String,
    /// Run the cache with shared-cache semantics.
    #[arg(long, default_value_t = true)]
    shared: bool,
    /// Log one line per request with its source and timing.
    #[arg(long, default_value_t = true)]
    trace_requests: bool,
}

#[derive(Parser)]
struct CaArgs {
    /// Write the certificate to stdout instead of describing it.
    #[arg(long)]
    print: bool,
}

#[derive(Parser)]
struct StatsArgs {
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Emit JSON instead of a summary.
    #[arg(long)]
    json: bool,
}

fn home() -> PathBuf {
    std::env::var_os("SYNDEO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".syndeo")
        })
}

fn cache_root(override_path: &Option<PathBuf>) -> PathBuf {
    override_path.clone().unwrap_or_else(|| home().join("cache"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_proxy=info,syndeo_net=info")),
        )
        .with_target(false)
        .init();

    match Cli::parse().command.unwrap_or(Command::Run(RunArgs {
        listen: "127.0.0.1:8899".parse().unwrap(),
        cache: None,
        dns: "system".into(),
        shared: true,
        trace_requests: true,
    })) {
        Command::Run(args) => run(args).await,
        Command::Ca(args) => {
            let authority = CertificateAuthority::load_or_create(home().join("proxy"))?;
            if args.print {
                print!("{}", authority.certificate_pem());
                return Ok(());
            }
            let path = authority.certificate_path();
            println!("authority certificate: {}", path.display());
            println!();
            println!("Trust it for the duration of the measurement, then remove it:");
            println!("  macOS   sudo security add-trusted-cert -d -r trustRoot \\");
            println!("            -k /Library/Keychains/System.keychain {}", path.display());
            println!("  remove  sudo security delete-certificate -c 'Syndeo Local Measurement CA' \\");
            println!("            /Library/Keychains/System.keychain");
            println!();
            println!("Then point a browser at the proxy, for example:");
            println!("  /Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome \\");
            println!("    --proxy-server=http://127.0.0.1:8899 --user-data-dir=/tmp/syndeo-measure");
            Ok(())
        }
        Command::Stats(args) => {
            let cache = syndeo_cache::Cache::open(cache_root(&args.cache))?;
            let stats = cache.stats()?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!("{}", stats.render());
            }
            Ok(())
        }
    }
}

struct Proxy {
    net: Net,
    authority: CertificateAuthority,
    trace: bool,
}

async fn run(args: RunArgs) -> Result<()> {
    let dns: DnsMode = args
        .dns
        .parse()
        .map_err(|e: String| anyhow::anyhow!("--dns: {e}"))?;

    let net = Net::new(NetConfig {
        cache_root: cache_root(&args.cache),
        shared_cache: args.shared,
        dns,
        ..NetConfig::default()
    })
    .context("starting the network process")?;

    let authority = CertificateAuthority::load_or_create(home().join("proxy"))?;
    let cert_path = authority.certificate_path();

    let proxy = Arc::new(Proxy {
        net,
        authority,
        trace: args.trace_requests,
    });

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;

    tracing::info!(listen = %args.listen, cache = %proxy.net.config().cache_root.display(), "proxy up");
    tracing::info!(certificate = %cert_path.display(), "trust this to intercept https");
    tracing::info!("statistics at http://syndeo.local/stats through the proxy, or `syndeo-proxy stats`");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(%err, "accept failed");
                continue;
            }
        };
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let proxy = proxy.clone();
                async move { handle(proxy, req).await }
            });
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await
            {
                tracing::debug!(%peer, %err, "connection closed");
            }
        });
    }
}

type Body = Full<Bytes>;

async fn handle(proxy: Arc<Proxy>, req: Request<Incoming>) -> Result<Response<Body>, hyper::Error> {
    if req.method() == hyper::Method::CONNECT {
        return Ok(connect(proxy, req));
    }
    Ok(forward(proxy, req, None).await)
}

/// `CONNECT host:port` — answer 200, then take over the tunnel and terminate TLS
/// with a leaf we mint for that host.
fn connect(proxy: Arc<Proxy>, req: Request<Incoming>) -> Response<Body> {
    let Some(authority) = req.uri().authority().cloned() else {
        return text(StatusCode::BAD_REQUEST, "CONNECT needs an authority");
    };
    let host = authority.host().to_string();
    let port = authority.port_u16().unwrap_or(443);

    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(err) => {
                tracing::debug!(%err, "upgrade failed");
                return;
            }
        };
        let config = match proxy.authority.server_config(&host) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(%host, %err, "could not mint a leaf certificate");
                return;
            }
        };
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let tls = match acceptor.accept(TokioIo::new(upgraded)).await {
            Ok(s) => s,
            Err(err) => {
                tracing::debug!(%host, %err, "tls handshake failed");
                return;
            }
        };

        let origin = Arc::new(format!("https://{host}{}", if port == 443 { String::new() } else { format!(":{port}") }));
        let service = service_fn(move |req| {
            let proxy = proxy.clone();
            let origin = origin.clone();
            async move { Ok::<_, hyper::Error>(forward(proxy, req, Some(origin.to_string())).await) }
        });

        if let Err(err) = ServerBuilder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(tls), service)
            .await
        {
            tracing::debug!(%host, %err, "tunnelled connection closed");
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .body(Full::new(Bytes::new()))
        .expect("static response")
}

/// Turn one proxied request into a fetch, and the fetch back into a response.
async fn forward(proxy: Arc<Proxy>, req: Request<Incoming>, origin: Option<String>) -> Response<Body> {
    let method = req.method().clone();
    let url = match absolute_url(&req, origin.as_deref()) {
        Some(u) => u,
        None => return text(StatusCode::BAD_REQUEST, "could not determine the target url"),
    };

    if let Some(response) = stats::intercept(&proxy.net, &url) {
        return response;
    }

    let headers = req.headers().clone();
    let body = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => return text(StatusCode::BAD_REQUEST, &format!("reading the request body: {err}")),
    };

    let fetch = FetchRequest {
        method: method.clone(),
        url: url.clone(),
        headers,
        body,
        // A proxied browser does not tell us what a subresource's integrity is,
        // so nothing here is ever eligible for peer fetch. The measurement is of
        // the cache, uncontaminated.
        integrity: None,
    };

    match proxy.net.fetch(fetch).await {
        Ok(response) => {
            if proxy.trace {
                tracing::info!(
                    "{:>13}  {:>4}  {:>5}ms  {:>9}  {}",
                    response.source.as_str(),
                    response.status,
                    response.elapsed_ms,
                    syndeo_cache::stats::human(response.body.len() as u64),
                    url
                );
            }
            let mut builder = Response::builder().status(response.status);
            {
                let out = builder.headers_mut().expect("builder is valid");
                let tokens = syndeo_cache::headers::connection_tokens(&response.headers);
                for (name, value) in response.headers.iter() {
                    if syndeo_cache::headers::is_hop_by_hop(name.as_str(), &tokens) {
                        continue;
                    }
                    if name == http::header::CONTENT_LENGTH {
                        continue;
                    }
                    out.append(name.clone(), value.clone());
                }
                out.insert(
                    "x-syndeo-source",
                    http::HeaderValue::from_static(response.source.as_str()),
                );
            }
            builder
                .body(Full::new(response.body))
                .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "malformed response"))
        }
        Err(err) => {
            tracing::warn!(%url, %err, "fetch failed");
            text(StatusCode::BAD_GATEWAY, &format!("{err}"))
        }
    }
}

/// Proxied requests arrive in absolute form; requests inside a CONNECT tunnel
/// arrive in origin form and need the tunnel's authority put back.
fn absolute_url(req: &Request<Incoming>, origin: Option<&str>) -> Option<String> {
    let uri = req.uri();
    if uri.scheme().is_some() && uri.authority().is_some() {
        return Some(uri.to_string());
    }
    if let Some(origin) = origin {
        return Some(format!("{origin}{}", uri.path_and_query()?.as_str()));
    }
    let host = req.headers().get(http::header::HOST)?.to_str().ok()?;
    Some(format!("http://{host}{}", uri.path_and_query()?.as_str()))
}

fn text(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(message.to_string())))
        .expect("static response")
}
