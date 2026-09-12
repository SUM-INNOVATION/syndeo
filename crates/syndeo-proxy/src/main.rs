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
    #[arg(long, default_value = "doh:cloudflare")]
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
    /// Trust this authority for TLS, for this user only.
    ///
    /// Goes into the login keychain rather than the System one, so it needs no
    /// `sudo` and applies to nobody else who uses the machine. macOS will ask
    /// you to authorise the change; that prompt is the consent, and there is
    /// deliberately no way to skip it from here.
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

fn cache_root(override_path: &Option<PathBuf>) -> PathBuf {
    override_path
        .clone()
        .unwrap_or_else(|| home().join("cache"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("syndeo_proxy=info,syndeo_net=info")
            }),
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
    tracing::info!(
        "statistics at http://syndeo.local/stats through the proxy, or `syndeo-proxy stats`"
    );

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

        let origin = Arc::new(format!(
            "https://{host}{}",
            if port == 443 {
                String::new()
            } else {
                format!(":{port}")
            }
        ));
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
        .body(whole(Bytes::new()))
        .expect("static response")
}

/// Turn one proxied request into a fetch, and the fetch back into a response.
async fn forward(
    proxy: Arc<Proxy>,
    req: Request<Incoming>,
    origin: Option<String>,
) -> Response<Body> {
    let method = req.method().clone();
    let url = match absolute_url(&req, origin.as_deref()) {
        Some(u) => u,
        None => {
            return text(
                StatusCode::BAD_REQUEST,
                "could not determine the target url",
            )
        }
    };

    if let Some(response) = stats::intercept(&proxy.net, &url) {
        return response;
    }

    let headers = forwardable(req.headers());

    let body = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
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
/// macOS puts up its own authorisation dialog for a trust-setting change, and
/// that is the consent. There is no flag here to bypass it, because a browser
/// that can silently add a root to your machine is a browser you should not run.
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
}
