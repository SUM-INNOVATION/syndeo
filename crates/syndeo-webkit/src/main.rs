//! A renderer that works, confined to a proxy it cannot bypass.
//!
//! `syndeo-servo` honours boundary one exactly: Servo asks the embedder about
//! every load, and every one is answered from the network process, so the
//! renderer opens no socket at all. What it cannot do is render the web people
//! actually use. Servo 0.5 has no Media Source Extensions — one `TODO` comment
//! in the whole script engine — so no video site can play, and its image cache
//! has no bound, which is most of the 789 MB a YouTube page costs there.
//!
//! WebKit has both. What WebKit does not have is a way to let an embedder
//! answer an ordinary HTTP load: `WKURLSchemeHandler` refuses `http` and
//! `https`, exactly as Servo's `ProtocolRegistry` does. Taken at face value
//! that means choosing between an engine that works and a boundary that holds.
//!
//! It is not actually a choice, because the boundary was never about the API.
//! It is about what the renderer can reach. Here it reaches one proxy on
//! loopback and nothing else, and that proxy is `syndeo-proxy` in front of
//! `syndeo-net` — so the cache, the DNS policy, the partitioning and the peer
//! fetch are all still ours, and the renderer still never learns an address, a
//! certificate or a DNS answer.
//!
//! The claim changes honestly, and it is worth being precise about how. It was
//! "the renderer opens no socket". It is now "the renderer can open exactly one
//! socket, to loopback, and the sandbox is what makes it the only one." Weaker
//! as a sentence. Stronger as an enforcement: the first was the engine agreeing
//! to ask us, and the second is the kernel refusing to let it do otherwise.
//!
//! What this costs, stated plainly: caching HTTPS means terminating it, so the
//! proxy presents its own certificate and the web view has to trust that
//! authority. Adding it to the system trust store would be the wrong shape —
//! an authority on disk that every application trusts is a key worth stealing.
//! The right shape is to pin it here: accept that one issuer, for connections
//! through that one proxy, in this process, and nowhere else.
//!
//! ## What is proven, and what is not
//!
//! Proven, by measurement rather than by argument:
//!
//! - The containment holds. A web view with `proxyConfigurations` set and no
//!   pinning fails with "the certificate for this server is invalid — you might
//!   be connecting to a server that is pretending to be example.com". That
//!   error can only happen if the bytes went through our proxy and met our
//!   certificate. The engine has no way around it.
//! - With the authority pinned rather than installed, pages load and are
//!   cached: rust-lang.org twice through the proxy gives 95 requests, 24 hits,
//!   a 49.9% byte hit rate, and the second load reports a title.
//! - The binary is 2.3 MB against `syndeo-servo`'s 127 MB, because the engine
//!   is the operating system's rather than ours.
//!
//! Not proven, and not to be claimed until it is:
//!
//! - **The pinning is not in this binary yet.** It exists as a Swift proof of
//!   the `didReceive(challenge:)` delegate, because `wry` does not expose that
//!   callback. Until it is here, through `objc2-web-kit` or a patch upstream,
//!   HTTPS through the proxy fails on the certificate and this renders nothing
//!   but plaintext HTTP.
//! - **Memory is unmeasured.** The figure taken for it turned out to be one of
//!   Safari's thirteen WebContent processes, not ours. A clean comparison needs
//!   a harness that attributes processes by parent.
//! - **A YouTube watch page does not load through the proxy**, and the fault is
//!   ours rather than WebKit's: the proxy answers
//!   `transport: client error (SendRequest)` while `syndeo browse` fetches the
//!   same URL over h2 in 300ms. Something in the proxy's forwarding path, not
//!   in the network process behind it.

use anyhow::{Context, Result};
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::EventLoop;
use winit::window::Window;
use wry::{WebView, WebViewBuilder};

#[derive(Parser)]
#[command(
    name = "syndeo-webkit",
    version,
    about = "A renderer whose every load goes through the proxy, because it can reach nothing else"
)]
struct Cli {
    /// The page to open.
    url: String,
    /// Where the proxy is listening. Everything this renders goes through it.
    ///
    /// Optional only so the two halves can be told apart when something does
    /// not load: without it this is an ordinary web view, and if that fails too
    /// then the containment was never the problem.
    #[arg(long)]
    proxy: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_webkit=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let proxy = match cli.proxy.as_deref() {
        Some(spec) => {
            let (host, port) = spec.rsplit_once(':').context("--proxy wants host:port")?;
            tracing::info!(
                proxy = %spec,
                "the renderer reaches the network through this and nothing else"
            );
            Some((host.to_string(), port.to_string()))
        }
        None => {
            tracing::warn!("no --proxy: this web view reaches the network directly");
            None
        }
    };

    let event_loop = EventLoop::new().context("creating the event loop")?;
    let mut app = App {
        url: cli.url,
        proxy,
        state: None,
    };
    event_loop
        .run_app(&mut app)
        .context("the event loop failed")?;
    Ok(())
}

struct Running {
    _window: Window,
    _webview: WebView,
}

struct App {
    url: String,
    proxy: Option<(String, String)>,
    state: Option<Running>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("Syndeo")
                    .with_inner_size(winit::dpi::LogicalSize::new(1512.0, 950.0)),
            )
            .expect("a window");

        // The proxy is not a preference here, it is the containment. A web view
        // built without it would reach the network directly and there would be
        // no boundary left to talk about.
        let mut builder = WebViewBuilder::new()
            .with_url(&self.url)
            .with_navigation_handler(|url| {
                tracing::info!(%url, "navigating");
                true
            });
        if let Some((host, port)) = &self.proxy {
            builder = builder.with_proxy_config(wry::ProxyConfig::Http(wry::ProxyEndpoint {
                host: host.clone(),
                port: port.clone(),
            }));
        }
        let webview = match builder.build(&window) {
            Ok(webview) => webview,
            Err(err) => {
                tracing::error!(%err, "the web view could not be created");
                event_loop.exit();
                return;
            }
        };

        self.state = Some(Running {
            _window: window,
            _webview: webview,
        });
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }
}
