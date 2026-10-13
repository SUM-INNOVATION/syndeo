//! A renderer that works, configured to send its traffic through
//! syndeo-proxy, with the proxy's certificate pinned.
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
//! `https`, exactly as Servo's `ProtocolRegistry` does.
//!
//! So the web view is given a proxy. Its data store is configured to send its
//! traffic through `syndeo-proxy` on loopback, in front of `syndeo-net`, so the
//! cache, the DNS policy and the peer fetch are still ours.
//!
//! Precisely what that is: a configuration WebKit's networking process honours
//! for the loads it makes, not a sandbox Syndeo controls and not a rule the
//! kernel enforces. Servo's claim was "the renderer opens no socket"; this one
//! is only that the web view is configured to use the proxy. Anything WebKit
//! does not send through that setting is not covered by it. Requests for
//! localhost and loopback addresses are one such case, observed: WebKit sent
//! them directly and never to the proxy. WebRTC's own transports are another
//! candidate, not yet measured.
//!
//! Caching HTTPS means terminating it, so the proxy presents certificates it
//! issued, and two things make WebKit accept them. The authority is trusted
//! for TLS in this user's login keychain (`syndeo-proxy ca --trust`, which asks
//! first), because WebKit validates subresources in its networking process
//! against the user's trust settings and nothing in ours. And this process pins
//! it: a server-trust challenge in the web view is evaluated with that one
//! authority as the only anchor, so a certificate from anyone else is refused.
//! See `pin_macos.rs`.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use syndeo_webkit::owned::{authority_is_trusted, default_home, start_proxy};
use syndeo_webkit::{pin, proxied};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::EventLoop;
use winit::keyboard::Key;
use winit::window::Window;
use wry::{WebView, WebViewBuilder};

#[derive(Parser, Clone)]
#[command(
    name = "syndeo-webkit",
    version,
    about = "A renderer configured to send its traffic through syndeo-proxy, with the proxy's certificate pinned",
    long_about = "A renderer configured to send its traffic through syndeo-proxy, with the \
                  proxy's certificate pinned.\n\n\
                  The proxy is a setting WebKit honours, not a boundary Syndeo enforces. \
                  WebKit was seen to send requests for localhost and loopback addresses \
                  directly, never to the proxy, and transports it does not send through the \
                  setting, such as WebRTC's, are not covered."
)]
struct Cli {
    /// The page to open.
    url: String,
    /// The proxy's authority.
    ///
    /// Defaults to the one belonging to the proxy this starts, so it does not
    /// normally need giving.
    #[arg(long, value_name = "PEM")]
    proxy_ca: Option<PathBuf>,
    /// Where the cache, the keys and the authority live.
    #[arg(long)]
    home: Option<PathBuf>,
    /// Start any video on the page, muted.
    ///
    /// WebKit blocks autoplay with sound, which is correct behaviour and makes
    /// "does media work" unanswerable from a script: the page loads, the player
    /// is ready, and nothing plays because nothing clicked. This asks it to,
    /// so playback can be measured rather than assumed.
    #[arg(long)]
    autoplay: bool,
    /// Where the proxy is listening. The web view is configured to send its
    /// HTTP and HTTPS traffic through it, except for localhost and loopback
    /// addresses, which WebKit was seen to reach directly.
    ///
    /// Optional only so the two halves can be told apart when something does
    /// not load: without it this is an ordinary web view, and if that fails too
    /// then the proxy was never the problem.
    #[arg(long)]
    proxy: Option<String>,
}

pub fn run(install: syndeo_shell::supervisor::InstallDir) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_webkit=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let home = cli.home.clone().unwrap_or_else(default_home);

    let authority = cli
        .proxy_ca
        .clone()
        .unwrap_or_else(|| home.join("proxy").join("syndeo-ca.pem"));

    // First, before anything is started: the failure without this is every page
    // refusing to load with nothing said about why, because WebKit validates
    // subresources in its networking process and that does not consult us.
    if !authority_is_trusted(&authority) {
        eprintln!(
            "The proxy's authority is not trusted yet, so every https page would be\n\
             refused with no error shown. It is a per-user certificate — no sudo, and\n\
             nobody else on this machine:\n\
             \n\
             \x20   syndeo-proxy ca --trust\n\
             \n\
             It asks before it does anything, and `syndeo-proxy ca --untrust` undoes it."
        );
        std::process::exit(1);
    }

    // Start the proxy unless told to use one that is already running. Without
    // this the browser is two commands and a path, which is two more than a
    // browser should need. It lives exactly as long as this binding.
    let (owned, credential) = if cli.proxy.is_none() {
        let (owned, credential) = start_proxy(install, &home)?;
        (Some(owned), Some(credential))
    } else {
        (None, None)
    };
    if let Some(owned) = &owned {
        tracing::info!(
            proxy = %owned.endpoint,
            pid = owned.pid(),
            "started this browser's own proxy; it goes when the browser does"
        );
    }
    let spec = match (&cli.proxy, &owned) {
        (Some(spec), _) => spec.clone(),
        (None, Some(owned)) => owned.endpoint.to_string(),
        (None, None) => unreachable!("a proxy is either named or started"),
    };

    let cli = Cli {
        proxy: Some(spec),
        proxy_ca: Some(authority),
        ..cli
    };
    let proxy = match cli.proxy.as_deref() {
        Some(spec) => {
            let (host, port) = spec.rsplit_once(':').context("--proxy wants host:port")?;
            tracing::info!(
                proxy = %spec,
                "the web view is configured to send its HTTP and HTTPS traffic through this \
                 proxy, except for localhost and loopback addresses, which WebKit was seen to \
                 reach directly"
            );
            Some(proxied::ProxyTarget {
                host: host.to_string(),
                port: port.to_string(),
                // Our own proxy asks every client for this; one named with
                // --proxy is somebody else's and asks for nothing.
                credential,
            })
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
    proxy: Option<proxied::ProxyTarget>,
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

/// The configuration a web view is built with: its data store, with the proxy
/// and its credential already set, before the web view exists.
fn proxy_configuration(
    target: &proxied::ProxyTarget,
) -> Result<objc2::rc::Retained<objc2_web_kit::WKWebViewConfiguration>> {
    let mtm = objc2::MainThreadMarker::new().context("web views are built on the main thread")?;
    proxied::configuration(target, mtm)
}

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
        if let Some(target) = &self.proxy {
            match proxy_configuration(target) {
                Ok(configuration) => {
                    use wry::WebViewBuilderExtMacos;
                    builder = builder.with_webview_configuration(configuration);
                }
                Err(err) => {
                    tracing::error!(%err, "could not configure the proxy; not opening a tab");
                    return;
                }
            }
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

        // The proxy is what puts this renderer behind our cache and DNS policy.
        // A web view built without it would reach the network directly.
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
        if let Some(target) = &self.proxy {
            match proxy_configuration(target) {
                Ok(configuration) => {
                    use wry::WebViewBuilderExtMacos;
                    builder = builder.with_webview_configuration(configuration);
                }
                Err(err) => {
                    // Never a web view without its proxy: it would reach the
                    // network directly.
                    tracing::error!(%err, "could not configure the proxy");
                    event_loop.exit();
                    return;
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The help says what the proxy setting covers, and claims nothing more.
    #[test]
    fn the_help_says_what_the_proxy_covers_and_no_more() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("configured to send its HTTP and HTTPS traffic through it"),
            "{help}"
        );
        // And the exception that was seen, wherever it says so.
        assert!(
            help.matches("localhost and loopback").count() >= 2,
            "{help}"
        );
        for overclaim in [
            "Everything this renders",
            "nothing else",
            "cannot bypass",
            "every byte",
        ] {
            assert!(
                !help.contains(overclaim),
                "--help claims: {overclaim}\n{help}"
            );
        }
    }
}
