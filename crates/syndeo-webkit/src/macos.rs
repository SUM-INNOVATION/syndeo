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

use crate::pin;
use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::EventLoop;
use winit::keyboard::Key;
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
    /// The proxy's authority, trusted by this process and nowhere else.
    ///
    /// Required with `--proxy`, because caching HTTPS means terminating it and
    /// a web view that does not know this issuer refuses every page.
    #[arg(long, value_name = "PEM")]
    proxy_ca: Option<PathBuf>,
    /// Start any video on the page, muted.
    ///
    /// WebKit blocks autoplay with sound, which is correct behaviour and makes
    /// "does media work" unanswerable from a script: the page loads, the player
    /// is ready, and nothing plays because nothing clicked. This asks it to,
    /// so playback can be measured rather than assumed.
    #[arg(long)]
    autoplay: bool,
    /// Where the proxy is listening. Everything this renders goes through it.
    ///
    /// Optional only so the two halves can be told apart when something does
    /// not load: without it this is an ordinary web view, and if that fails too
    /// then the containment was never the problem.
    #[arg(long)]
    proxy: Option<String>,
}

pub fn run() -> Result<()> {
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

    let pinned = match (&proxy, &cli.proxy_ca) {
        (Some(_), Some(path)) => {
            let pem = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            tracing::info!(authority = %path.display(), "trusting the proxy's authority, and no other");
            Some(pem)
        }
        (Some(_), None) => {
            tracing::warn!(
                "no --proxy-ca: every https page will be refused, because the proxy \
                 terminates TLS and presents its own certificate"
            );
            None
        }
        _ => None,
    };

    let event_loop = EventLoop::new().context("creating the event loop")?;
    let mut app = App {
        url: cli.url,
        proxy,
        pinned,
        state: None,
        modifiers: winit::keyboard::ModifiersState::empty(),
        autoplay: cli.autoplay,
    };
    event_loop
        .run_app(&mut app)
        .context("the event loop failed")?;
    Ok(())
}

struct Running {
    window: Window,
    /// One child web view per tab.
    ///
    /// Children rather than one view replacing the window's own, which is both
    /// what makes tabs possible and what stops the browser crashing: wry's
    /// `build()` swaps out the window's NSView, and winit's window delegate
    /// then holds a weak reference to a view that no longer exists. The first
    /// time the window lost focus it dereferenced it and died in
    /// `objc_loadWeakRetained`. `build_as_child` leaves winit's view alone.
    tabs: Vec<WebView>,
    active: usize,
    /// Held for as long as the web views are: a delegate that has been dropped
    /// is a web view that trusts nothing and renders nothing.
    _pinner: Option<objc2::rc::Retained<pin::Pinner>>,
}

impl Running {
    /// The area a tab's view should fill.
    fn content(&self) -> wry::Rect {
        let size = self.window.inner_size();
        wry::Rect {
            position: winit::dpi::PhysicalPosition::new(0, 0).into(),
            size: winit::dpi::PhysicalSize::new(size.width, size.height).into(),
        }
    }

    /// Show one tab and hide the rest.
    ///
    /// Hidden rather than destroyed, which is the point of a tab: the page
    /// keeps its scroll position, its playing video and its JavaScript heap,
    /// and switching back costs nothing. It is also where the memory goes, so
    /// the count is logged.
    fn show(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        let bounds = self.content();
        for (i, tab) in self.tabs.iter().enumerate() {
            let visible = i == index;
            let _ = tab.set_visible(visible);
            if visible {
                let _ = tab.set_bounds(bounds);
                let _ = tab.focus();
            }
        }
        tracing::info!(tab = index + 1, of = self.tabs.len(), "showing");
    }
}

struct App {
    url: String,
    proxy: Option<(String, String)>,
    pinned: Option<String>,
    state: Option<Running>,
    modifiers: winit::keyboard::ModifiersState,
    autoplay: bool,
}

/// Find a video and start it, muted, however late it appears.
///
/// Polled rather than run once: a player built by script is not in the document
/// when the document finishes loading, and on a page like YouTube the element
/// arrives seconds later.
const AUTOPLAY: &str = r#"
(function () {
  var tries = 0;
  var timer = setInterval(function () {
    var v = document.querySelector('video');
    if (v) {
      v.muted = true;
      var p = v.play();
      if (p && p.catch) { p.catch(function (e) { console.log('play refused: ' + e); }); }
      clearInterval(timer);
    } else if (++tries > 60) {
      clearInterval(timer);
    }
  }, 500);
})();
"#;

impl App {
    /// A new tab, sharing everything the others share.
    ///
    /// The same process, so it costs one document rather than a whole browser:
    /// Servo's model was a process per page and paid its fixed cost every
    /// time, which is the whole reason a second window cost as much as the
    /// first.
    fn open_tab(&mut self) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let mut builder = WebViewBuilder::new()
            .with_bounds(state.content())
            .with_url("about:blank");
        if let Some((host, port)) = &self.proxy {
            builder = builder.with_proxy_config(wry::ProxyConfig::Http(wry::ProxyEndpoint {
                host: host.clone(),
                port: port.clone(),
            }));
        }
        match builder.build_as_child(&state.window) {
            Ok(tab) => {
                if let Some(pinner) = &state._pinner {
                    use wry::WebViewExtMacOS;
                    let delegate = pin::as_delegate(pinner);
                    unsafe { tab.webview().setNavigationDelegate(Some(&delegate)) };
                }
                state.tabs.push(tab);
                let last = state.tabs.len() - 1;
                state.show(last);
            }
            Err(err) => tracing::error!(%err, "could not open a tab"),
        }
    }

    /// Close the visible tab, and the window with the last of them.
    fn close_tab(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.tabs.len() <= 1 {
            event_loop.exit();
            return;
        }
        let closing = state.active;
        state.tabs.remove(closing);
        let next = closing.min(state.tabs.len() - 1);
        state.show(next);
    }

    fn cycle(&mut self, by: isize) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let count = state.tabs.len() as isize;
        if count == 0 {
            return;
        }
        let next = (state.active as isize + by).rem_euclid(count);
        state.show(next as usize);
    }
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
            .with_initialization_script(if self.autoplay { AUTOPLAY } else { "" })
            .with_bounds(wry::Rect {
                position: winit::dpi::PhysicalPosition::new(0, 0).into(),
                size: winit::dpi::PhysicalSize::new(
                    window.inner_size().width,
                    window.inner_size().height,
                )
                .into(),
            })
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
        let webview = match builder.build_as_child(&window) {
            Ok(webview) => webview,
            Err(err) => {
                tracing::error!(%err, "the web view could not be created");
                event_loop.exit();
                return;
            }
        };

        // Set after the web view exists, because it is the web view's own
        // delegate. This replaces wry's, which is why the navigation handler
        // above is the last thing registered through wry rather than the first.
        let pinner = match &self.pinned {
            Some(pem) => match objc2::MainThreadMarker::new()
                .ok_or_else(|| anyhow::anyhow!("not on the main thread"))
                .and_then(|mtm| pin::Pinner::new(mtm, pem))
            {
                Ok(pinner) => {
                    use wry::WebViewExtMacOS;
                    let native = webview.webview();
                    let delegate = pin::as_delegate(&pinner);
                    unsafe { native.setNavigationDelegate(Some(&delegate)) };
                    Some(pinner)
                }
                Err(err) => {
                    tracing::error!(%err, "could not read the proxy's authority");
                    None
                }
            },
            None => None,
        };

        let mut running = Running {
            window,
            tabs: vec![webview],
            active: 0,
            _pinner: pinner,
        };
        running.show(0);
        self.state = Some(running);
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            // A child view does not resize with its window; it has to be told.
            WindowEvent::Resized(_) => {
                let bounds = state.content();
                if let Some(tab) = state.tabs.get(state.active) {
                    let _ = tab.set_bounds(bounds);
                }
            }

            WindowEvent::ModifiersChanged(changed) => {
                self.modifiers = changed.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != winit::event::ElementState::Pressed {
                    return;
                }
                // Command on macOS, Control elsewhere — the shortcut everyone
                // already has in their fingers on the platform they are on.
                let accel = if cfg!(target_os = "macos") {
                    self.modifiers.super_key()
                } else {
                    self.modifiers.control_key()
                };
                if !accel {
                    return;
                }
                match &event.logical_key {
                    Key::Character(c) if c == "t" => self.open_tab(),
                    Key::Character(c) if c == "w" => self.close_tab(event_loop),
                    Key::Character(c) if c == "]" => self.cycle(1),
                    Key::Character(c) if c == "[" => self.cycle(-1),
                    Key::Character(c) => {
                        if let Some(n) = c.chars().next().and_then(|c| c.to_digit(10)) {
                            if n >= 1 {
                                if let Some(state) = self.state.as_mut() {
                                    state.show(n as usize - 1);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
