//! Step two of the build order: wrap the cache in a local intercepting proxy,
//! point an ordinary browser at it, and measure hit rate and dedupe ratio on
//! real traffic. That measurement is the go/no-go, and it costs weeks, not
//! quarters. No browser exists yet at this point, deliberately.

mod auth;
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
use syndeo_net::{DnsMode, FetchRequest, Net, NetConfig, RedirectMode};
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

#[derive(Parser, Clone, Debug, PartialEq)]
struct RunArgs {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:8899")]
    listen: SocketAddr,
    /// Where the cache lives.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, default_value = "doh:cloudflare")]
    dns: String,
    /// Run the cache with shared-cache semantics. On unless `--shared=false`.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    shared: bool,
    /// Log one line per request with its source and timing. On unless
    /// `--trace-requests=false`.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    trace_requests: bool,
    /// The largest request body, in bytes, a client may send through the
    /// proxy. A larger upload is refused with 413 before it is read.
    #[arg(long, default_value_t = DEFAULT_MAX_REQUEST_BODY)]
    max_request_body: u64,
    /// Exit when whoever started this closes our stdin.
    ///
    /// For syndeo-webkit, which starts its own proxy and keeps the other end of
    /// that pipe: the kernel closes it however the browser ends, force-quit and
    /// `kill -9` included. Off unless asked for, because a proxy started from a
    /// script or with `nohup` has /dev/null for stdin, which reads as closed at
    /// once.
    #[arg(long, hide = true)]
    exit_with_parent: bool,
    /// Once listening, write one line to stdout saying where — see
    /// [`READY`] — and nothing else there; logs go to stderr instead.
    ///
    /// For syndeo-webkit, which starts its own proxy on a port the system
    /// picks, and learns which one from the proxy itself rather than by
    /// finding something that answers.
    #[arg(long, hide = true)]
    announce: bool,
    /// Require every client to present a per-launch credential, read from
    /// stdin as one frame before anything else is read from it.
    ///
    /// For syndeo-webkit, which generates the token, hands it over on the pipe
    /// it already holds, and configures its web view to answer with it. The
    /// token never appears in arguments, the environment, or any log.
    #[arg(long, hide = true)]
    auth_stdin: bool,
}

/// 64 MiB: generous for a form or an upload, and a ceiling on what one client
/// can make the proxy hold in memory.
const DEFAULT_MAX_REQUEST_BODY: u64 = 64 * 1024 * 1024;

/// What `syndeo-proxy` with no subcommand runs: `run`, with nothing given.
fn bare_run() -> RunArgs {
    RunArgs::try_parse_from(["syndeo-proxy"]).expect("run's defaults parse")
}

/// The one line `run --announce` writes to stdout, followed by the address it
/// is listening on and a newline. The number is the version of this record.
const READY: &str = "SYNDEO-PROXY-READY 1";

#[derive(Parser)]
struct CaArgs {
    /// Write the certificate to stdout instead of describing it.
    #[arg(long)]
    print: bool,
    /// Trust this authority for TLS, for this user only.
    ///
    /// Goes into the login keychain rather than the System one, so it needs no
    /// `sudo` and applies to nobody else who uses the machine. macOS does not
    /// ask before a trust setting goes into your own login keychain, so this
    /// asks instead: it changes nothing unless you type exactly `yes`, and
    /// there is deliberately no way to skip that from here.
    #[arg(long)]
    trust: bool,
    /// Remove the trust this added, and the certificate with it.
    #[arg(long)]
    untrust: bool,
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

/// Where the proxy's cache lives: `--cache` when it is given, and otherwise a
/// directory of the proxy's own under the Syndeo home.
///
/// Not `<home>/cache`, which is the network process's. redb holds its file
/// exclusively, so while syndeo-webkit's proxy had that one open, `syndeo
/// browse`, `syndeo-ui` and the agent could not start a network process at
/// all. `run` and `stats` both come through here, so they always agree.
fn cache_root(home: &std::path::Path, explicit: Option<&std::path::Path>) -> PathBuf {
    explicit
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| home.join("proxy").join("cache"))
}

/// What to say when the cache is locked. redb cannot say who holds it, so this
/// does not pretend to either.
fn already_open(root: &std::path::Path) -> anyhow::Error {
    anyhow::anyhow!(
        "the cache at {} is already open, and only one process can hold it at a time. \
         Another process may be using it: another syndeo-proxy, or the one syndeo-webkit \
         starts. To run alongside it, give this one a cache of its own with --cache <path>.",
        root.display()
    )
}

/// The statistics for the cache `stats` would read, rendered as it prints them.
fn stats_report(home: &std::path::Path, args: &StatsArgs) -> Result<String> {
    let root = cache_root(home, args.cache.as_deref());
    let cache = match syndeo_cache::Cache::open(&root) {
        Ok(cache) => cache,
        Err(syndeo_cache::CacheError::AlreadyOpen) => return Err(already_open(&root)),
        Err(err) => return Err(err.into()),
    };
    let stats = cache.stats()?;
    Ok(if args.json {
        serde_json::to_string_pretty(&stats)?
    } else {
        stats.render()
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Parsed first, because `--announce` decides where logs go: stdout then
    // belongs to the one line that says where the proxy listens.
    let announcing = matches!(&cli.command, Some(Command::Run(args)) if args.announce);
    let logs = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("syndeo_proxy=info,syndeo_net=info")
            }),
        )
        .with_target(false);
    if announcing {
        logs.with_writer(std::io::stderr).init();
    } else {
        logs.init();
    }

    // No subcommand is `run` with every default — the same defaults, from the
    // same definition, so the two can never drift apart again. (They had: the
    // bare command used the system resolver while `run` used DoH.)
    let command = match cli.command {
        Some(command) => command,
        None => Command::Run(bare_run()),
    };
    match command {
        Command::Run(args) => run(args).await,
        Command::Ca(args) => {
            let authority = CertificateAuthority::load_or_create(home().join("proxy"))?;
            if args.print {
                print!("{}", authority.certificate_pem());
                return Ok(());
            }
            let path = authority.certificate_path();

            if args.trust || args.untrust {
                return trust_in_login_keychain(&path, args.trust);
            }

            println!("authority certificate: {}", path.display());
            println!();
            println!();
            println!("Why this exists: caching HTTPS means terminating it, so the proxy");
            println!("presents a certificate it issued. A browser that does not know this");
            println!("issuer refuses every page.");
            println!();
            println!("Trust it for this user only — no sudo, nobody else on the machine:");
            println!("  syndeo-proxy ca --trust");
            println!("  syndeo-proxy ca --untrust      # and to undo it");
            println!();
            println!("What that costs, plainly: your user account will trust one more");
            println!("authority for TLS. It was generated on this machine and its key is at");
            println!("{}.", authority.key_path().display());
            println!("Anyone who takes that key can impersonate any site to you. Remove the");
            println!("trust when you are done, and keep the key as private as any other.");
            Ok(())
        }
        Command::Stats(args) => {
            println!("{}", stats_report(&home(), &args)?);
            Ok(())
        }
    }
}

struct Proxy {
    net: Net,
    authority: CertificateAuthority,
    trace: bool,
    /// The largest request body accepted; see `--max-request-body`.
    max_request_body: u64,
    /// The credential every client must present, when one was handed over.
    auth: Option<auth::ProxyAuth>,
    /// How long a tunnel may stay silent before its first byte says what it
    /// carries. Past it, the tunnel is closed without a certificate minted or
    /// an origin asked.
    tunnel_first_byte: std::time::Duration,
    /// Requests this proxy has been asked to handle, so a test can see that a
    /// loop stopped rather than only that an answer came back.
    #[cfg(test)]
    handled: std::sync::atomic::AtomicUsize,
}

async fn run(args: RunArgs) -> Result<()> {
    // The token frame is read first, synchronously and in full, while nothing
    // else is reading stdin; only then may the parent watch take it over.
    let auth = if args.auth_stdin {
        match auth::ProxyAuth::read_frame(&mut std::io::stdin().lock()) {
            Ok(auth) => Some(auth),
            Err(refusal) => {
                // Says what was wrong with the frame, never what was in it.
                eprintln!("syndeo-proxy: {refusal}");
                std::process::exit(2);
            }
        }
    } else {
        None
    };
    if args.exit_with_parent {
        syndeo_ipc::exit_when_parent_does();
    }
    let dns: DnsMode = args
        .dns
        .parse()
        .map_err(|e: String| anyhow::anyhow!("--dns: {e}"))?;

    let root = cache_root(&home(), args.cache.as_deref());
    let net = open_net(&root, args.shared, dns)?;

    let authority = CertificateAuthority::load_or_create(home().join("proxy"))?;
    let cert_path = authority.certificate_path();

    let proxy = Arc::new(Proxy {
        net,
        authority,
        trace: args.trace_requests,
        max_request_body: args.max_request_body,
        auth,
        tunnel_first_byte: TUNNEL_FIRST_BYTE,
        #[cfg(test)]
        handled: Default::default(),
    });

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    // Where it actually listens: with port 0 the system chose, and only the
    // bound socket knows which.
    let bound = listener.local_addr().context("reading the bound address")?;
    if args.announce {
        announce_ready(&mut std::io::stdout().lock(), bound)
            .context("announcing where the proxy listens")?;
    }

    tracing::info!(listen = %bound, cache = %proxy.net.config().cache_root.display(), "proxy up");
    tracing::info!(certificate = %cert_path.display(), "trust this to intercept https");
    tracing::info!(
        "statistics at http://syndeo.local/stats through the proxy, or `syndeo-proxy stats`"
    );

    serve(listener, proxy).await
}

/// Say where the proxy listens: one line, written only once it is bound, and
/// flushed at once, since the reader is waiting for it.
fn announce_ready(out: &mut impl std::io::Write, address: SocketAddr) -> std::io::Result<()> {
    writeln!(out, "{READY} {address}")?;
    out.flush()
}

/// The network process the proxy fetches through, on the cache at `root`.
fn open_net(root: &std::path::Path, shared: bool, dns: DnsMode) -> Result<Net> {
    Net::new(NetConfig {
        cache_root: root.to_path_buf(),
        shared_cache: shared,
        dns,
        ..NetConfig::default()
    })
    .map_err(|err| match err {
        syndeo_net::NetError::Cache(syndeo_cache::CacheError::AlreadyOpen) => already_open(root),
        other => anyhow::Error::new(other).context("starting the network process"),
    })
}

/// Answer every connection on `listener` for as long as the process lives.
///
/// Separate from `run` so a test can bind its own listener on a free port and
/// put a proxy on it, rather than going through the command line and 8899.
async fn serve(listener: TcpListener, proxy: Arc<Proxy>) -> Result<()> {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(%err, "accept failed");
                continue;
            }
        };
        // The address this connection reached, which is the one a request
        // naming this proxy as its own target would name.
        let local = match stream.local_addr() {
            Ok(address) => address,
            Err(err) => {
                tracing::debug!(%peer, %err, "no local address");
                continue;
            }
        };
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let proxy = proxy.clone();
                async move { handle(proxy, req, local).await }
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

/// The proxy's own response body.
///
/// Boxed rather than `Full<Bytes>`, because a response out of the network
/// process may still be arriving. A proxy that buffered every body before
/// forwarding it would measure a cache that nobody would ship: time-to-first-byte
/// is most of what a browser experiences, and holding it back would hide exactly
/// the thing the measurement is for.
// Unsync, because a streamed body is a boxed `Stream` and those are `Send`
// but not `Sync`. Hyper does not need it to be.
type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>;

fn whole(bytes: Bytes) -> Body {
    use http_body_util::BodyExt;
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn streaming(body: syndeo_net::FetchBody) -> Body {
    use futures::StreamExt;
    use http_body_util::{BodyExt, StreamBody};
    let frames = body.into_stream().map(|chunk| {
        chunk
            .map(hyper::body::Frame::data)
            .map_err(std::io::Error::other)
    });
    BodyExt::boxed_unsync(StreamBody::new(frames))
}

async fn handle(
    proxy: Arc<Proxy>,
    req: Request<Incoming>,
    local: SocketAddr,
) -> Result<Response<Body>, hyper::Error> {
    #[cfg(test)]
    proxy
        .handled
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // Every request that reaches the proxy itself — a CONNECT, a proxied
    // request, or one addressed to the proxy directly — answers for itself.
    // Requests inside a tunnel never come through here: the CONNECT that
    // opened the tunnel was the one that had to.
    if let Some(auth) = &proxy.auth {
        if !auth.admits(req.headers()) {
            return Ok(auth::required(&req));
        }
    }
    if req.method() == hyper::Method::CONNECT {
        return Ok(connect(proxy, req, local));
    }
    Ok(forward(proxy, req, None, local).await)
}

/// How long a new tunnel may stay silent before it is closed.
const TUNNEL_FIRST_BYTE: std::time::Duration = std::time::Duration::from_secs(10);

/// The first byte of every TLS handshake: a record of type handshake.
const TLS_HANDSHAKE_RECORD: u8 = 0x16;

/// `CONNECT host:port` — answer 200, take over the tunnel, and serve whatever
/// it carries: TLS, terminated with a leaf we mint for that host, or plain
/// HTTP/1, which is how WebKit's proxy setting sends every `http://` page.
///
/// What the tunnel carries is read from its first byte, not guessed from the
/// port: a TLS handshake always starts with one record type, and anything else
/// goes to an HTTP/1 parser, which refuses what is not HTTP. The scheme follows
/// from that — `https` for TLS, `http` otherwise — whatever port was named.
fn connect(proxy: Arc<Proxy>, req: Request<Incoming>, local: SocketAddr) -> Response<Body> {
    let Some(authority) = req.uri().authority().cloned() else {
        return text(StatusCode::BAD_REQUEST, "CONNECT needs an authority");
    };
    let host = authority.host().to_string();
    let Some(port) = authority.port_u16() else {
        return text(StatusCode::BAD_REQUEST, "CONNECT needs a port");
    };
    if via_names_us(req.headers()) || targets_this_proxy(&host, port, local) {
        return loop_detected();
    }

    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(err) => {
                tracing::debug!(%err, "upgrade failed");
                return;
            }
        };
        let mut io = TokioIo::new(upgraded);

        // Bounded: a tunnel that says nothing, or closes, costs no certificate
        // and no request to anyone.
        let mut first = [0u8; 1];
        let read = tokio::time::timeout(
            proxy.tunnel_first_byte,
            tokio::io::AsyncReadExt::read(&mut io, &mut first),
        )
        .await;
        match read {
            Ok(Ok(1)) => {}
            Ok(Ok(_)) => {
                tracing::debug!(%host, "tunnel closed before it carried anything");
                return;
            }
            Ok(Err(err)) => {
                tracing::debug!(%host, %err, "tunnel failed before it carried anything");
                return;
            }
            Err(_) => {
                tracing::debug!(%host, "tunnel carried nothing in time; closed");
                return;
            }
        }
        let io = Prefixed {
            first: Some(first[0]),
            io,
        };

        if first[0] == TLS_HANDSHAKE_RECORD {
            // Only now, with a handshake actually arriving, is a leaf minted.
            let config = match proxy.authority.server_config(&host) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(%host, %err, "could not mint a leaf certificate");
                    return;
                }
            };
            let acceptor = tokio_rustls::TlsAcceptor::from(config);
            let tls = match acceptor.accept(io).await {
                Ok(s) => s,
                Err(err) => {
                    tracing::debug!(%host, %err, "tls handshake failed");
                    return;
                }
            };
            let origin = tunnel_origin("https", &host, port);
            let service = tunnel_service(proxy, origin, local);
            if let Err(err) = ServerBuilder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service)
                .await
            {
                tracing::debug!(%host, %err, "tunnelled connection closed");
            }
        } else {
            let origin = tunnel_origin("http", &host, port);
            let service = tunnel_service(proxy, origin, local);
            // Keep-alive, so one tunnel carries as many requests as the browser
            // sends down it.
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(io), service)
                .await
            {
                tracing::debug!(%host, %err, "plain tunnelled connection closed");
            }
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .body(whole(Bytes::new()))
        .expect("static response")
}

/// The origin a tunnel reaches: the CONNECT authority, under the scheme the
/// tunnel turned out to carry, with the port left out only when it is that
/// scheme's default.
fn tunnel_origin(scheme: &str, host: &str, port: u16) -> String {
    let default = if scheme == "https" { 443 } else { 80 };
    if port == default {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

/// Every request inside a tunnel goes to the tunnel's origin and nowhere else.
fn tunnel_service(
    proxy: Arc<Proxy>,
    origin: String,
    local: SocketAddr,
) -> impl hyper::service::Service<
    Request<Incoming>,
    Response = Response<Body>,
    Error = hyper::Error,
    Future = impl std::future::Future<Output = Result<Response<Body>, hyper::Error>> + Send,
> + Clone {
    let origin = Arc::new(origin);
    service_fn(move |req| {
        let proxy = proxy.clone();
        let origin = origin.clone();
        async move { Ok::<_, hyper::Error>(forward(proxy, req, Some(origin.to_string()), local).await) }
    })
}

/// A stream with one byte already read from it, put back in front.
///
/// Deciding what a tunnel carries means reading its first byte, and whatever
/// then parses the stream — TLS or HTTP — has to see that byte too.
struct Prefixed<IO> {
    first: Option<u8>,
    io: IO,
}

impl<IO: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Prefixed<IO> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(byte) = self.first {
            if buf.remaining() > 0 {
                buf.put_slice(&[byte]);
                self.first = None;
                return std::task::Poll::Ready(Ok(()));
            }
        }
        std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<IO: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Prefixed<IO> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.io).poll_write(cx, data)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Turn one proxied request into a fetch, and the fetch back into a response.
async fn forward(
    proxy: Arc<Proxy>,
    req: Request<Incoming>,
    origin: Option<String>,
    local: SocketAddr,
) -> Response<Body> {
    // A request that has already been through this proxy is a loop, whatever
    // name it reached us by: `localtest.me`, `localhost`, or anything else
    // that resolves back here. Answered before anything is fetched, so a loop
    // costs one extra hop and not a machine's worth of sockets.
    if via_names_us(req.headers()) {
        return loop_detected();
    }

    let method = req.method().clone();
    // A request that is neither in proxy form nor inside a tunnel was sent to
    // the proxy as though it were the website. Only the statistics page is
    // meant to be reached that way.
    let direct = origin.is_none() && req.uri().authority().is_none();
    let url = match target_url(&req, origin.as_deref()) {
        Ok(u) => u,
        Err(refusal) => return text(StatusCode::BAD_REQUEST, refusal),
    };

    if let Some(response) = stats::intercept(&proxy.net, &url) {
        return response;
    }
    if direct {
        return text(
            StatusCode::BAD_REQUEST,
            "this is a proxy: configure it as one rather than requesting from it directly",
        );
    }
    if url_targets_this_proxy(&url, local) {
        return loop_detected();
    }

    let mut headers = forwardable(req.headers());
    append_our_via(&mut headers);

    // The body is read whole before it is sent on, so how much of it is read
    // is bounded: by what the client declares, and by what actually arrives.
    let declared = req
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|len| len > proxy.max_request_body) {
        return too_large(proxy.max_request_body);
    }
    let limit = usize::try_from(proxy.max_request_body).unwrap_or(usize::MAX);
    let body = match http_body_util::Limited::new(req.into_body(), limit)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(err) if err.is::<http_body_util::LengthLimitError>() => {
            return too_large(proxy.max_request_body)
        }
        Err(err) => {
            return text(
                StatusCode::BAD_REQUEST,
                &format!("reading the request body: {err}"),
            )
        }
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
        // Unpartitioned, deliberately. A proxied browser does not tell us which
        // tab a request came from either, so there is no top-level site to
        // partition under — and inventing one from the request's own host would
        // be the same as not partitioning while looking like it was. Hit rates
        // measured here are therefore an upper bound; the browser's own figures
        // are the partitioned ones.
        partition: None,
        // The browser follows its own redirects. Following them here would
        // answer the first URL with the destination's page, so the page would
        // run as the site that redirected to it, and any cookie the redirect
        // set would never reach the browser.
        redirect: RedirectMode::Manual,
    };

    match proxy.net.fetch(fetch).await {
        Ok(response) => {
            if proxy.trace {
                tracing::info!(
                    "{:>13}  {:>8}  {:>4}  {:>5}ms  {:>9}  {}",
                    response.source.as_str(),
                    response.protocol.as_str(),
                    response.status,
                    response.elapsed_ms,
                    match response.body.known_len() {
                        Some(len) => syndeo_cache::stats::human(len as u64),
                        None => "streaming".to_string(),
                    },
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
                // Provenance and transport are different questions, so they are
                // different headers.
                out.insert(
                    "x-syndeo-protocol",
                    http::HeaderValue::from_static(response.protocol.as_str()),
                );
            }
            builder
                .body(streaming(response.body))
                .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "malformed response"))
        }
        Err(err) => {
            tracing::warn!(%url, %err, "fetch failed");
            text(StatusCode::BAD_GATEWAY, &format!("{err}"))
        }
    }
}

/// The URL a request is for.
///
/// Proxied requests arrive in absolute form. Requests inside a CONNECT tunnel
/// arrive in origin form, and go to the tunnel's origin — the authority the
/// CONNECT named — and nowhere else. A request inside a tunnel that names an
/// absolute URL for another origin is refused rather than followed there, and
/// its `Host` header is never consulted: a tunnel opened to one site is not a
/// way to reach a second.
fn target_url(req: &Request<Incoming>, origin: Option<&str>) -> Result<String, &'static str> {
    let uri = req.uri();
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    if let Some(origin) = origin {
        if uri.authority().is_some() && !same_origin(uri, origin) {
            return Err("a request inside a tunnel may only be for the tunnel's own origin");
        }
        return Ok(format!("{origin}{path}"));
    }
    if uri.scheme().is_some() && uri.authority().is_some() {
        return Ok(uri.to_string());
    }
    let host = req
        .headers()
        .get(http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or("could not determine the target url")?;
    Ok(format!("http://{host}{path}"))
}

/// Whether an absolute-form request URI names the same origin as `origin`:
/// scheme and host ignoring ASCII case, and the port after defaults.
fn same_origin(uri: &hyper::Uri, origin: &str) -> bool {
    let Ok(origin) = origin.parse::<hyper::Uri>() else {
        return false;
    };
    let port = |u: &hyper::Uri| {
        u.port_u16().or_else(|| match u.scheme_str() {
            Some(s) if s.eq_ignore_ascii_case("https") => Some(443),
            Some(s) if s.eq_ignore_ascii_case("http") => Some(80),
            _ => None,
        })
    };
    let same = |a: Option<&str>, b: Option<&str>| match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    };
    same(uri.scheme_str(), origin.scheme_str())
        && same(uri.host(), origin.host())
        && port(uri).is_some()
        && port(uri) == port(&origin)
}

/// The name this proxy gives itself in `Via`: the received-by of RFC 9110
/// section 7.6.3, as a pseudonym rather than a host, so it names the software
/// and not the machine.
const VIA_PSEUDONYM: &str = "syndeo";

/// Whether any `Via` entry says the request has already passed through us.
///
/// Every `Via` field is read, each is split into its comma-separated entries,
/// and the received-by of each is compared with our pseudonym, ignoring ASCII
/// case. Only that exact token counts: `syndeo-proxy` is another proxy, and a
/// parenthesised comment is free text, so `(syndeo)` or `(x, 1.1 syndeo)` in
/// one is not us either.
fn via_names_us(headers: &http::HeaderMap) -> bool {
    headers
        .get_all(http::header::VIA)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(via_entries)
        .any(|entry| {
            let mut parts = entry.split_whitespace();
            let _received_protocol = parts.next();
            parts
                .next()
                .is_some_and(|received_by| received_by.eq_ignore_ascii_case(VIA_PSEUDONYM))
        })
}

/// The entries of one `Via` field, with comments removed. Commas separate
/// entries only outside parentheses, and a backslash quotes the next
/// character inside a comment.
fn via_entries(field: &str) -> Vec<String> {
    let mut entries = vec![String::new()];
    let mut depth = 0usize;
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            '\\' if depth > 0 => {
                chars.next();
            }
            ',' if depth == 0 => entries.push(String::new()),
            _ if depth == 0 => entries.last_mut().expect("never empty").push(c),
            _ => {}
        }
    }
    entries
}

/// Add this proxy to the end of the request's `Via` chain, keeping whatever
/// was already there, as one field.
fn append_our_via(headers: &mut http::HeaderMap) {
    let mut chain: Vec<u8> = Vec::new();
    for value in headers.get_all(http::header::VIA) {
        if !chain.is_empty() {
            chain.extend_from_slice(b", ");
        }
        chain.extend_from_slice(value.as_bytes());
    }
    if !chain.is_empty() {
        chain.extend_from_slice(b", ");
    }
    chain.extend_from_slice(b"1.1 ");
    chain.extend_from_slice(VIA_PSEUDONYM.as_bytes());
    let value = http::HeaderValue::from_bytes(&chain)
        .unwrap_or_else(|_| http::HeaderValue::from_static("1.1 syndeo"));
    headers.insert(http::header::VIA, value);
}

/// Whether a host and port, written as an address, are this proxy's own.
///
/// The quick check, for a target that names us literally. A name that
/// resolves to us is caught by `Via` instead, one hop later, because only
/// resolving it would say where it goes.
fn targets_this_proxy(host: &str, port: u16, local: SocketAddr) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    port == local.port() && (ip == local.ip() || ip.is_loopback() || ip.is_unspecified())
}

fn url_targets_this_proxy(url: &str, local: SocketAddr) -> bool {
    let Ok(uri) = url.parse::<hyper::Uri>() else {
        return false;
    };
    let (Some(host), Some(scheme)) = (uri.host(), uri.scheme_str()) else {
        return false;
    };
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    targets_this_proxy(host, port, local)
}

/// 413, and the connection closed: what is left of the body is unread, and
/// the connection could not be used for another request without reading it.
fn too_large(limit: u64) -> Response<Body> {
    let mut response = text(
        StatusCode::PAYLOAD_TOO_LARGE,
        &format!(
            "request bodies through this proxy are limited to {limit} bytes (--max-request-body)"
        ),
    );
    response.headers_mut().insert(
        http::header::CONNECTION,
        http::HeaderValue::from_static("close"),
    );
    response
}

fn loop_detected() -> Response<Body> {
    text(
        StatusCode::LOOP_DETECTED,
        "this request has already been through this proxy",
    )
}

fn text(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(whole(Bytes::from(message.to_string())))
        .expect("static response")
}

/// Trust, or stop trusting, the proxy's authority for this user.
///
/// The login keychain rather than the System one: no `sudo`, and no effect on
/// anyone else who uses the machine. Trust is set for the SSL policy only, so
/// this authority can vouch for a TLS server and for nothing else — not code
/// signing, not S/MIME, not a timestamp.
///
/// macOS does not ask before a trust setting goes into the user's own login
/// keychain, so the consent is ours: nothing changes unless the user types
/// exactly `yes`. There is no flag here to bypass it, because a browser that
/// can silently add a root to your machine is a browser you should not run.
fn trust_in_login_keychain(certificate: &std::path::Path, trust: bool) -> anyhow::Result<()> {
    use std::process::Command as Exec;

    let home = std::env::var("HOME").context("HOME is not set")?;
    let keychain = format!("{home}/Library/Keychains/login.keychain-db");

    if trust {
        // Asked here, because macOS does not ask. A trust setting in the user's
        // own login keychain goes in without a prompt, so a browser that ran
        // this silently would be adding a root to someone's machine without
        // telling them. The prompt is ours to put up.
        println!("This will add one certificate authority to your login keychain,");
        println!("trusted for TLS only, for your user only — no sudo, nobody else");
        println!("on this machine. It was generated here and its key is at");
        println!("{}.", certificate.with_extension("key").display());
        println!();
        println!("Anyone who takes that key can impersonate any website to you.");
        println!("Remove it with `syndeo-proxy ca --untrust` when you are done.");
        print!("Type 'yes' to continue: ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            println!("Nothing was trusted.");
            return Ok(());
        }
        let status = Exec::new("/usr/bin/security")
            .args([
                "add-trusted-cert",
                // User trust, not admin: the login keychain.
                "-r",
                // A self-signed authority is a root. `trustAsRoot` is for a
                // certificate that is not one, and macOS answers it with
                // "one or more parameters passed to a function were not valid".
                "trustRoot",
                // SSL and nothing else.
                "-p",
                "ssl",
                "-k",
                &keychain,
            ])
            .arg(certificate)
            .status()
            .context("running security add-trusted-cert")?;
        if !status.success() {
            anyhow::bail!("macOS declined or the authorisation was cancelled; nothing was trusted");
        }
        println!();
        println!("Trusted. Remove it with `syndeo-proxy ca --untrust` when you are done.");
    } else {
        // No `-d`: that is the admin domain, and `--trust` put this in the
        // user's own. Asking the wrong domain answers "the specified item
        // could not be found in the keychain" and leaves the trust setting
        // exactly where it was.
        let status = Exec::new("/usr/bin/security")
            .args(["remove-trusted-cert"])
            .arg(certificate)
            .status();
        // `remove-trusted-cert` fails when there was no trust setting to
        // remove, which is the state the caller asked for, so it is not an
        // error worth stopping on.
        match status {
            Ok(s) if s.success() => println!("Trust removed."),
            _ => println!("No trust setting to remove."),
        }
        let _ = Exec::new("/usr/bin/security")
            .args([
                "delete-certificate",
                "-c",
                "Syndeo Local Measurement CA",
                &keychain,
            ])
            .status();
        println!("The certificate is out of the login keychain.");
    }
    Ok(())
}

/// The headers that may be sent on to an origin.
///
/// Hop-by-hop headers belong to the connection the client made to *us*, and
/// forwarding them is a bug in any proxy. Over HTTP/2 it is not merely untidy:
/// RFC 9113 section 8.2.2 makes `Connection`, `Keep-Alive`, `Proxy-Connection`,
/// `Transfer-Encoding` and `Upgrade` illegal in a request, and a strict server
/// answers with a stream error instead of a page. Google does; GitHub does not,
/// which is why this presented for a long time as "Google is special" rather
/// than as a bug on this side.
///
/// `Host` goes too. HTTP/2 carries the authority in `:authority` and hyper sets
/// it from the URI, so a forwarded `Host` is a second copy of the same fact and
/// one more thing that can disagree with the first. The renderer bridge has
/// always stripped these; this path never did.
fn forwardable(incoming: &http::HeaderMap) -> http::HeaderMap {
    let tokens = syndeo_cache::headers::connection_tokens(incoming);
    let mut out = http::HeaderMap::new();
    for (name, value) in incoming.iter() {
        if name == http::header::HOST
            || syndeo_cache::headers::is_hop_by_hop(name.as_str(), &tokens)
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// One request as an origin saw it.
    #[derive(Clone, Debug)]
    struct Seen {
        path: String,
        headers: http::HeaderMap,
    }

    type Answer = Arc<dyn Fn(&str) -> Response<Full<Bytes>> + Send + Sync>;

    /// A plain-HTTP origin on a free loopback port that remembers every request.
    struct Origin {
        address: SocketAddr,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Origin {
        async fn start(answer: Answer) -> Origin {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let answer = answer.clone();
                    let log = log.clone();
                    tokio::spawn(async move {
                        let service = service_fn(move |req: Request<Incoming>| {
                            let answer = answer.clone();
                            let log = log.clone();
                            async move {
                                log.lock().unwrap().push(Seen {
                                    path: req.uri().path().to_string(),
                                    headers: req.headers().clone(),
                                });
                                Ok::<_, std::convert::Infallible>(answer(req.uri().path()))
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    });
                }
            });
            Origin { address, seen }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.address)
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn respond(
        status: u16,
        headers: &[(&str, &str)],
        body: &'static [u8],
    ) -> Response<Full<Bytes>> {
        let mut builder = Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Full::new(Bytes::from_static(body))).unwrap()
    }

    /// A real proxy on a free port, resolving through the system resolver so
    /// nothing here needs the internet.
    async fn start_proxy(dir: &std::path::Path) -> SocketAddr {
        start_proxy_with_count(dir).await.0
    }

    /// The same, keeping hold of the proxy so a test can ask how many
    /// requests it was asked to handle.
    async fn start_proxy_with_count(dir: &std::path::Path) -> (SocketAddr, Arc<Proxy>) {
        let net = open_net(&cache_root(dir, None), true, DnsMode::System).unwrap();
        let authority = CertificateAuthority::load_or_create(dir.join("proxy")).unwrap();
        let proxy = Arc::new(Proxy {
            net,
            authority,
            trace: false,
            max_request_body: 16,
            auth: None,
            tunnel_first_byte: std::time::Duration::from_millis(300),
            handled: Default::default(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, proxy.clone()));
        (address, proxy)
    }

    /// What came back through the proxy.
    struct Reply {
        status: StatusCode,
        headers: http::HeaderMap,
        body: Bytes,
    }

    /// Send one request through the proxy the way a browser configured to use
    /// it does: a connection to the proxy, with the full URL as the target.
    async fn through(proxy: SocketAddr, url: &str, headers: &[(&str, &str)]) -> Reply {
        let uri: hyper::Uri = url.parse().unwrap();
        let host = uri.authority().unwrap().to_string();
        send(proxy, hyper::Method::GET, url, &host, headers).await
    }

    /// Send any request target to the proxy: a full URL, an origin-form path,
    /// or a CONNECT authority.
    async fn send(
        proxy: SocketAddr,
        method: hyper::Method,
        target: &str,
        host: &str,
        headers: &[(&str, &str)],
    ) -> Reply {
        let stream = tokio::net::TcpStream::connect(proxy).await.unwrap();
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(connection.with_upgrades());
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .header(http::header::HOST, host);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = sender
            .send_request(request.body(Full::new(Bytes::new())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body,
        }
    }

    #[tokio::test]
    async fn a_redirect_reaches_the_browser_as_a_redirect() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|path| match path {
            "/start" => respond(
                302,
                &[
                    ("location", "/target"),
                    ("set-cookie", "a=1; Path=/"),
                    ("set-cookie", "b=2; Path=/"),
                ],
                b"moved",
            ),
            _ => respond(200, &[], b"the destination"),
        }))
        .await;
        let proxy = start_proxy(dir.path()).await;

        let reply = through(proxy, &origin.url("/start"), &[]).await;

        assert_eq!(reply.status, StatusCode::FOUND);
        assert_eq!(reply.headers.get("location").unwrap(), "/target");
        let cookies: Vec<&str> = reply
            .headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(cookies, ["a=1; Path=/", "b=2; Path=/"]);
        assert_eq!(&reply.body[..], b"moved");
        let paths: Vec<String> = origin.seen().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["/start"], "the destination must not be fetched");
    }

    fn cookies_of(reply: &Reply) -> Vec<String> {
        reply
            .headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_cookie_reaches_the_client_that_caused_it_and_no_later_one() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|path| match path {
            "/moved" => respond(
                301,
                &[
                    ("cache-control", "public, max-age=600"),
                    ("location", "/page"),
                    ("set-cookie", "hop=1; Path=/"),
                ],
                b"",
            ),
            _ => respond(
                200,
                &[
                    ("cache-control", "public, max-age=600"),
                    ("set-cookie", "a=1; Path=/"),
                    ("set-cookie", "b=2; Path=/"),
                ],
                b"shared page",
            ),
        }))
        .await;
        let proxy = start_proxy(dir.path()).await;

        let first = through(proxy, &origin.url("/page"), &[]).await;
        assert_eq!(cookies_of(&first), ["a=1; Path=/", "b=2; Path=/"]);
        let second = through(proxy, &origin.url("/page"), &[]).await;
        assert_eq!(&second.body[..], b"shared page");
        assert!(cookies_of(&second).is_empty(), "replayed to a later client");

        // A redirect the browser follows itself is cached as a redirect, and
        // the cookie it set is not handed to the next client either.
        let first = through(proxy, &origin.url("/moved"), &[]).await;
        assert_eq!(first.status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(cookies_of(&first), ["hop=1; Path=/"]);
        let second = through(proxy, &origin.url("/moved"), &[]).await;
        assert_eq!(second.status, StatusCode::MOVED_PERMANENTLY);
        assert!(cookies_of(&second).is_empty(), "replayed to a later client");

        let paths: Vec<String> = origin.seen().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["/page", "/moved"], "the second of each was a hit");
    }

    fn via(fields: &[&str]) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        for field in fields {
            headers.append(http::header::VIA, field.parse().unwrap());
        }
        headers
    }

    #[test]
    fn via_names_us_only_by_our_exact_received_by() {
        for fields in [
            &["1.1 syndeo"][..],
            &["HTTP/1.1 SynDeo"],
            &["1.0 fred, 1.1 syndeo"],
            &["1.0 fred", "1.1 SYNDEO (a comment after it)"],
            &["1.0 a (x, y), 1.1 syndeo"],
            &["  1.1   syndeo  "],
        ] {
            assert!(via_names_us(&via(fields)), "{fields:?} names us");
        }
        for fields in [
            &[][..],
            &["1.1 syndeo-proxy"],
            &["1.1 notsyndeo"],
            &["1.1 syndeox"],
            &["1.1 example (syndeo)"],
            &["1.1 example (x, 1.1 syndeo)"],
            &[r"1.1 example (a \) , 1.1 syndeo)"],
            &["syndeo"],
            &["1.1 syn deo"],
        ] {
            assert!(!via_names_us(&via(fields)), "{fields:?} does not name us");
        }
    }

    #[test]
    fn our_via_goes_on_the_end_of_the_chain() {
        let mut headers = via(&["1.0 fred", "1.1 upstream (squid)"]);
        append_our_via(&mut headers);
        let all: Vec<&str> = headers
            .get_all(http::header::VIA)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(all, ["1.0 fred, 1.1 upstream (squid), 1.1 syndeo"]);

        let mut empty = http::HeaderMap::new();
        append_our_via(&mut empty);
        assert_eq!(empty.get(http::header::VIA).unwrap(), "1.1 syndeo");
    }

    #[test]
    fn only_an_address_that_is_ours_counts_as_ourselves() {
        let local: SocketAddr = "127.0.0.1:8899".parse().unwrap();
        assert!(targets_this_proxy("127.0.0.1", 8899, local));
        assert!(targets_this_proxy("[::1]", 8899, local));
        assert!(targets_this_proxy("0.0.0.0", 8899, local));
        assert!(!targets_this_proxy("127.0.0.1", 8898, local));
        assert!(!targets_this_proxy("192.0.2.1", 8899, local));
        // A name is not an address; resolving it is the network's business,
        // and `Via` catches it when it comes back.
        assert!(!targets_this_proxy("localhost", 8899, local));
        assert!(url_targets_this_proxy("http://127.0.0.1:8899/x", local));
        assert!(!url_targets_this_proxy("http://127.0.0.1/x", local));
    }

    /// Bounded, or it is not a fix: every loop test has to finish well inside
    /// this.
    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(5), future)
            .await
            .expect("the proxy answered within five seconds")
    }

    fn handled(proxy: &Proxy) -> usize {
        proxy.handled.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn a_forwarded_request_carries_the_via_chain() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"ok"))).await;
        let proxy = start_proxy(dir.path()).await;

        let plain = through(proxy, &origin.url("/a"), &[]).await;
        let chained = through(proxy, &origin.url("/b"), &[("via", "1.0 upstream")]).await;

        assert_eq!(plain.status, StatusCode::OK);
        assert_eq!(chained.status, StatusCode::OK);
        let seen = origin.seen();
        assert_eq!(seen.len(), 2, "each request reached the origin once");
        assert_eq!(seen[0].headers.get("via").unwrap(), "1.1 syndeo");
        assert_eq!(
            seen[1].headers.get("via").unwrap(),
            "1.0 upstream, 1.1 syndeo"
        );
    }

    #[tokio::test]
    async fn a_request_that_has_been_through_us_is_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"ok"))).await;
        let proxy = start_proxy(dir.path()).await;

        for chain in [
            "1.1 syndeo",
            "1.1 SynDeo",
            "1.0 upstream, HTTP/1.1 syndeo (x)",
        ] {
            let reply = within(through(proxy, &origin.url("/x"), &[("via", chain)])).await;
            assert_eq!(reply.status, StatusCode::LOOP_DETECTED, "{chain}");
        }
        assert!(origin.seen().is_empty(), "nothing was fetched");
    }

    #[tokio::test]
    async fn a_near_miss_is_somebody_else() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"ok"))).await;
        let proxy = start_proxy(dir.path()).await;

        for chain in ["1.1 syndeo-proxy", "1.1 example (syndeo)"] {
            let reply = through(proxy, &origin.url("/x"), &[("via", chain)]).await;
            assert_eq!(reply.status, StatusCode::OK, "{chain}");
        }
        assert_eq!(origin.seen().len(), 2);
    }

    #[tokio::test]
    async fn asking_the_proxy_for_itself_by_address_stops_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (address, proxy) = start_proxy_with_count(dir.path()).await;

        let url = format!("http://127.0.0.1:{}/x", address.port());
        let reply = within(through(address, &url, &[])).await;

        assert_eq!(reply.status, StatusCode::LOOP_DETECTED);
        assert_eq!(handled(&proxy), 1, "refused before anything was fetched");
    }

    #[tokio::test]
    async fn asking_the_proxy_for_itself_by_name_stops_one_hop_later() {
        let dir = tempfile::tempdir().unwrap();
        let (address, proxy) = start_proxy_with_count(dir.path()).await;

        // A name that resolves back here: the literal check cannot see it, and
        // `Via` stops it when it arrives the second time.
        let url = format!("http://localhost:{}/x", address.port());
        let reply = within(through(address, &url, &[])).await;

        assert_eq!(reply.status, StatusCode::LOOP_DETECTED);
        assert_eq!(
            handled(&proxy),
            2,
            "one request from the client, one from ourselves"
        );
    }

    #[tokio::test]
    async fn the_statistics_page_and_connect_still_answer() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = start_proxy(dir.path()).await;

        let proxied = through(proxy, "http://syndeo.local/stats", &[]).await;
        assert_eq!(proxied.status, StatusCode::OK);
        let direct = send(proxy, hyper::Method::GET, "/stats", "syndeo.local", &[]).await;
        assert_eq!(direct.status, StatusCode::OK);

        let not_a_website = send(proxy, hyper::Method::GET, "/x", "example.test", &[]).await;
        assert_eq!(not_a_website.status, StatusCode::BAD_REQUEST);

        let tunnel = within(send(
            proxy,
            hyper::Method::CONNECT,
            "example.test:443",
            "example.test:443",
            &[],
        ))
        .await;
        assert_eq!(tunnel.status, StatusCode::OK);

        let ourselves = format!("127.0.0.1:{}", proxy.port());
        let refused = within(send(
            proxy,
            hyper::Method::CONNECT,
            &ourselves,
            &ourselves,
            &[],
        ))
        .await;
        assert_eq!(refused.status, StatusCode::LOOP_DETECTED);
    }

    /// Put `count` cacheable entries in the cache at `root`, and let go of it.
    fn seed(root: &std::path::Path, count: usize) {
        let cache = syndeo_cache::Cache::open(root).unwrap();
        let mut response = http::HeaderMap::new();
        response.insert("cache-control", "max-age=600".parse().unwrap());
        let now = cache.now();
        for i in 0..count {
            cache
                .store(
                    None,
                    "GET",
                    &format!("https://example.test/{i}"),
                    &http::HeaderMap::new(),
                    200,
                    &response,
                    format!("body {i}").as_bytes(),
                    now,
                    now,
                )
                .unwrap();
        }
    }

    fn entries(home: &std::path::Path, cache: Option<PathBuf>) -> u64 {
        let json = stats_report(home, &StatsArgs { cache, json: true }).unwrap();
        let stats: serde_json::Value = serde_json::from_str(&json).unwrap();
        stats["entries"].as_u64().unwrap()
    }

    #[test]
    fn the_proxy_cache_is_not_the_network_process_cache() {
        let home = std::path::Path::new("/home/someone/.syndeo");
        // `<home>/cache` is where syndeo-net keeps its own (net/main.rs).
        assert_eq!(cache_root(home, None), home.join("proxy").join("cache"));
        assert_ne!(cache_root(home, None), home.join("cache"));
        let explicit = std::path::Path::new("/elsewhere");
        assert_eq!(cache_root(home, Some(explicit)), explicit);
    }

    #[test]
    fn both_caches_can_be_open_at_once() {
        let home = tempfile::tempdir().unwrap();
        let network = syndeo_cache::Cache::open(home.path().join("cache")).unwrap();
        let proxy = syndeo_cache::Cache::open(cache_root(home.path(), None)).unwrap();
        assert!(network.stats().is_ok() && proxy.stats().is_ok());
    }

    #[test]
    fn stats_reads_the_proxy_cache_unless_told_otherwise() {
        let home = tempfile::tempdir().unwrap();
        seed(&cache_root(home.path(), None), 1);
        seed(&home.path().join("cache"), 3);
        let elsewhere = tempfile::tempdir().unwrap();
        seed(elsewhere.path(), 2);

        assert_eq!(entries(home.path(), None), 1);
        assert_eq!(
            entries(home.path(), Some(elsewhere.path().to_path_buf())),
            2
        );
    }

    #[test]
    fn a_cache_in_use_says_so_and_what_to_do() {
        let home = tempfile::tempdir().unwrap();
        let root = cache_root(home.path(), None);
        let _held = open_net(&root, true, DnsMode::System).unwrap();

        let from_run = open_net(&root, true, DnsMode::System)
            .err()
            .unwrap()
            .to_string();
        let from_stats = stats_report(
            home.path(),
            &StatsArgs {
                cache: None,
                json: false,
            },
        )
        .err()
        .unwrap()
        .to_string();
        for message in [from_run, from_stats] {
            assert!(message.contains(&root.display().to_string()), "{message}");
            assert!(message.contains("may be using it"), "{message}");
            assert!(message.contains("--cache"), "{message}");
        }
    }

    #[test]
    fn connection_headers_do_not_reach_the_origin() {
        // The exact shape that made every Google-operated host answer 502: a
        // proxied request carrying the client's connection headers into HTTP/2,
        // where they are a protocol error rather than an untidiness.
        let mut incoming = http::HeaderMap::new();
        for (name, value) in [
            ("host", "www.google.com"),
            ("proxy-connection", "keep-alive"),
            ("connection", "keep-alive, x-custom"),
            ("keep-alive", "timeout=5"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "websocket"),
            ("x-custom", "named by connection, so hop-by-hop too"),
            ("accept", "text/html"),
            ("user-agent", "syndeo-test"),
        ] {
            incoming.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }

        let out = forwardable(&incoming);

        for gone in [
            "host",
            "proxy-connection",
            "connection",
            "keep-alive",
            "transfer-encoding",
            "upgrade",
            "x-custom",
        ] {
            assert!(!out.contains_key(gone), "{gone} must not be forwarded");
        }
        assert_eq!(out.get("accept").unwrap(), "text/html");
        assert_eq!(out.get("user-agent").unwrap(), "syndeo-test");
    }

    // ------------------------------------------------ what a tunnel carries

    /// Open a CONNECT tunnel through the proxy by hand, as WebKit does, and
    /// return the stream and the status the proxy answered with.
    async fn open_tunnel(proxy: SocketAddr, authority: &str) -> (tokio::net::TcpStream, u16) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(proxy).await.unwrap();
        stream
            .write_all(
                format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(
                stream.read(&mut byte).await.unwrap(),
                1,
                "the proxy hung up"
            );
            head.push(byte[0]);
        }
        let status = String::from_utf8_lossy(&head)
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (stream, status)
    }

    /// Speak HTTP/1 inside an open tunnel.
    async fn http_over<S>(stream: S) -> hyper::client::conn::http1::SendRequest<Full<Bytes>>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(connection);
        sender
    }

    async fn get(
        sender: &mut hyper::client::conn::http1::SendRequest<Full<Bytes>>,
        target: &str,
        headers: &[(&str, &str)],
    ) -> Reply {
        let mut request = Request::builder().method(hyper::Method::GET).uri(target);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = within(sender.send_request(request.body(Full::new(Bytes::new())).unwrap()))
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body,
        }
    }

    #[tokio::test]
    async fn plain_http_through_a_tunnel_reaches_the_origin_and_the_tunnel_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|path| {
            respond(
                200,
                &[("cache-control", "max-age=600")],
                match path {
                    "/one" => b"first",
                    _ => b"second",
                },
            )
        }))
        .await;
        let (proxy, handle) = start_proxy_with_count(dir.path()).await;
        // A port that is not 80: what the tunnel carries decides the scheme.
        let authority = format!("localhost:{}", origin.address.port());

        let (stream, status) = open_tunnel(proxy, &authority).await;
        assert_eq!(status, 200);
        let mut sender = http_over(stream).await;

        let first = get(&mut sender, "/one", &[("host", &authority)]).await;
        assert_eq!(first.status, StatusCode::OK);
        assert_eq!(&first.body[..], b"first");
        // The same tunnel, a second request.
        let second = get(&mut sender, "/two", &[("host", &authority)]).await;
        assert_eq!(&second.body[..], b"second");
        assert!(second.headers.contains_key("x-syndeo-source"));

        let paths: Vec<String> = origin.seen().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["/one", "/two"]);
        assert_eq!(
            handle.authority.leaf_count(),
            0,
            "a certificate was minted for plain HTTP"
        );
    }

    #[tokio::test]
    async fn tls_through_a_tunnel_is_terminated_with_a_leaf_for_that_host() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"plain"))).await;
        let (proxy, handle) = start_proxy_with_count(dir.path()).await;
        let authority = format!("localhost:{}", origin.address.port());

        let (stream, status) = open_tunnel(proxy, &authority).await;
        assert_eq!(status, 200);

        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut handle.authority.certificate_pem().as_bytes()) {
            roots.add(cert.unwrap()).unwrap();
        }
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tls = within(tokio_rustls::TlsConnector::from(Arc::new(config)).connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            stream,
        ))
        .await
        .expect("the proxy should present a leaf for the tunnel's host");
        assert_eq!(handle.authority.leaf_count(), 1);

        // Inside TLS the scheme is https, whatever the port: the proxy asks the
        // origin over TLS, and this plain-HTTP origin cannot answer that.
        let mut sender = http_over(tls).await;
        let reply = get(&mut sender, "/secure", &[("host", &authority)]).await;
        assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
        assert!(
            origin.seen().is_empty(),
            "the request went out as plain HTTP"
        );
    }

    #[tokio::test]
    async fn a_tunnel_to_one_origin_cannot_be_used_to_reach_another() {
        let dir = tempfile::tempdir().unwrap();
        let first = Origin::start(Arc::new(|_| respond(200, &[], b"A"))).await;
        let second = Origin::start(Arc::new(|_| respond(200, &[], b"B"))).await;
        let proxy = start_proxy(dir.path()).await;
        let authority = format!("127.0.0.1:{}", first.address.port());

        let (stream, _) = open_tunnel(proxy, &authority).await;
        let mut sender = http_over(stream).await;

        // An absolute URL for B, inside a tunnel to A: refused.
        let refused = get(&mut sender, &second.url("/secret"), &[]).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);

        // A Host header naming B changes nothing: the request goes to A.
        let b_host = format!("127.0.0.1:{}", second.address.port());
        let reply = get(&mut sender, "/page", &[("host", &b_host)]).await;
        assert_eq!(&reply.body[..], b"A");

        // An absolute URL for A itself, in any case, is A.
        let same = format!("HTTP://127.0.0.1:{}/again", first.address.port());
        assert_eq!(&get(&mut sender, &same, &[]).await.body[..], b"A");

        assert!(
            second.seen().is_empty(),
            "the tunnel reached another origin"
        );
        let paths: Vec<String> = first.seen().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["/page", "/again"]);
    }

    #[tokio::test]
    async fn an_empty_or_silent_tunnel_closes_without_a_certificate_or_a_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"never"))).await;
        let (proxy, handle) = start_proxy_with_count(dir.path()).await;
        let authority = format!("localhost:{}", origin.address.port());

        // Closed at once.
        let (mut stream, _) = open_tunnel(proxy, &authority).await;
        stream.shutdown().await.unwrap();
        let mut rest = Vec::new();
        within(stream.read_to_end(&mut rest)).await.unwrap();

        // Silent past the proxy's patience (300ms in tests): the proxy closes it.
        let (mut stream, _) = open_tunnel(proxy, &authority).await;
        let mut byte = [0u8; 1];
        let read = within(stream.read(&mut byte)).await.unwrap();
        assert_eq!(read, 0, "the proxy should have closed a silent tunnel");

        assert_eq!(handle.authority.leaf_count(), 0);
        assert!(origin.seen().is_empty());
    }

    #[tokio::test]
    async fn what_is_not_http_or_tls_is_refused_by_the_parser() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"never"))).await;
        let (proxy, handle) = start_proxy_with_count(dir.path()).await;
        let authority = format!("localhost:{}", origin.address.port());

        let (mut stream, _) = open_tunnel(proxy, &authority).await;
        stream
            .write_all(b"\x00\x01garbage that is no protocol\r\n\r\n")
            .await
            .unwrap();
        // Refused: a 400 and a close, or a reset when the close finds bytes
        // the parser never read. Never anything forwarded.
        let mut answer = Vec::new();
        match within(stream.read_to_end(&mut answer)).await {
            Ok(_) => {
                let answer = String::from_utf8_lossy(&answer);
                assert!(
                    answer.is_empty() || answer.starts_with("HTTP/1.1 400"),
                    "{answer}"
                );
            }
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset),
        }
        assert_eq!(handle.authority.leaf_count(), 0);
        assert!(origin.seen().is_empty());
    }

    #[tokio::test]
    async fn a_loop_through_a_tunnel_is_still_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"never"))).await;
        let proxy = start_proxy(dir.path()).await;

        // To the proxy itself: refused at the CONNECT.
        let (_, status) = open_tunnel(proxy, &format!("127.0.0.1:{}", proxy.port())).await;
        assert_eq!(status, 508);

        // A request inside a tunnel that has already been through us.
        let authority = format!("127.0.0.1:{}", origin.address.port());
        let (stream, _) = open_tunnel(proxy, &authority).await;
        let mut sender = http_over(stream).await;
        let reply = get(&mut sender, "/x", &[("via", "1.1 syndeo")]).await;
        assert_eq!(reply.status, StatusCode::LOOP_DETECTED);
        assert!(origin.seen().is_empty());
    }

    // -------------------------------------------------------- authentication

    const TOKEN: [u8; 32] = [0x5a; 32];
    const STALE: [u8; 32] = [0xa5; 32];

    async fn start_proxy_with_auth(dir: &std::path::Path) -> (SocketAddr, Arc<Proxy>) {
        let net = open_net(&cache_root(dir, None), true, DnsMode::System).unwrap();
        let authority = CertificateAuthority::load_or_create(dir.join("proxy")).unwrap();
        let frame = auth::frame_for(&TOKEN);
        let proxy = Arc::new(Proxy {
            net,
            authority,
            trace: false,
            max_request_body: DEFAULT_MAX_REQUEST_BODY,
            auth: Some(auth::ProxyAuth::read_frame(&mut frame.as_slice()).unwrap()),
            tunnel_first_byte: std::time::Duration::from_millis(300),
            handled: Default::default(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, proxy.clone()));
        (address, proxy)
    }

    /// Write a request head and read the response head, on a socket the caller
    /// keeps — which is the point: a 407 is answered on the same connection.
    async fn exchange(stream: &mut tokio::net::TcpStream, head: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let mut byte = [0u8; 1];
        while !response.ends_with(b"\r\n\r\n") {
            let n = within(stream.read(&mut byte)).await.unwrap();
            assert_eq!(n, 1, "the proxy closed the connection");
            response.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&response).to_string();
        let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, text)
    }

    #[tokio::test]
    async fn a_407_is_answered_on_the_same_connection_and_the_tunnel_then_opens() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"inside"))).await;
        let (proxy, _) = start_proxy_with_auth(dir.path()).await;
        let authority = format!("127.0.0.1:{}", origin.address.port());
        let mut stream = tokio::net::TcpStream::connect(proxy).await.unwrap();

        let (status, head) = exchange(
            &mut stream,
            &format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n"),
        )
        .await;
        assert_eq!(status, 407);
        assert!(
            head.to_ascii_lowercase()
                .contains("proxy-authenticate: basic realm=\"syndeo\""),
            "{head}"
        );
        assert!(
            !head.to_ascii_lowercase().contains("connection: close"),
            "{head}"
        );

        // The same socket, with the credential, as WebKit retries.
        let credential = auth::header_for(&TOKEN);
        let (status, _) = exchange(
            &mut stream,
            &format!(
                "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Authorization: {credential}\r\n\r\n"
            ),
        )
        .await;
        assert_eq!(status, 200);

        // Inside the tunnel nothing is asked for again — and a credential sent
        // anyway never reaches the origin.
        let mut sender = http_over(stream).await;
        let reply = get(
            &mut sender,
            "/inside",
            &[("host", &authority), ("proxy-authorization", &credential)],
        )
        .await;
        assert_eq!(&reply.body[..], b"inside");
        let seen = origin.seen();
        assert_eq!(seen.len(), 1);
        assert!(!seen[0].headers.contains_key("proxy-authorization"));
    }

    #[tokio::test]
    async fn proxied_requests_and_the_statistics_page_need_the_credential_too() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"page"))).await;
        let (proxy, _) = start_proxy_with_auth(dir.path()).await;
        let credential = auth::header_for(&TOKEN);

        let refused = through(proxy, &origin.url("/page"), &[]).await;
        assert_eq!(refused.status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);
        let stats = send(proxy, hyper::Method::GET, "/stats", "syndeo.local", &[]).await;
        assert_eq!(stats.status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);
        assert!(origin.seen().is_empty());

        let admitted = through(
            proxy,
            &origin.url("/page"),
            &[("proxy-authorization", &credential)],
        )
        .await;
        assert_eq!(admitted.status, StatusCode::OK);
        let stats = send(
            proxy,
            hyper::Method::GET,
            "/stats",
            "syndeo.local",
            &[("proxy-authorization", &credential)],
        )
        .await;
        assert_eq!(stats.status, StatusCode::OK);

        let seen = origin.seen();
        assert_eq!(seen.len(), 1);
        assert!(
            !seen[0].headers.contains_key("proxy-authorization"),
            "the credential was forwarded to the origin"
        );
    }

    #[tokio::test]
    async fn a_stale_repeated_or_malformed_credential_reaches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"never"))).await;
        let (proxy, handle) = start_proxy_with_auth(dir.path()).await;
        let authority = format!("127.0.0.1:{}", origin.address.port());
        let right = auth::header_for(&TOKEN);
        // A token from an earlier launch — what a stale credential cached
        // somewhere would carry.
        let stale = auth::header_for(&STALE);

        for credentials in [
            vec![stale.as_str()],
            vec![right.as_str(), right.as_str()],
            vec!["Basic"],
            vec!["Bearer abc"],
        ] {
            let headers: Vec<(&str, &str)> = credentials
                .iter()
                .map(|c| ("proxy-authorization", *c))
                .collect();
            let tunnel = send(
                proxy,
                hyper::Method::CONNECT,
                &authority,
                &authority,
                &headers,
            )
            .await;
            assert_eq!(
                tunnel.status,
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "{credentials:?}"
            );
            let page = through(proxy, &origin.url("/x"), &headers).await;
            assert_eq!(
                page.status,
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "{credentials:?}"
            );
            let stats = send(
                proxy,
                hyper::Method::GET,
                "/stats",
                "syndeo.local",
                &headers,
            )
            .await;
            assert_eq!(
                stats.status,
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "{credentials:?}"
            );
            // Nothing in a refusal repeats the credential.
            for reply in [&tunnel, &page, &stats] {
                assert!(reply.body.is_empty());
                for value in reply.headers.values() {
                    assert!(!value.to_str().unwrap_or("").contains(&right[6..]));
                }
            }
        }
        assert!(
            origin.seen().is_empty(),
            "a refused request reached the origin"
        );
        assert_eq!(
            handle.authority.leaf_count(),
            0,
            "a refused CONNECT opened a tunnel"
        );
    }

    #[tokio::test]
    async fn a_refused_upload_closes_its_connection_rather_than_leaving_the_body_unread() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"never"))).await;
        let (proxy, _) = start_proxy_with_auth(dir.path()).await;
        let mut stream = tokio::net::TcpStream::connect(proxy).await.unwrap();
        let url = origin.url("/upload");
        let authority = format!("127.0.0.1:{}", origin.address.port());
        let (status, head) = exchange(
            &mut stream,
            &format!(
                "POST {url} HTTP/1.1\r\nHost: {authority}\r\nContent-Length: 11\r\n\r\nhello world"
            ),
        )
        .await;
        assert_eq!(status, 407);
        assert!(
            head.to_ascii_lowercase().contains("connection: close"),
            "{head}"
        );
        let mut rest = Vec::new();
        let _ = within(stream.read_to_end(&mut rest)).await;
        assert!(
            rest.is_empty(),
            "the connection was kept for another request"
        );
        assert!(origin.seen().is_empty());
    }

    // ------------------------------------------------------------ flags

    fn run_args(args: &[&str]) -> RunArgs {
        let mut all = vec!["syndeo-proxy", "run"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all).unwrap().command {
            Some(Command::Run(args)) => args,
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn on_by_default_flags_can_be_turned_off_and_still_be_named_bare() {
        let defaults = run_args(&[]);
        assert!(defaults.shared && defaults.trace_requests);
        assert!(run_args(&["--shared"]).shared);
        assert!(!run_args(&["--shared=false"]).shared);
        assert!(!run_args(&["--shared", "false"]).shared);
        assert!(run_args(&["--trace-requests"]).trace_requests);
        assert!(!run_args(&["--trace-requests=false"]).trace_requests);
        // A bare flag followed by another option does not swallow it.
        let both = run_args(&["--shared", "--dns", "system"]);
        assert!(both.shared);
        assert_eq!(both.dns, "system");
        assert!(Cli::try_parse_from(["syndeo-proxy", "run", "--shared=maybe"]).is_err());
    }

    #[test]
    fn the_bare_command_is_run_with_runs_own_defaults() {
        assert_eq!(bare_run(), run_args(&[]));
        assert_eq!(bare_run().dns, "doh:cloudflare");
        assert_eq!(bare_run().listen, "127.0.0.1:8899".parse().unwrap());
    }

    // ----------------------------------------------------- request bodies

    /// Send `body` through the proxy with `method`, declaring its length or
    /// streaming it chunked, and return the status and whether the proxy
    /// closed the connection after answering.
    async fn upload(proxy: SocketAddr, url: &str, body: &[u8], chunked: bool) -> (u16, String) {
        use tokio::io::AsyncWriteExt;
        let mut stream = tokio::net::TcpStream::connect(proxy).await.unwrap();
        let authority = url.split('/').nth(2).unwrap();
        let framing = if chunked {
            "Transfer-Encoding: chunked".to_string()
        } else {
            format!("Content-Length: {}", body.len())
        };
        let (status, head) = if chunked {
            stream
                .write_all(
                    format!("POST {url} HTTP/1.1\r\nHost: {authority}\r\n{framing}\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut chunk = format!("{:x}\r\n", body.len()).into_bytes();
            chunk.extend_from_slice(body);
            chunk.extend_from_slice(b"\r\n0\r\n\r\n");
            exchange(&mut stream, std::str::from_utf8(&chunk).unwrap()).await
        } else {
            let mut request =
                format!("POST {url} HTTP/1.1\r\nHost: {authority}\r\n{framing}\r\n\r\n");
            request.push_str(std::str::from_utf8(body).unwrap());
            exchange(&mut stream, &request).await
        };
        (status, head)
    }

    #[tokio::test]
    async fn a_request_body_past_the_limit_is_refused_before_anything_is_sent_on() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin::start(Arc::new(|_| respond(200, &[], b"received"))).await;
        // The test proxy's limit is 16 bytes.
        let proxy = start_proxy(dir.path()).await;
        let url = origin.url("/upload");

        let (status, _) = upload(proxy, &url, b"sixteen bytes!!!", false).await;
        assert_eq!(status, 200, "a body at the limit goes through");

        for chunked in [false, true] {
            let (status, head) = upload(proxy, &url, b"seventeen bytes!!", chunked).await;
            assert_eq!(status, 413, "chunked: {chunked}");
            assert!(
                head.to_ascii_lowercase().contains("connection: close"),
                "{head}"
            );
        }
        assert_eq!(
            origin.seen().len(),
            1,
            "a refused upload reached the origin"
        );
    }

    #[test]
    fn the_request_body_limit_is_a_documented_flag() {
        assert_eq!(run_args(&[]).max_request_body, 64 * 1024 * 1024);
        assert_eq!(
            run_args(&["--max-request-body", "1048576"]).max_request_body,
            1_048_576
        );
        use clap::CommandFactory;
        let help = Cli::command()
            .find_subcommand_mut("run")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("--max-request-body"), "{help}");
    }
}
