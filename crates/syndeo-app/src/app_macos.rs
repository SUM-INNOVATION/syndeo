//! The application: AppKit, with WebKit in its windows.
//!
//! Everything a Mac user expects of an application comes from AppKit itself,
//! not from a window library: a Dock icon, the Syndeo, File, Edit and Window
//! menus with their usual shortcuts, a Dock click that brings a window back,
//! and closing the last window leaving the application running, as Mac
//! applications do, until it is quit.
//!
//! **It fails closed.** A web view that can reach the network is made only
//! from a [`Browsing`], and a `Browsing` exists only once all of this held at
//! launch: the proxy this application started is listening, a web view
//! configuration routed through it was built, its certificate authority is on
//! disk and is a certificate, and the delegate that accepts that authority
//! and no other was built. Each window then gets its own configuration,
//! routed through the proxy, and its web view carries that delegate, checked
//! after it is set. If any of it fails, the window holds a native label
//! saying so instead — no web view at all, so nothing in it can reach the
//! network. There is no unproxied configuration and no unpinned web view
//! anywhere in this application.
//!
//! The proxy is the one helper, started once at launch from the bundle's
//! `Contents/Helpers` (`syndeo_webkit::owned`), and stopped when the
//! application quits. If the application dies without quitting, the proxy
//! sees its stdin close and exits on its own.
//!
//! The keystore is not part of this application: it is never started, no
//! keychain entry is made for it, no passphrase is asked for and no recovery
//! phrase is generated.

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSEventModifierFlags, NSMenu, NSMenuItem, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSNotification, NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use syndeo_shell::supervisor::InstallDir;
use syndeo_webkit::{owned, pin, proxied};

/// The first-launch page, with `{{TRUST}}` filled in.
const FIRST_LAUNCH: &str = include_str!("first_launch.html");

/// The first line of every window that cannot browse.
const CANNOT_BROWSE: &str = "Syndeo can't open websites right now.";

// ------------------------------------------------------------- the decisions

/// What a window's content is made of: the WebKit pieces, behind a seam so
/// that the decisions about them are tested without WebKit.
trait Engine {
    type Config;
    type Pinner;
    type View;
    /// A web view configuration whose data store sends everything through
    /// `target`, with its credential.
    fn configure(&self, target: &proxied::ProxyTarget) -> Result<Self::Config>;
    /// The delegate that accepts the authority in `pem`, and no other.
    fn pinner(&self, pem: &str) -> Result<Self::Pinner>;
    /// A web view made with `config`.
    fn web_view(&self, config: &Self::Config) -> Self::View;
    /// Make `pinner` the view's navigation delegate, and say whether it is,
    /// as the view reports it afterwards.
    fn pin(&self, view: &Self::View, pinner: &Self::Pinner) -> bool;
    /// A view that says `text` and cannot reach the network.
    fn error(&self, text: &str) -> Self::View;
}

/// Everything a web view that can reach the network needs. It exists only
/// when [`prepare`] found all of it.
struct Browsing<P> {
    target: proxied::ProxyTarget,
    pinner: P,
}

/// What launch came to: browsing, with the proxy this application owns, or
/// why not.
enum Readiness<S, P> {
    Ready { proxy: S, browsing: Browsing<P> },
    NotReady(String),
}

/// Decide, once, whether this application may make web views that reach the
/// network. `start` starts the proxy and says where it listens; `authority`
/// reads its certificate authority. Every step has to succeed, and a proxy
/// started for a launch that then fails is dropped, which stops it.
fn prepare<E: Engine, S>(
    engine: &E,
    start: impl FnOnce() -> Result<(S, proxied::ProxyTarget)>,
    authority: impl FnOnce() -> Result<String>,
) -> Readiness<S, E::Pinner> {
    let (proxy, target) = match start() {
        Ok(started) => started,
        Err(err) => {
            return Readiness::NotReady(format!("Syndeo could not start its local proxy: {err:#}"))
        }
    };
    if let Err(err) = engine.configure(&target) {
        return Readiness::NotReady(format!(
            "Syndeo could not route its web views through its local proxy: {err:#}"
        ));
    }
    let pem = match authority() {
        Ok(pem) => pem,
        Err(err) => {
            return Readiness::NotReady(format!(
                "Syndeo's certificate authority is missing or damaged: {err:#}"
            ))
        }
    };
    let pinner = match engine.pinner(&pem) {
        Ok(pinner) => pinner,
        Err(err) => {
            return Readiness::NotReady(format!(
                "Syndeo could not pin its certificate authority: {err:#}"
            ))
        }
    };
    Readiness::Ready {
        proxy,
        browsing: Browsing { target, pinner },
    }
}

/// What a new window holds: a web view routed through the proxy and pinned
/// to its authority, or, if any of that is not so, a view that says why and
/// cannot reach the network.
fn content<E: Engine>(engine: &E, browsing: Option<&Browsing<E::Pinner>>, reason: &str) -> E::View {
    let Some(browsing) = browsing else {
        return engine.error(&cannot_browse(reason));
    };
    let config = match engine.configure(&browsing.target) {
        Ok(config) => config,
        Err(err) => {
            return engine.error(&cannot_browse(&format!(
                "Syndeo could not route this window through its local proxy: {err:#}"
            )))
        }
    };
    let view = engine.web_view(&config);
    if !engine.pin(&view, &browsing.pinner) {
        return engine.error(&cannot_browse(
            "Syndeo could not pin this window to its certificate authority.",
        ));
    }
    view
}

/// What a window that cannot browse says.
fn cannot_browse(reason: &str) -> String {
    format!(
        "{CANNOT_BROWSE}\n\n{reason}\n\nQuit Syndeo and open it again. \
         If this keeps happening, reinstall Syndeo."
    )
}

/// The proxy's certificate authority, read from `home` and required to be a
/// certificate: present, PEM, and DER that parses as one.
fn authority_pem(home: &Path) -> Result<String> {
    let path = authority(home);
    let pem =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let der = pin::der_from_pem(&pem)?;
    security_framework::certificate::SecCertificate::from_der(&der)
        .map_err(|err| anyhow::anyhow!("{} is not a certificate: {err}", path.display()))?;
    Ok(pem)
}

/// Where the proxy keeps its authority's certificate, under `home`.
fn authority(home: &Path) -> PathBuf {
    home.join("proxy").join("syndeo-ca.pem")
}

// ------------------------------------------------------------- WebKit

/// The engine the application runs: WebKit, AppKit and the pinning delegate.
struct WebKit {
    mtm: MainThreadMarker,
    frame: NSRect,
}

impl Engine for WebKit {
    type Config = Retained<WKWebViewConfiguration>;
    type Pinner = Retained<pin::Pinner>;
    type View = Retained<NSView>;

    fn configure(&self, target: &proxied::ProxyTarget) -> Result<Self::Config> {
        proxied::configuration(target, self.mtm)
    }

    fn pinner(&self, pem: &str) -> Result<Self::Pinner> {
        pin::Pinner::new(self.mtm, pem)
    }

    fn web_view(&self, config: &Self::Config) -> Self::View {
        let view = unsafe {
            WKWebView::initWithFrame_configuration(WKWebView::alloc(self.mtm), self.frame, config)
        };
        Retained::into_super(view)
    }

    fn pin(&self, view: &Self::View, pinner: &Self::Pinner) -> bool {
        let Some(webview) = view.downcast_ref::<WKWebView>() else {
            return false;
        };
        let delegate = pin::as_delegate(pinner);
        unsafe { webview.setNavigationDelegate(Some(&delegate)) };
        let set = unsafe { webview.navigationDelegate() };
        set.is_some_and(|set| {
            std::ptr::eq(
                Retained::as_ptr(&set).cast::<AnyObject>(),
                Retained::as_ptr(pinner).cast::<AnyObject>(),
            )
        })
    }

    fn error(&self, text: &str) -> Self::View {
        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), self.mtm);
        label.setSelectable(true);
        Retained::into_super(Retained::into_super(label))
    }
}

// ------------------------------------------------------------- the application

pub struct State {
    install: InstallDir,
    home: PathBuf,
    /// The proxy this application started, and owns until it quits.
    proxy: RefCell<Option<owned::OwnedProxy>>,
    /// Present only when launch found everything browsing needs.
    browsing: RefCell<Option<Browsing<Retained<pin::Pinner>>>>,
    /// Why there is no browsing, when there is none.
    not_ready: RefCell<String>,
    windows: RefCell<Vec<Retained<NSWindow>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoAppDelegate"]
    #[ivars = State]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.prepare();
            self.open_window();
            let app = NSApplication::sharedApplication(self.mtm());
            #[allow(deprecated)] // `activate` is macOS 14 and later; this runs on 13.
            app.activateIgnoringOtherApps(true);
        }

        /// A Mac application stays running with no window open.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            false
        }

        /// A Dock click, or `open` again, with no window showing brings one
        /// back.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, has_visible_windows: bool) -> bool {
            if !has_visible_windows {
                self.show_a_window();
            }
            true
        }

        /// Quitting stops the proxy, here, before the process exits: AppKit
        /// ends the process without running Rust's destructors.
        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.ivars().browsing.borrow_mut().take();
            if let Some(proxy) = self.ivars().proxy.borrow_mut().take() {
                tracing::info!(pid = proxy.pid(), "stopping the proxy");
                drop(proxy);
            }
        }
    }

    impl AppDelegate {
        /// File > New Window.
        #[unsafe(method(newWindow:))]
        fn new_window(&self, _sender: Option<&AnyObject>) {
            self.open_window();
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, state: State) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(state);
        unsafe { msg_send![super(this), init] }
    }

    fn engine(&self) -> WebKit {
        WebKit {
            mtm: self.mtm(),
            frame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1200.0, 800.0)),
        }
    }

    /// Start the proxy and decide whether this application can browse.
    fn prepare(&self) {
        let state = self.ivars();
        let readiness = prepare(
            &self.engine(),
            || {
                owned::start_proxy(state.install, &state.home).map(|(owned, credential)| {
                    let endpoint = owned.endpoint;
                    let target = proxied::ProxyTarget {
                        host: endpoint.ip().to_string(),
                        port: endpoint.port().to_string(),
                        credential: Some(credential),
                    };
                    (owned, target)
                })
            },
            || authority_pem(&state.home),
        );
        match readiness {
            Readiness::Ready { proxy, browsing } => {
                tracing::info!(proxy = %proxy.endpoint, pid = proxy.pid(), "ready to browse through this application's proxy");
                *state.proxy.borrow_mut() = Some(proxy);
                *state.browsing.borrow_mut() = Some(browsing);
            }
            Readiness::NotReady(reason) => {
                tracing::error!("not browsing: {reason}");
                *state.not_ready.borrow_mut() = reason;
            }
        }
    }

    /// Bring back a window that is closed, or open one if there is none.
    fn show_a_window(&self) {
        let existing = self.ivars().windows.borrow().first().cloned();
        match existing {
            Some(window) => window.makeKeyAndOrderFront(None),
            None => self.open_window(),
        }
    }

    fn open_window(&self) {
        let mtm = self.mtm();
        let state = self.ivars();
        let engine = self.engine();
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                engine.frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // Closed windows are kept and brought back, not freed under us.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Syndeo"));

        let view = content(
            &engine,
            state.browsing.borrow().as_ref(),
            &state.not_ready.borrow(),
        );
        if let Some(webview) = view.downcast_ref::<WKWebView>() {
            let page = NSString::from_str(&self.first_launch_page());
            unsafe { webview.loadHTMLString_baseURL(&page, None) };
        }
        window.setContentView(Some(&view));
        window.center();
        window.makeKeyAndOrderFront(None);
        state.windows.borrow_mut().push(window);
    }

    /// The first-launch page, saying whether the authority is trusted.
    fn first_launch_page(&self) -> String {
        let trust = if owned::authority_is_trusted(&authority(&self.ivars().home)) {
            "Syndeo's local certificate authority is trusted in your login keychain."
        } else {
            "Syndeo's local certificate authority is not trusted yet. Secure (https) websites \
             will not open until it is, and this preview of Syndeo cannot yet ask for that."
        };
        FIRST_LAUNCH.replace("{{TRUST}}", &escape(trust))
    }
}

/// Text for an HTML page.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Where this application keeps its state: its proxy's cache and authority.
/// The macOS place for an application's own data, not the command-line
/// tools' `~/.syndeo`.
fn home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let dir = PathBuf::from(home).join("Library/Application Support/Syndeo");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// One menu item: its title, the action it sends, and its key equivalent.
fn item(mtm: MainThreadMarker, title: &str, action: Sel, key: &str) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    }
}

/// A top-level menu, its items, added to `bar`.
fn submenu(
    mtm: MainThreadMarker,
    bar: &NSMenu,
    title: &str,
    items: &[Retained<NSMenuItem>],
) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for entry in items {
        menu.addItem(entry);
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&menu));
    bar.addItem(&holder);
    menu
}

/// The menu bar: Syndeo, File, Edit and Window, with the usual shortcuts.
fn main_menu(mtm: MainThreadMarker, app: &NSApplication) -> Retained<NSMenu> {
    let bar = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    let hide_others = item(mtm, "Hide Others", sel!(hideOtherApplications:), "h");
    hide_others
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Option | NSEventModifierFlags::Command);
    submenu(
        mtm,
        &bar,
        "Syndeo",
        &[
            item(mtm, "About Syndeo", sel!(orderFrontStandardAboutPanel:), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Hide Syndeo", sel!(hide:), "h"),
            hide_others,
            item(mtm, "Show All", sel!(unhideAllApplications:), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Quit Syndeo", sel!(terminate:), "q"),
        ],
    );
    submenu(
        mtm,
        &bar,
        "File",
        &[
            item(mtm, "New Window", sel!(newWindow:), "n"),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Close Window", sel!(performClose:), "w"),
        ],
    );
    submenu(
        mtm,
        &bar,
        "Edit",
        &[
            item(mtm, "Undo", sel!(undo:), "z"),
            item(mtm, "Redo", sel!(redo:), "Z"),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Cut", sel!(cut:), "x"),
            item(mtm, "Copy", sel!(copy:), "c"),
            item(mtm, "Paste", sel!(paste:), "v"),
            item(mtm, "Select All", sel!(selectAll:), "a"),
        ],
    );
    let window = submenu(
        mtm,
        &bar,
        "Window",
        &[
            item(mtm, "Minimize", sel!(performMiniaturize:), "m"),
            item(mtm, "Zoom", sel!(performZoom:), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Bring All to Front", sel!(arrangeInFront:), ""),
        ],
    );
    app.setWindowsMenu(Some(&window));
    bar
}

pub fn run(install: InstallDir) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("syndeo_app=info,syndeo_webkit=info")
            }),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let mtm = MainThreadMarker::new().context("Syndeo has to start on the main thread")?;
    let home = home()?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(
        mtm,
        State {
            install,
            home,
            proxy: RefCell::new(None),
            browsing: RefCell::new(None),
            not_ready: RefCell::new(String::from("Syndeo has not started its local proxy.")),
            windows: RefCell::new(Vec::new()),
        },
    );
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&main_menu(mtm, &app)));
    app.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A self-signed certificate, for the authority checks; it trusts nothing.
    const CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----
MIIBlTCCATugAwIBAgIUGgRkWGzfowVfNfPjItnxjsGaUUYwCgYIKoZIzj0EAwIw
IDEeMBwGA1UEAwwVU3luZGVvIHRlc3QgYXV0aG9yaXR5MB4XDTI2MTAxMzAzMDA1
NloXDTM2MTAxMDAzMDA1NlowIDEeMBwGA1UEAwwVU3luZGVvIHRlc3QgYXV0aG9y
aXR5MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEkmzho9CX2qN6iBocDCk2tHEt
vUfQckRMNDyGOmQAwEZRNLcp+CkOAbf1ble2NaC0Boui2U/wtWWzIPOUSfQRlKNT
MFEwHQYDVR0OBBYEFAaKb6g+m2wskrOHEyTWltcpUdSkMB8GA1UdIwQYMBaAFAaK
b6g+m2wskrOHEyTWltcpUdSkMA8GA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwID
SAAwRQIgPjq7upuEjVQmLkmR2ZPI1VLifBWIOC7/R7FwfFSHJgACIQD6Bd4kkKXM
yJpRaKsz2Azv7maKpleZREReLU9u1UnLsw==
-----END CERTIFICATE-----
";

    /// What the fake engine made.
    #[derive(Debug, Clone, PartialEq)]
    enum View {
        /// A web view, with the configuration it was made with and the
        /// delegate it carries, if any.
        Browser {
            routed_to: String,
            pinned_to: Option<String>,
        },
        Error(String),
    }

    /// WebKit's stand-in: each step can be told to fail, and it remembers
    /// what was asked of it.
    #[derive(Default)]
    struct Fake {
        configure_fails: Cell<bool>,
        pinner_fails: Cell<bool>,
        pin_fails: Cell<bool>,
        configured: Cell<usize>,
        pinners: Cell<usize>,
    }

    impl Engine for Fake {
        type Config = String;
        type Pinner = String;
        type View = RefCell<View>;

        fn configure(&self, target: &proxied::ProxyTarget) -> Result<String> {
            self.configured.set(self.configured.get() + 1);
            if self.configure_fails.get() {
                anyhow::bail!("the Network framework refused the proxy");
            }
            Ok(format!("{}:{}", target.host, target.port))
        }
        fn pinner(&self, pem: &str) -> Result<String> {
            self.pinners.set(self.pinners.get() + 1);
            if self.pinner_fails.get() {
                anyhow::bail!("Security refused the authority");
            }
            Ok(pem.lines().nth(1).unwrap_or_default().to_string())
        }
        fn web_view(&self, config: &String) -> RefCell<View> {
            RefCell::new(View::Browser {
                routed_to: config.clone(),
                pinned_to: None,
            })
        }
        fn pin(&self, view: &RefCell<View>, pinner: &String) -> bool {
            if self.pin_fails.get() {
                return false;
            }
            if let View::Browser { pinned_to, .. } = &mut *view.borrow_mut() {
                *pinned_to = Some(pinner.clone());
            }
            true
        }
        fn error(&self, text: &str) -> RefCell<View> {
            RefCell::new(View::Error(text.to_string()))
        }
    }

    /// The proxy this application would own: says when it is dropped, which
    /// is when the real one is stopped.
    struct Proxy(Rc<Cell<bool>>);
    impl Drop for Proxy {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    fn target() -> proxied::ProxyTarget {
        proxied::ProxyTarget {
            host: "127.0.0.1".into(),
            port: "4567".into(),
            credential: None,
        }
    }

    /// Launch with `engine`: a proxy that starts, and the test certificate.
    fn launch(engine: &Fake, stopped: &Rc<Cell<bool>>) -> Readiness<Proxy, String> {
        let stopped = stopped.clone();
        prepare(
            engine,
            move || Ok((Proxy(stopped), target())),
            || Ok(CERTIFICATE.to_string()),
        )
    }

    fn view(engine: &Fake, readiness: &Readiness<Proxy, String>) -> View {
        let (browsing, reason) = match readiness {
            Readiness::Ready { browsing, .. } => (Some(browsing), ""),
            Readiness::NotReady(reason) => (None, reason.as_str()),
        };
        content(engine, browsing, reason).into_inner()
    }

    fn says(view: &View, text: &str) -> bool {
        matches!(view, View::Error(said) if said.starts_with(CANNOT_BROWSE) && said.contains(text))
    }

    #[test]
    fn with_everything_in_place_a_window_browses_through_the_proxy_pinned_to_its_authority() {
        let engine = Fake::default();
        let stopped = Rc::new(Cell::new(false));
        let readiness = launch(&engine, &stopped);
        assert!(matches!(readiness, Readiness::Ready { .. }));
        assert_eq!(
            view(&engine, &readiness),
            View::Browser {
                routed_to: "127.0.0.1:4567".into(),
                pinned_to: Some(CERTIFICATE.lines().nth(1).unwrap().into())
            }
        );
        assert!(!stopped.get(), "the proxy of a ready launch is kept");
    }

    #[test]
    fn a_proxy_that_did_not_start_means_no_browsing_and_nothing_else_tried() {
        let engine = Fake::default();
        let readiness: Readiness<Proxy, String> = prepare(
            &engine,
            || anyhow::bail!("the proxy exited before it was ready (exit status: 1)"),
            || Ok(CERTIFICATE.to_string()),
        );
        let shown = view(&engine, &readiness);
        assert!(
            says(&shown, "Syndeo could not start its local proxy"),
            "{shown:?}"
        );
        assert_eq!((engine.configured.get(), engine.pinners.get()), (0, 0));
    }

    #[test]
    fn a_configuration_that_fails_at_launch_means_no_browsing_and_the_proxy_is_stopped() {
        let engine = Fake::default();
        engine.configure_fails.set(true);
        let stopped = Rc::new(Cell::new(false));
        let readiness = launch(&engine, &stopped);
        assert!(
            stopped.get(),
            "the proxy of a launch that cannot browse was left running"
        );
        let shown = view(&engine, &readiness);
        assert!(
            says(
                &shown,
                "could not route its web views through its local proxy"
            ),
            "{shown:?}"
        );
    }

    #[test]
    fn a_missing_authority_means_no_browsing() {
        let engine = Fake::default();
        let stopped = Rc::new(Cell::new(false));
        let readiness: Readiness<Proxy, String> = prepare(
            &engine,
            {
                let stopped = stopped.clone();
                move || Ok((Proxy(stopped), target()))
            },
            || authority_pem(Path::new("/nonexistent/syndeo-home")),
        );
        assert!(stopped.get());
        let shown = view(&engine, &readiness);
        assert!(
            says(&shown, "certificate authority is missing or damaged"),
            "{shown:?}"
        );
        assert_eq!(
            engine.pinners.get(),
            0,
            "a pinner was built from no authority"
        );
    }

    #[test]
    fn a_damaged_authority_is_refused_whatever_the_damage() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("proxy")).unwrap();
        for (what, pem) in [
            ("empty", String::new()),
            (
                "not base64",
                "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n".into(),
            ),
            (
                "base64 of nothing like a certificate",
                "-----BEGIN CERTIFICATE-----\nMIIBIjAN\n-----END CERTIFICATE-----\n".into(),
            ),
            (
                "truncated",
                CERTIFICATE.lines().take(4).collect::<Vec<_>>().join("\n"),
            ),
        ] {
            std::fs::write(authority(home.path()), &pem).unwrap();
            assert!(
                authority_pem(home.path()).is_err(),
                "a {what} authority was accepted"
            );
        }
        std::fs::write(authority(home.path()), CERTIFICATE).unwrap();
        assert_eq!(authority_pem(home.path()).unwrap(), CERTIFICATE);
    }

    #[test]
    fn a_pinner_that_cannot_be_built_means_no_browsing() {
        let engine = Fake::default();
        engine.pinner_fails.set(true);
        let stopped = Rc::new(Cell::new(false));
        let readiness = launch(&engine, &stopped);
        assert!(stopped.get());
        let shown = view(&engine, &readiness);
        assert!(
            says(&shown, "could not pin its certificate authority"),
            "{shown:?}"
        );
    }

    #[test]
    fn a_window_whose_configuration_fails_gets_no_web_view() {
        let engine = Fake::default();
        let stopped = Rc::new(Cell::new(false));
        let readiness = launch(&engine, &stopped);
        engine.configure_fails.set(true);
        let shown = view(&engine, &readiness);
        assert!(
            says(
                &shown,
                "could not route this window through its local proxy"
            ),
            "{shown:?}"
        );
    }

    #[test]
    fn a_web_view_the_pinner_did_not_take_is_never_shown() {
        let engine = Fake::default();
        let stopped = Rc::new(Cell::new(false));
        let readiness = launch(&engine, &stopped);
        engine.pin_fails.set(true);
        let shown = view(&engine, &readiness);
        assert!(says(&shown, "could not pin this window"), "{shown:?}");
    }

    #[test]
    fn the_page_is_given_text_never_markup() {
        assert_eq!(
            escape("<b>\"&\"</b>"),
            "&lt;b&gt;&quot;&amp;&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn the_first_launch_page_has_its_place_to_fill() {
        assert!(FIRST_LAUNCH.contains("{{TRUST}}"));
    }

    /// The application's state is the Mac's place for it, never the
    /// command-line tools' home.
    #[test]
    fn the_state_lives_in_application_support() {
        let dir = authority(Path::new(
            "/Users/someone/Library/Application Support/Syndeo",
        ));
        assert_eq!(
            dir,
            Path::new("/Users/someone/Library/Application Support/Syndeo/proxy/syndeo-ca.pem")
        );
    }
}
