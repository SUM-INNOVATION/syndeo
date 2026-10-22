//! The application: AppKit, with WebKit in its windows.
//!
//! Everything a Mac user expects of an application comes from AppKit itself,
//! not from a window library: a Dock icon; the Syndeo, File, Edit, View,
//! History and Window menus with their usual shortcuts; tabs, as macOS draws
//! them; a Dock click that brings a window back; and closing the last window
//! leaving the application running, as Mac applications do, until it is quit.
//! Each window has its controller (`browser_macos`), which is let go when the
//! window closes.
//!
//! **It fails closed.** A web view that can reach the network is made only
//! from a [`Browsing`], and a `Browsing` exists only once all of this held at
//! launch: the proxy this application started is listening, a web view
//! configuration routed through it was built, its certificate authority is on
//! disk and is a certificate, and the delegate that accepts that authority
//! and no other was built. Each window then gets its own configuration,
//! routed through the proxy, and its web view's delegate is the window's
//! controller, which answers every TLS challenge with that authority as the
//! only anchor, checked after it is set. If any of it fails, the window holds
//! a native label saying so instead — no web view at all, so nothing in it
//! can reach the network. There is no unproxied configuration and no unpinned
//! web view anywhere in this application, and a page asking for a new window
//! gets a tab made the same way. WebKit itself opens `localhost` and loopback
//! addresses directly rather than through the proxy, as measured on CI, and
//! may open local-network addresses either way; Syndeo caches neither. Each
//! window's bar says which applies to its page (`address::route`), and
//! nothing here claims that every request goes through Syndeo.
//!
//! Nothing here asks for, reads or changes the keychain's trust settings:
//! each window accepts the proxy's authority, and no other, itself, so
//! secure pages open without it. Settings (`settings_macos`) holds the
//! search engine and Clear Browsing Data.
//!
//! The proxy is the one helper, started once at launch from the bundle's
//! `Contents/Helpers` (`syndeo_webkit::owned`), and stopped when the
//! application quits. If the application dies without quitting, the proxy
//! sees its stdin close and exits on its own.
//!
//! The keystore is not part of this application: it is never started, no
//! keychain entry is made for it, no passphrase is asked for and no recovery
//! phrase is generated.
//!
//! It is a web browser to macOS: its Info.plist claims http and https, so it
//! can be chosen as the default browser, and another application can ask it
//! to open an address (`open -a Syndeo <address>`, or a link in Mail). Each
//! such address opens in a tab made as every tab is, or is refused there as
//! typing it would be (`address::asked`); none is opened before launch has
//! decided whether this application can browse. At launch it also says, in
//! its log, what it can do with passkeys (`passkeys`): without Apple's
//! browser passkey entitlement, which it does not hold, nothing.

use crate::address::{self, Asked, Search};
use crate::browser_macos::{browser_content, BrowserController, Hooks};
use crate::settings_macos::{self, Clearing, SettingsController};
use anyhow::{Context, Result};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSEventModifierFlags, NSMenu, NSMenuItem, NSTextField, NSView, NSWindow, NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSArray, NSNotification, NSPoint, NSRect, NSSize, NSString, NSURLRequest, NSURL,
};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};
use std::cell::{Cell, RefCell};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use syndeo_shell::supervisor::InstallDir;
use syndeo_webkit::{owned, pin, proxied};

/// The first-launch page, with `{{SEARCH}}` filled in.
const FIRST_LAUNCH: &str = include_str!("first_launch.html");

/// The first line of every window that cannot browse.
const CANNOT_BROWSE: &str = "Syndeo can't open websites right now.";

/// Every browser window's tabbing identifier, so they tab together.
const TABS: &str = "com.sum.syndeo.browser";

// ------------------------------------------------------------- the decisions

/// What a window's content is made of: the WebKit pieces, behind a seam so
/// that the decisions about them are tested without WebKit.
trait Engine {
    type Config;
    type Pinner;
    type WebView;
    type View;
    type Delegate;
    /// A web view configuration whose data store sends everything through
    /// `target`, with its credential.
    fn configure(&self, target: &proxied::ProxyTarget) -> Result<Self::Config>;
    /// The delegate that accepts the authority in `pem`, and no other.
    fn pinner(&self, pem: &str) -> Result<Self::Pinner>;
    /// A web view made with `config`.
    fn web_view(&self, config: &Self::Config) -> Self::WebView;
    /// Make the view's navigation delegate one that answers challenges with
    /// `pinner`, and hand it back, but only if the view reports it as its
    /// navigation delegate afterwards.
    fn pin(&self, view: &Self::WebView, pinner: &Self::Pinner) -> Option<Self::Delegate>;
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

/// What a new window holds.
enum Content<W, V, D> {
    /// A web view routed through the proxy, and the delegate that pins it.
    Browser(W, D),
    /// A view saying why the window cannot browse, which cannot reach the
    /// network.
    Refused(V),
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
fn content<E: Engine>(
    engine: &E,
    browsing: Option<&Browsing<E::Pinner>>,
    reason: &str,
) -> Content<E::WebView, E::View, E::Delegate> {
    let Some(browsing) = browsing else {
        return Content::Refused(engine.error(&cannot_browse(reason)));
    };
    let config = match engine.configure(&browsing.target) {
        Ok(config) => config,
        Err(err) => {
            return Content::Refused(engine.error(&cannot_browse(&format!(
                "Syndeo could not route this window through its local proxy: {err:#}"
            ))))
        }
    };
    let view = engine.web_view(&config);
    let Some(delegate) = engine.pin(&view, &browsing.pinner) else {
        return Content::Refused(engine.error(&cannot_browse(
            "Syndeo could not pin this window to its certificate authority.",
        )));
    };
    Content::Browser(view, delegate)
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

/// The engine the application runs: WebKit, AppKit and the window
/// controllers that pin.
struct WebKit {
    mtm: MainThreadMarker,
    frame: NSRect,
    /// The domains sent around the proxy, directly, as this window is made:
    /// see [`State::direct`].
    direct: Vec<String>,
}

impl Engine for WebKit {
    type Config = Retained<WKWebViewConfiguration>;
    type Pinner = Retained<pin::Pinner>;
    type WebView = Retained<WKWebView>;
    type View = Retained<NSView>;
    type Delegate = Retained<BrowserController>;

    fn configure(&self, target: &proxied::ProxyTarget) -> Result<Self::Config> {
        // Every window sets the one data store's proxy configuration, so each
        // carries the hosts sent directly, or making one would undo them.
        proxied::configuration_sending_direct(target, &self.direct, self.mtm)
    }

    fn pinner(&self, pem: &str) -> Result<Self::Pinner> {
        pin::Pinner::new(self.mtm, pem)
    }

    fn web_view(&self, config: &Self::Config) -> Self::WebView {
        unsafe {
            WKWebView::initWithFrame_configuration(WKWebView::alloc(self.mtm), self.frame, config)
        }
    }

    fn pin(&self, view: &Self::WebView, pinner: &Self::Pinner) -> Option<Self::Delegate> {
        let controller = BrowserController::new(self.mtm, view.clone(), pinner.clone());
        controller.take_over().then_some(controller)
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
    /// Every open window, with its controller.
    windows: RefCell<Vec<(Retained<NSWindow>, Retained<BrowserController>)>>,
    /// Windows that closed, let go of once their closing has finished.
    retired: RefCell<Vec<(Retained<NSWindow>, Retained<BrowserController>)>>,
    hooks: RefCell<Option<Rc<Hooks>>>,
    /// The Settings window's controller, made the first time it is shown.
    settings: RefCell<Option<Retained<SettingsController>>>,
    /// Launch has finished, and decided whether this application can browse.
    launched: Cell<bool>,
    /// Addresses other applications asked this one to open before then.
    asked: RefCell<Vec<String>>,
    /// The domains every window sends around the proxy, directly, because
    /// macOS refused them through it (#58): this session's, in memory only,
    /// never written anywhere. At most [`DIRECT_KEPT`].
    direct: RefCell<Vec<String>>,
    /// The hosts the proxy has said were refused this session. At most
    /// [`DIRECT_KEPT`].
    heard: RefCell<Vec<String>>,
}

/// How many domains a session sends directly, and how many refused hosts it
/// remembers, at most.
const DIRECT_KEPT: usize = 1024;

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
            crate::passkeys::report();
            self.ivars().launched.set(true);
            // Opened to open addresses: those, and no start page beside them.
            let asked = self.ivars().asked.take();
            if asked.is_empty() {
                self.open_window();
            }
            for address in asked {
                self.open_asked(&address);
            }
            let app = NSApplication::sharedApplication(self.mtm());
            #[allow(deprecated)] // `activate` is macOS 14 and later; this runs on 13.
            app.activateIgnoringOtherApps(true);
        }

        /// Another application asks this one to open addresses. Before
        /// launch has decided whether it can browse, they wait for it.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            let addresses: Vec<String> = urls
                .iter()
                .map(|url| url.absoluteString().map(|text| text.to_string()).unwrap_or_default())
                .collect();
            if !self.ivars().launched.get() {
                self.ivars().asked.borrow_mut().extend(addresses);
                return;
            }
            for address in addresses {
                self.open_asked(&address);
            }
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
        fn new_window_action(&self, _sender: Option<&AnyObject>) {
            self.open_window();
        }

        /// File > New Tab with no window in front: a window.
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&AnyObject>) {
            self.open_window();
        }

        /// Syndeo > Settings….
        #[unsafe(method(showSettings:))]
        fn show_settings(&self, _sender: Option<&AnyObject>) {
            self.settings().show();
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
            direct: self.ivars().direct.borrow().clone(),
        }
    }

    /// Send `hosts` around the proxy, directly, for every window from now
    /// on; whether that took effect. Only ever for hosts a window saw macOS
    /// refuse through the proxy: see `BrowserController`.
    fn send_direct(&self, hosts: &[String]) -> bool {
        let browsing = self.ivars().browsing.borrow();
        let Some(browsing) = browsing.as_ref() else {
            return false;
        };
        let mut direct = self.ivars().direct.borrow().clone();
        for host in hosts {
            if !address::sent_direct(host, &direct) && direct.len() < DIRECT_KEPT {
                direct.push(host.clone());
            }
        }
        match proxied::send_direct(&browsing.target, &direct, self.mtm()) {
            Ok(()) => {
                *self.ivars().direct.borrow_mut() = direct;
                true
            }
            Err(err) => {
                tracing::warn!("could not send a host around the proxy: {err:#}");
                false
            }
        }
    }

    /// The hosts the proxy has said were refused, this session.
    fn refused(&self) -> Vec<String> {
        let said = match &*self.ivars().proxy.borrow() {
            Some(proxy) => proxy.control().refused(),
            None => Vec::new(),
        };
        let mut heard = self.ivars().heard.borrow_mut();
        for host in said {
            if !heard.contains(&host) && heard.len() < DIRECT_KEPT {
                heard.push(host);
            }
        }
        heard.clone()
    }

    /// What every controller may ask of the application, made once.
    fn hooks(&self) -> Rc<Hooks> {
        if let Some(hooks) = self.ivars().hooks.borrow().clone() {
            return hooks;
        }
        let me = Weak::from_retained(&self.retain());
        let (tab, closed, start) = (me.clone(), me.clone(), me.clone());
        let (direct, send, refused) = (me.clone(), me.clone(), me);
        let search: Box<dyn Fn() -> Search> = Box::new(settings_macos::search);
        let hooks = Rc::new(Hooks {
            open_tab: Box::new(move |window, request| {
                if let Some(app) = tab.load() {
                    app.open_tab(window, request);
                }
            }),
            closed: Box::new(move |window| {
                if let Some(app) = closed.load() {
                    app.forget(window);
                }
            }),
            start_page: Box::new(move || {
                start
                    .load()
                    .map(|app| app.first_launch_page())
                    .unwrap_or_default()
            }),
            search,
            direct: Box::new(move || {
                direct
                    .load()
                    .map(|app| app.ivars().direct.borrow().clone())
                    .unwrap_or_default()
            }),
            send_direct: Box::new(move |hosts| {
                send.load().is_some_and(|app| app.send_direct(hosts))
            }),
            refused: Box::new(move || refused.load().map(|app| app.refused()).unwrap_or_default()),
        });
        *self.ivars().hooks.borrow_mut() = Some(hooks.clone());
        hooks
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

    /// The Settings window's controller, made the first time it is asked for:
    /// clearing stops every window's loading, and asks this application's
    /// proxy, if it has one, to empty its cache.
    fn settings(&self) -> Retained<SettingsController> {
        if let Some(settings) = self.ivars().settings.borrow().clone() {
            return settings;
        }
        let me = Weak::from_retained(&self.retain());
        let clearing = Clearing {
            stop_loading: Box::new(move || {
                let Some(app) = me.load() else { return };
                // Copied first: stopping reports back, and nothing may find
                // the list borrowed then.
                let controllers: Vec<Retained<BrowserController>> = app
                    .ivars()
                    .windows
                    .borrow()
                    .iter()
                    .map(|(_, controller)| controller.clone())
                    .collect();
                for controller in controllers {
                    controller.stop_loading();
                }
            }),
            proxy: self
                .ivars()
                .proxy
                .borrow()
                .as_ref()
                .map(owned::OwnedProxy::control),
        };
        let settings = SettingsController::new(self.mtm(), clearing);
        *self.ivars().settings.borrow_mut() = Some(settings.clone());
        settings
    }

    /// Bring a window to the front, or open one if there is none.
    fn show_a_window(&self) {
        let existing = self
            .ivars()
            .windows
            .borrow()
            .first()
            .map(|(window, _)| window.clone());
        match existing {
            Some(window) => window.makeKeyAndOrderFront(None),
            None => self.open_window(),
        }
    }

    /// A new window, in front, with the start page.
    fn open_window(&self) {
        let window = self.new_window(None);
        window.center();
        window.makeKeyAndOrderFront(None);
    }

    /// A new tab beside `parent`: `request`, or the start page.
    fn open_tab(&self, parent: &NSWindow, request: Option<Retained<NSURLRequest>>) {
        let window = self.new_window(request);
        parent.addTabbedWindow_ordered(&window, NSWindowOrderingMode::Above);
        window.makeKeyAndOrderFront(None);
    }

    /// An address another application asked this one to open, in a new tab
    /// beside the browser window in front, or a new window if there is none:
    /// made by `new_window`, as every tab is, so it browses only through the
    /// proxy, pinned to its authority. One `address::asked` refuses gets
    /// Syndeo's page saying why. The log has its scheme, and nothing else of
    /// it.
    fn open_asked(&self, address: &str) {
        let scheme = address::explicit_scheme(address).unwrap_or_default();
        let (request, refused) = match address::asked(address) {
            Asked::Open(url) => match NSURL::URLWithString(&NSString::from_str(url.as_str())) {
                Some(url) => (Some(NSURLRequest::requestWithURL(&url)), None),
                None => (None, Some("That address could not be read.".to_string())),
            },
            Asked::Refused(why) => (None, Some(why)),
        };
        if refused.is_some() {
            tracing::info!(scheme = %scheme, "refused an address another application asked to open");
        } else {
            tracing::info!(scheme = %scheme, "opening an address another application asked to open");
        }
        let parent = self.front_browser();
        let window = self.new_window(request);
        if let (Some(why), Some(controller)) = (refused, self.controller_of(&window)) {
            controller.refuse(address, &why);
        }
        match parent {
            Some(parent) => parent.addTabbedWindow_ordered(&window, NSWindowOrderingMode::Above),
            None => window.center(),
        }
        window.makeKeyAndOrderFront(None);
    }

    /// The browser window in front: the main window, if it is one of this
    /// application's browser windows, or else the one opened last.
    fn front_browser(&self) -> Option<Retained<NSWindow>> {
        let windows = self.ivars().windows.borrow();
        let ours = |window: &NSWindow| {
            windows
                .iter()
                .any(|(open, _)| std::ptr::eq(&**open, window))
        };
        NSApplication::sharedApplication(self.mtm())
            .mainWindow()
            .filter(|main| ours(main))
            .or_else(|| windows.last().map(|(window, _)| window.clone()))
    }

    /// The controller of `window`, one of this application's browser windows.
    fn controller_of(&self, window: &NSWindow) -> Option<Retained<BrowserController>> {
        self.ivars()
            .windows
            .borrow()
            .iter()
            .find(|(open, _)| std::ptr::eq(&**open, window))
            .map(|(_, controller)| controller.clone())
    }

    /// A window that closed: its controller already let go of its web view.
    /// Kept until the next window opens or closes, so nothing is freed while
    /// AppKit is still closing it.
    fn forget(&self, window: &NSWindow) {
        let state = self.ivars();
        state.retired.borrow_mut().clear();
        let mut windows = state.windows.borrow_mut();
        let mut kept = Vec::with_capacity(windows.len());
        for (open, controller) in windows.drain(..) {
            if std::ptr::eq(&*open, window) {
                open.setDelegate(None);
                state.retired.borrow_mut().push((open, controller));
            } else {
                kept.push((open, controller));
            }
        }
        *windows = kept;
        tracing::info!(open = windows.len(), "a window closed");
    }

    /// A window: browsing through the proxy if this application can, a
    /// window that says why not if it cannot.
    fn new_window(&self, request: Option<Retained<NSURLRequest>>) -> Retained<NSWindow> {
        let mtm = self.mtm();
        let state = self.ivars();
        state.retired.borrow_mut().clear();
        let engine = self.engine();
        let hooks = self.hooks();
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
        // This application holds its windows, and lets them go when they close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Syndeo"));
        window.setTabbingIdentifier(&NSString::from_str(TABS));

        let made = content(
            &engine,
            state.browsing.borrow().as_ref(),
            &state.not_ready.borrow(),
        );
        let controller = match made {
            Content::Browser(webview, controller) => {
                let frame = NSRect::new(NSPoint::new(0.0, 0.0), engine.frame.size);
                let (container, ui) = browser_content(mtm, frame, &webview, &controller);
                controller.attach(&window, ui, hooks);
                window.setContentView(Some(&container));
                match request {
                    Some(request) => controller.load(&request),
                    None => controller.show_start_page(),
                }
                controller
            }
            Content::Refused(view) => {
                window.setContentView(Some(&view));
                BrowserController::inert(mtm, &window, hooks)
            }
        };
        window.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        let mut windows = state.windows.borrow_mut();
        windows.push((window.clone(), controller));
        tracing::info!(open = windows.len(), "a window opened");
        window
    }

    /// The start page, saying what typing words there does now.
    fn first_launch_page(&self) -> String {
        start_page(settings_macos::search())
    }
}

/// The start page, for `search`.
fn start_page(search: Search) -> String {
    let words = match search {
        Search::Off => "Search is off, so words you type there are not sent anywhere. To \
                        search from there, choose a search engine in Syndeo > Settings."
            .to_string(),
        engine => format!(
            "Words you type there are searched for with {} when you press Return, and \
             nothing is sent while you type. Choose another search engine, or none, in \
             Syndeo > Settings.",
            engine.title()
        ),
    };
    FIRST_LAUNCH.replace("{{SEARCH}}", &address::escape(&words))
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

/// The menu bar: Syndeo, File, Edit, View, History and Window, with the
/// usual shortcuts. Browsing items have no target, so they reach the window
/// in front through the responder chain, and are disabled where there is no
/// browsing to do.
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
            item(mtm, "Settings…", sel!(showSettings:), ","),
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
            item(mtm, "New Tab", sel!(newWindowForTab:), "t"),
            item(mtm, "New Window", sel!(newWindow:), "n"),
            item(mtm, "Open Location…", sel!(openLocation:), "l"),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Close", sel!(performClose:), "w"),
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
    submenu(
        mtm,
        &bar,
        "View",
        &[
            item(mtm, "Reload Page", sel!(reloadPage:), "r"),
            item(mtm, "Stop Loading", sel!(stopPage:), "."),
        ],
    );
    submenu(
        mtm,
        &bar,
        "History",
        &[
            item(mtm, "Back", sel!(syndeoBack:), "["),
            item(mtm, "Forward", sel!(syndeoForward:), "]"),
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
        .with_ansi(std::io::stderr().is_terminal())
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
            retired: RefCell::new(Vec::new()),
            hooks: RefCell::new(None),
            settings: RefCell::new(None),
            launched: Cell::new(false),
            asked: RefCell::new(Vec::new()),
            direct: RefCell::new(Vec::new()),
            heard: RefCell::new(Vec::new()),
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
        type WebView = RefCell<View>;
        type View = RefCell<View>;
        type Delegate = String;

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
        fn pin(&self, view: &RefCell<View>, pinner: &String) -> Option<String> {
            if self.pin_fails.get() {
                return None;
            }
            if let View::Browser { pinned_to, .. } = &mut *view.borrow_mut() {
                *pinned_to = Some(pinner.clone());
            }
            Some(pinner.clone())
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

    /// What a new window would hold, and the delegate it was given, if any.
    fn window(engine: &Fake, readiness: &Readiness<Proxy, String>) -> (View, Option<String>) {
        let (browsing, reason) = match readiness {
            Readiness::Ready { browsing, .. } => (Some(browsing), ""),
            Readiness::NotReady(reason) => (None, reason.as_str()),
        };
        match content(engine, browsing, reason) {
            Content::Browser(view, delegate) => (view.into_inner(), Some(delegate)),
            Content::Refused(view) => (view.into_inner(), None),
        }
    }

    fn view(engine: &Fake, readiness: &Readiness<Proxy, String>) -> View {
        window(engine, readiness).0
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
        let pinned = CERTIFICATE.lines().nth(1).unwrap().to_string();
        assert_eq!(
            window(&engine, &readiness),
            (
                View::Browser {
                    routed_to: "127.0.0.1:4567".into(),
                    pinned_to: Some(pinned.clone())
                },
                Some(pinned)
            )
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
        let (shown, delegate) = window(&engine, &readiness);
        assert!(says(&shown, "could not pin this window"), "{shown:?}");
        assert_eq!(
            delegate, None,
            "a window without its pinning delegate was handed out"
        );
    }

    #[test]
    fn the_start_page_says_what_typing_words_does_now() {
        assert!(FIRST_LAUNCH.contains("{{SEARCH}}"));
        let on = start_page(Search::DuckDuckGo);
        assert!(
            on.contains("searched for with DuckDuckGo when you press Return"),
            "{on}"
        );
        let off = start_page(Search::Off);
        assert!(off.contains("Search is off"), "{off}");
        assert!(!off.contains("{{"), "{off}");
    }

    /// The start page says secure pages need nothing from the keychain, and
    /// says which pages do not come through Syndeo; it never says the
    /// authority needs trusting.
    #[test]
    fn the_start_page_asks_for_no_trust_and_claims_no_more_than_was_measured() {
        let page = start_page(Search::DuckDuckGo);
        assert!(page.contains("Nothing is added to your keychain"), "{page}");
        assert!(page.contains("Direct — not cached by Syndeo"), "{page}");
        assert!(page.contains("Local — not cached by Syndeo"), "{page}");
        assert!(page.contains("may connect directly or through"), "{page}");
        assert!(page.contains("classified by its own address"), "{page}");
        for claim in [
            "not in your login keychain",
            "trust Syndeo",
            "Trust Syndeo",
            "all traffic",
            "every request",
        ] {
            assert!(!page.contains(claim), "the start page says {claim:?}");
        }
    }

    /// The start page says, in so many words and where every new window and
    /// tab shows it, that WebRTC goes around Syndeo: measured in CI
    /// 39198465745, where a page's STUN went from WebKit's own networking
    /// process to a public server, and two connections in a page joined
    /// directly. ci/test-app.sh realtime keeps that measurement a gate, so
    /// the sentence cannot quietly stop being true.
    #[test]
    fn the_start_page_says_webrtc_goes_around_syndeo() {
        let page = start_page(Search::DuckDuckGo);
        assert!(
            page.contains(
                "<li id=\"webrtc\"><b>WebRTC media and peer connections connect directly and are \
                 not sent through or cached by Syndeo. Syndeo is not a VPN.</b></li>"
            ),
            "{page}"
        );
    }

    /// Nothing in the application asks for, reads or changes the keychain's
    /// trust settings, or runs `security`, which could: each window accepts
    /// the proxy's authority itself.
    #[test]
    fn nothing_here_touches_the_keychains_trust() {
        for (file, source) in [
            ("app_macos.rs", include_str!("app_macos.rs")),
            ("browser_macos.rs", include_str!("browser_macos.rs")),
            ("settings_macos.rs", include_str!("settings_macos.rs")),
            ("address.rs", include_str!("address.rs")),
            ("passkeys.rs", include_str!("passkeys.rs")),
            ("main.rs", include_str!("main.rs")),
        ] {
            let code = source.split("#[cfg(test)]").next().unwrap();
            for touch in [
                "authority_is_trusted",
                "trust_in_login_keychain",
                "add-trusted-cert",
                "remove-trusted-cert",
                "SecTrustSettings",
                "/usr/bin/security",
                "AuthorizationRight",
            ] {
                assert!(!code.contains(touch), "{file} has {touch}");
            }
        }
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
