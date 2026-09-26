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
//! does not send through that setting is not covered by it — WebRTC's own
//! transports are the obvious candidate, and are not yet measured.
//!
//! Caching HTTPS means terminating it, so the proxy presents certificates it
//! issued, and two things make WebKit accept them. The authority is trusted
//! for TLS in this user's login keychain (`syndeo-proxy ca --trust`, which asks
//! first), because WebKit validates subresources in its networking process
//! against the user's trust settings and nothing in ours. And this process pins
//! it: a server-trust challenge in the web view is evaluated with that one
//! authority as the only anchor, so a certificate from anyone else is refused.
//! See `pin_macos.rs`.

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

#[derive(Parser, Clone)]
#[command(
    name = "syndeo-webkit",
    version,
    about = "A renderer configured to send its traffic through syndeo-proxy, with the proxy's certificate pinned"
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
    /// Where the proxy is listening. Everything this renders goes through it.
    ///
    /// Optional only so the two halves can be told apart when something does
    /// not load: without it this is an ordinary web view, and if that fails too
    /// then the proxy was never the problem.
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
    let home = cli.home.clone().unwrap_or_else(default_home);

    let spec = cli
        .proxy
        .clone()
        .unwrap_or_else(|| DEFAULT_PROXY.to_string());
    let authority = cli
        .proxy_ca
        .clone()
        .unwrap_or_else(|| home.join("proxy").join("syndeo-ca.pem"));

    // First, before anything is started: the failure without this is every page
    // refusing to load with nothing said about why, because WebKit validates
    // subresources in its networking process and that does not consult us.
    if !authority_is_trusted() {
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
    // browser should need.
    let _proxy_process = if cli.proxy.is_none() {
        Some(start_proxy(&home)?)
    } else {
        None
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

/// Where everything lives when nobody says otherwise.
fn default_home() -> PathBuf {
    std::env::var_os("SYNDEO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".syndeo")
        })
}

const DEFAULT_PROXY: &str = "127.0.0.1:8899";

/// The proxy this browser started: that process and no other.
///
/// Dropping it kills and reaps the child, which covers returning from `run`,
/// an error on the way out, and unwinding. Until it is reaped the child's pid
/// cannot be given to anyone else, so the kill can only ever reach our own
/// proxy. What a destructor cannot cover — a force-quit, a crash, `kill -9` —
/// the child covers itself: it holds the other end of its stdin pipe and exits
/// when the kernel closes ours.
struct ProxyChild(std::process::Child);

impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Refuse to start a proxy where something is already listening.
///
/// Whatever is there is not ours to stop, and not ours to trust: pages would go
/// to it, and plain http through it would be readable and changeable by
/// whoever runs it. If it is a proxy the user started themselves, `--proxy`
/// says so on purpose.
fn refuse_if_occupied(address: std::net::SocketAddr) -> Result<()> {
    if std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(300)).is_ok()
    {
        anyhow::bail!(
            "something is already listening on {address}, and this would start its own \
             proxy there. If that is a syndeo-proxy you started, use it with \
             `--proxy {address}`; otherwise stop it first."
        );
    }
    Ok(())
}

/// Start `command` as this browser's proxy and wait until it listens on
/// `address`, noticing if it exits first.
fn start_owned(
    mut command: std::process::Command,
    address: std::net::SocketAddr,
    within: std::time::Duration,
) -> Result<ProxyChild> {
    refuse_if_occupied(address)?;
    let child = command
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("starting the proxy")?;
    let mut owned = ProxyChild(child);

    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if let Some(status) = owned.0.try_wait()? {
            anyhow::bail!("the proxy exited before it was listening ({status})");
        }
        if std::net::TcpStream::connect(address).is_ok() {
            tracing::info!(proxy = %address, "the proxy is listening");
            return Ok(owned);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    anyhow::bail!(
        "the proxy did not start listening on {address} within {} seconds",
        within.as_secs()
    )
}

/// Start the proxy this browser fetches through, and wait until it answers.
///
/// A sibling binary, found the way the shell finds its own, so a build tree and
/// an install both work. It goes when this process goes, however that happens:
/// see [`ProxyChild`].
fn start_proxy(home: &std::path::Path) -> Result<ProxyChild> {
    let exe = std::env::current_exe().context("locating the running binary")?;
    let resolved = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());
    let beside = resolved
        .parent()
        .map(|d| d.join("syndeo-proxy"))
        .filter(|p| p.exists())
        .context(
            "cannot find syndeo-proxy next to this binary. Every Syndeo binary \
             has to be installed into the same directory.",
        )?;

    let mut command = std::process::Command::new(beside);
    command
        .args(["run", "--exit-with-parent"])
        // On the environment rather than a flag, because that is where the
        // proxy reads it from and inventing a flag it does not have is how the
        // first attempt at this failed.
        .env("SYNDEO_HOME", home);
    // It has to be listening before the first page is asked for.
    start_owned(
        command,
        DEFAULT_PROXY.parse().expect("a valid address"),
        std::time::Duration::from_secs(10),
    )
}

/// Whether our authority is in a keychain WebKit's networking process consults.
///
/// Only a hint — it asks whether the certificate is present, not whether its
/// trust settings are right — but it is the difference between a clear message
/// and a window that renders nothing for no stated reason.
fn authority_is_trusted() -> bool {
    std::process::Command::new("/usr/bin/security")
        .args(["find-certificate", "-c", "Syndeo Local Measurement CA"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn sleeper() -> ProxyChild {
        ProxyChild(Command::new("/bin/sleep").arg("60").spawn().unwrap())
    }

    /// Held by every test that binds a port, or picks one it relies on nobody
    /// else taking: a port one test lets go of can be handed straight to
    /// another.
    static PORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A free port below the ephemeral range. A port handed out by binding to
    /// port 0 is an ephemeral one, and connecting to it once it is free again
    /// can be given that same port as its source — a TCP connection to itself,
    /// which looks exactly like somebody listening. 8899 is never in that
    /// range, so this is a property of the tests and not of the browser.
    fn quiet_address() -> std::net::SocketAddr {
        (20000..30000)
            .map(|port| std::net::SocketAddr::from(([127, 0, 0, 1], port)))
            .skip((std::process::id() % 5000) as usize)
            .find(|address| std::net::TcpListener::bind(address).is_ok())
            .expect("a free port below the ephemeral range")
    }

    /// Nobody listens on port 1, and no connection is ever given it as a
    /// source port.
    fn nobody() -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], 1))
    }

    #[test]
    fn the_proxy_goes_when_its_owner_is_dropped() {
        let owned = sleeper();
        let pid = owned.0.id();
        assert!(alive(pid));
        drop(owned);
        assert!(!alive(pid));
    }

    #[test]
    fn the_proxy_goes_when_its_owner_returns_an_error() {
        let mut pid = 0;
        let result: Result<()> = (|| {
            let owned = sleeper();
            pid = owned.0.id();
            anyhow::bail!("something failed after the proxy started")
        })();
        assert!(result.is_err());
        assert!(!alive(pid));
    }

    #[test]
    fn the_proxy_goes_when_its_owner_unwinds() {
        let pid = std::sync::atomic::AtomicU32::new(0);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let owned = sleeper();
            pid.store(owned.0.id(), std::sync::atomic::Ordering::SeqCst);
            panic!("something panicked after the proxy started");
        }));
        assert!(unwound.is_err());
        assert!(!alive(pid.load(std::sync::atomic::Ordering::SeqCst)));
    }

    #[test]
    fn a_proxy_that_exits_before_listening_is_noticed_at_once() {
        let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
        let started = Instant::now();
        let refused = start_owned(
            Command::new("/usr/bin/false"),
            nobody(),
            Duration::from_secs(10),
        );
        let message = refused.err().unwrap().to_string();
        assert!(
            message.contains("exited before it was listening"),
            "{message}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited out the deadline"
        );
    }

    #[test]
    fn a_taken_port_is_refused_before_anything_starts_and_left_alone() {
        let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let marker = std::env::temp_dir().join(format!(
            "syndeo-webkit-test-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos() + address.port() as u128
        ));
        let _ = std::fs::remove_file(&marker);
        let mut command = Command::new("/usr/bin/touch");
        command.arg(&marker);

        let message = start_owned(command, address, Duration::from_secs(5))
            .err()
            .unwrap()
            .to_string();

        assert!(message.contains(&format!("--proxy {address}")), "{message}");
        assert!(!marker.exists(), "a proxy was started anyway");
        // Whoever holds the port still holds it, and still answers.
        std::net::TcpStream::connect(address).unwrap();
        assert!(listener.accept().is_ok());
    }

    #[test]
    fn a_proxy_that_listens_is_kept_until_its_owner_goes() {
        let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
        let address = quiet_address();
        // nc rather than python3: /usr/bin/python3 is a launcher that starts
        // the interpreter as a child of its own, so killing it would leave the
        // real listener behind.
        let mut command = Command::new("/usr/bin/nc");
        command.args(["-lk", "127.0.0.1", &address.port().to_string()]);
        let owned = start_owned(command, address, Duration::from_secs(10)).unwrap();
        let pid = owned.0.id();
        assert!(alive(pid));
        drop(owned);
        assert!(!alive(pid));
    }
}
