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
//! **Tabs** (`tabs`, `tabs_macos`): each is a window of its own, tabbed by
//! AppKit, placed here: a new one after the tab it came from, never among
//! the pinned ones, which come first. ⌘1 to ⌘9 choose one; a closed tab is
//! kept, in memory only, to open again from History, the last 25 of them;
//! Show All Tabs is AppKit's overview, and ⇧⌘\ opens it; AppKit's File >
//! Close Other Tabs, and closing a window, leave pinned tabs open; and a
//! tab's context menu is Syndeo's.
//! When Syndeo quits it keeps its tabs' addresses and titles, and which are
//! pinned (the session), in its own folder: pinned tabs come back at every
//! launch; the rest when macOS is set to keep windows when an application
//! quits, or from History > Reopen All Windows from Last Session. Clear
//! Browsing Data forgets the closed tabs and the session.
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
use crate::browser_macos::{
    browser_content, controller_of, BrowserController, Hooks, Place, Snapshot, Start,
};
use crate::settings_macos::{self, Clearing, SettingsController};
use crate::tabs::{self, Closed, Session};
use crate::tabs_macos;
use anyhow::{Context, Result};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSEventModifierFlags, NSMenu, NSMenuItem, NSMenuItemValidation, NSTextField, NSView, NSWindow,
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
}

impl Engine for WebKit {
    type Config = Retained<WKWebViewConfiguration>;
    type Pinner = Retained<pin::Pinner>;
    type WebView = Retained<WKWebView>;
    type View = Retained<NSView>;
    type Delegate = Retained<BrowserController>;

    fn configure(&self, target: &proxied::ProxyTarget) -> Result<Self::Config> {
        proxied::configuration(target, self.mtm)
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
    /// Tabs closed, to open again: in memory only.
    closed: RefCell<Closed<Snapshot>>,
    /// What History > Reopen All Windows from Last Session opens: the tabs
    /// kept when Syndeo last quit, but for any already opened.
    last_session: RefCell<Session>,
    /// History > Recently Closed's menu.
    recent: RefCell<Option<Retained<NSMenu>>>,
    /// What answers a secondary click on a tab, kept while it answers.
    tab_clicks: RefCell<Option<Retained<AnyObject>>>,
    /// Launch has finished, and decided whether this application can browse.
    launched: Cell<bool>,
    /// Addresses other applications asked this one to open before then.
    asked: RefCell<Vec<String>>,
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
            crate::passkeys::report();
            self.ivars().launched.set(true);
            // Opened to open addresses: those, and no start page beside them.
            let asked = self.ivars().asked.take();
            self.open_first_windows(asked.is_empty());
            for address in asked {
                self.open_asked(&address);
            }
            self.watch_tab_clicks();
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
            self.keep_session();
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

        /// History > Reopen Last Closed Tab.
        #[unsafe(method(syndeoReopenClosedTab:))]
        fn reopen_closed_tab(&self, _sender: Option<&AnyObject>) {
            let last = self.ivars().closed.borrow_mut().take_last();
            if let Some(snapshot) = last {
                self.reopen(snapshot);
            }
        }

        /// History > Recently Closed > a tab: the item's tag is its place in
        /// the list.
        #[unsafe(method(syndeoReopenRecent:))]
        fn reopen_recent(&self, sender: Option<&AnyObject>) {
            let index = sender
                .and_then(|s| s.downcast_ref::<NSMenuItem>())
                .and_then(|item| usize::try_from(item.tag()).ok());
            let taken = index.and_then(|i| self.ivars().closed.borrow_mut().take(i));
            if let Some(snapshot) = taken {
                self.reopen(snapshot);
            }
        }

        /// ⇧⌘\ and a tab's own menu: AppKit's own View > Show All Tabs,
        /// which it adds once there are tabs, pressed as a person presses it.
        /// Its overview opens that way, and was not seen to open when this
        /// application sent its action itself (CI 39695773041, 39725848720).
        #[unsafe(method(syndeoShowAllTabs:))]
        fn show_all_tabs(&self, _sender: Option<&AnyObject>) {
            let app = NSApplication::sharedApplication(self.mtm());
            let view = app
                .mainMenu()
                .and_then(|bar| bar.itemWithTitle(ns_string!("View")))
                .and_then(|item| item.submenu());
            let index = view.as_ref().and_then(|menu| {
                menu.itemArray()
                    .iter()
                    .position(|item| item.action() == Some(sel!(toggleTabOverview:)))
            });
            match (view, index) {
                (Some(menu), Some(index)) => menu.performActionForItemAtIndex(index as isize),
                _ => {
                    if let Some(window) = self.front_browser() {
                        window.toggleTabOverview(None);
                    }
                }
            }
        }

        /// History > Reopen All Windows from Last Session.
        #[unsafe(method(syndeoReopenSession:))]
        fn reopen_session(&self, _sender: Option<&AnyObject>) {
            let session = std::mem::take(&mut *self.ivars().last_session.borrow_mut());
            self.open_session(&session);
        }
    }

    unsafe impl NSMenuItemValidation for AppDelegate {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.offers(item)
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

    /// What every controller may ask of the application, made once.
    fn hooks(&self) -> Rc<Hooks> {
        if let Some(hooks) = self.ivars().hooks.borrow().clone() {
            return hooks;
        }
        let me = Weak::from_retained(&self.retain());
        let (tab, closed, start) = (me.clone(), me.clone(), me);
        let search: Box<dyn Fn() -> Search> = Box::new(settings_macos::search);
        let hooks = Rc::new(Hooks {
            open_tab: Box::new(move |parent, after, start, place| {
                tab.load()
                    .map(|app| app.open_tab(parent, after, start, place))
            }),
            closed: Box::new(move |window, snapshot| {
                if let Some(app) = closed.load() {
                    app.forget(window, snapshot);
                }
            }),
            start_page: Box::new(move || {
                start
                    .load()
                    .map(|app| app.first_launch_page())
                    .unwrap_or_default()
            }),
            search,
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
            forget_tabs: Box::new({
                let me = Weak::from_retained(&self.retain());
                move || {
                    if let Some(app) = me.load() {
                        app.forget_tabs();
                    }
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
            None => {
                self.open_window();
            }
        }
    }

    /// A new window, in front, with the start page.
    fn open_window(&self) -> Retained<NSWindow> {
        let window = self.new_window(Start::Page);
        window.center();
        window.makeKeyAndOrderFront(None);
        window
    }

    /// A new tab, from `parent`, after `after`, starting as `start` says,
    /// and placed as `place` says: never among the pinned tabs; behind,
    /// with `parent` kept in front.
    fn open_tab(
        &self,
        parent: &NSWindow,
        after: &NSWindow,
        start: Start,
        place: Place,
    ) -> Retained<NSWindow> {
        let window = self.new_window(start);
        if place == Place::Window {
            window.cascadeTopLeftFromPoint(NSPoint::new(
                parent.frame().origin.x,
                parent.frame().origin.y + parent.frame().size.height,
            ));
            window.makeKeyAndOrderFront(None);
            return window;
        }
        let pinned = tabs_macos::tabs_of(after)
            .iter()
            .filter(|w| controller_of(w).is_some_and(|c| c.is_pinned()))
            .count();
        let index = tabs::after_pinned(tabs_macos::index_of(after) + 1, pinned);
        tabs_macos::place(after, &window, index);
        match place {
            Place::Behind => parent.makeKeyAndOrderFront(None),
            _ => window.makeKeyAndOrderFront(None),
        }
        window
    }

    /// Open a closed tab again, beside the window in front, or as a window.
    fn reopen(&self, snapshot: Snapshot) {
        self.show_recent();
        let pinned = snapshot.pinned;
        let window = match self.front_browser() {
            Some(front) => self.open_tab(&front, &front, Start::Again(snapshot), Place::Front),
            None => {
                let window = self.new_window(Start::Again(snapshot));
                window.center();
                window.makeKeyAndOrderFront(None);
                window
            }
        };
        if pinned {
            if let Some(controller) = controller_of(&window) {
                controller.set_pinned(true);
            }
        }
        tracing::info!("a closed tab opened again");
    }

    /// Open the tabs of `session`, window by window, each window's tab in
    /// front opened now and the others when they are first shown.
    fn open_session(&self, session: &Session) {
        for saved in &session.windows {
            let mut opened: Vec<Retained<NSWindow>> = Vec::new();
            for tab in &saved.tabs {
                let start = Start::Later {
                    address: tab.address.clone(),
                    title: tab.title.clone(),
                };
                let window = self.new_window(start);
                match opened.last() {
                    Some(last) => tabs_macos::place(last, &window, opened.len()),
                    None => window.center(),
                }
                if tab.pinned {
                    if let Some(controller) = controller_of(&window) {
                        controller.set_pinned(true);
                    }
                }
                opened.push(window);
            }
            if let Some(front) = opened.get(saved.selected).or(opened.first()) {
                front.makeKeyAndOrderFront(None);
            }
        }
        tracing::info!(
            windows = session.windows.len(),
            "tabs from the last session opened"
        );
    }

    /// An address another application asked this one to open, in a new tab
    /// beside the browser window in front, or a new window if there is none:
    /// made by `new_window`, as every tab is, so it browses only through the
    /// proxy, pinned to its authority, and placed as every new tab is. One
    /// `address::asked` refuses gets Syndeo's page saying why. The log has
    /// its scheme, and nothing else of it.
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
        let start = request.map(Start::Request).unwrap_or(Start::Page);
        let window = match self.front_browser() {
            Some(parent) => self.open_tab(&parent, &parent, start, Place::Front),
            None => {
                let window = self.new_window(start);
                window.center();
                window.makeKeyAndOrderFront(None);
                window
            }
        };
        if let (Some(why), Some(controller)) = (refused, controller_of(&window)) {
            controller.refuse(address, &why);
        }
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

    /// At launch: the last session's tabs, if macOS keeps windows when an
    /// application quits; otherwise a window with the start page, unless
    /// Syndeo was opened to open addresses, and the last session's pinned
    /// tabs before it. What is not opened is kept for History > Reopen All
    /// Windows from Last Session.
    fn open_first_windows(&self, start_page: bool) {
        let state = self.ivars();
        let mut saved = tabs_macos::read_session(&state.home);
        if state.browsing.borrow().is_none() {
            if start_page {
                self.open_window();
            }
            return;
        }
        if tabs_macos::keeps_windows() && !saved.is_empty() {
            self.open_session(&saved);
            return;
        }
        let front = start_page.then(|| self.open_window());
        let pinned = saved.pinned();
        let mut first: Option<Retained<NSWindow>> = None;
        for (index, tab) in pinned.iter().enumerate() {
            let start = Start::Later {
                address: tab.address.clone(),
                title: tab.title.clone(),
            };
            let window = self.new_window(start);
            match front.as_ref().or(first.as_ref()) {
                Some(beside) => tabs_macos::place(beside, &window, index),
                None => window.center(),
            }
            if let Some(controller) = controller_of(&window) {
                controller.set_pinned(true);
            }
            first.get_or_insert(window);
        }
        // The start page in front, before the pinned tabs; or, with none,
        // the first pinned tab.
        match (&front, &first) {
            (Some(front), Some(_)) => front.makeKeyAndOrderFront(None),
            (None, Some(first)) => first.makeKeyAndOrderFront(None),
            _ => {}
        }
        if !pinned.is_empty() {
            for window in &mut saved.windows {
                window.tabs.retain(|tab| !tab.pinned);
            }
            saved = saved.kept();
        }
        *state.last_session.borrow_mut() = saved;
    }

    /// Every browser window's tabs, as the session keeps them.
    fn session(&self) -> Session {
        let windows: Vec<Retained<NSWindow>> = self
            .ivars()
            .windows
            .borrow()
            .iter()
            .map(|(window, _)| window.clone())
            .collect();
        let mut seen: Vec<*const NSWindow> = Vec::new();
        let mut session = Session::default();
        for window in &windows {
            if seen.contains(&Retained::as_ptr(window)) {
                continue;
            }
            let tabbed = tabs_macos::tabs_of(window);
            seen.extend(tabbed.iter().map(Retained::as_ptr));
            let front = window.tabGroup().and_then(|group| group.selectedWindow());
            let mut saved = tabs::SavedWindow::default();
            for (index, tab) in tabbed.iter().enumerate() {
                if front.as_deref().is_some_and(|f| std::ptr::eq(f, &**tab)) {
                    saved.selected = index;
                }
                let snapshot = controller_of(tab).and_then(|c| c.snapshot());
                saved.tabs.push(match snapshot {
                    Some(s) => tabs::SavedTab {
                        address: s.address,
                        title: s.title,
                        pinned: s.pinned,
                    },
                    None => tabs::SavedTab {
                        address: String::new(),
                        title: String::new(),
                        pinned: false,
                    },
                });
            }
            session.windows.push(saved);
        }
        session.kept()
    }

    /// As Syndeo quits: keep the session, or none if no tab has a page. A
    /// launch that could not browse had no tabs of its own, and leaves the
    /// last session as it was.
    fn keep_session(&self) {
        if self.ivars().browsing.borrow().is_none() {
            return;
        }
        let session = self.session();
        match tabs_macos::write_session(&self.ivars().home, &session) {
            Ok(()) => tracing::info!(windows = session.windows.len(), "the session kept"),
            Err(err) => tracing::warn!("the session could not be kept: {err:#}"),
        }
    }

    /// Clear Browsing Data: no closed tab to open again, and no session.
    fn forget_tabs(&self) {
        let state = self.ivars();
        state.closed.borrow_mut().clear();
        *state.last_session.borrow_mut() = Session::default();
        self.show_recent();
        match tabs_macos::forget_session(&state.home) {
            Ok(()) => tracing::info!("closed tabs and the session forgotten"),
            Err(err) => tracing::warn!("the session could not be forgotten: {err:#}"),
        }
    }

    /// History > Recently Closed, as the closed tabs are now.
    fn show_recent(&self) {
        let Some(menu) = self.ivars().recent.borrow().clone() else {
            return;
        };
        menu.removeAllItems();
        for (index, snapshot) in self.ivars().closed.borrow().iter().enumerate() {
            let entry = item(
                self.mtm(),
                &tabs::menu_title(&snapshot.title, &snapshot.address),
                sel!(syndeoReopenRecent:),
                "",
            );
            entry.setTag(index as isize);
            menu.addItem(&entry);
        }
    }

    /// Whether the menu item is offered: the History items where there is
    /// something to open.
    fn offers(&self, entry: &NSMenuItem) -> bool {
        let state = self.ivars();
        match entry.action() {
            Some(a) if a == sel!(syndeoReopenClosedTab:) => !state.closed.borrow().is_empty(),
            Some(a) if a == sel!(syndeoReopenRecent:) => {
                usize::try_from(entry.tag()).is_ok_and(|i| i < state.closed.borrow().len())
            }
            Some(a) if a == sel!(syndeoReopenSession:) => {
                !state.last_session.borrow().is_empty() && state.browsing.borrow().is_some()
            }
            _ => true,
        }
    }

    /// Answer a secondary click on a tab with Syndeo's menu for it.
    fn watch_tab_clicks(&self) {
        let me = Weak::from_retained(&self.retain());
        let monitor = tabs_macos::watch_tab_clicks(self.mtm(), move |window| {
            me.load().and_then(|app| app.tab_menu(window))
        });
        *self.ivars().tab_clicks.borrow_mut() = monitor;
    }

    /// A tab's context menu: for `window`'s tab, if it is a browser's.
    fn tab_menu(&self, window: &NSWindow) -> Option<Retained<NSMenu>> {
        let mtm = self.mtm();
        let controller = controller_of(window)?;
        let to_tab: &AnyObject = controller.as_ref();
        let to_window: &AnyObject = window.as_ref();
        let to_app: &AnyObject = self.as_ref();
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Tab"));
        let pin = if controller.is_pinned() {
            "Unpin Tab"
        } else {
            "Pin Tab"
        };
        for entry in [
            tabs_macos::item(
                mtm,
                "Duplicate Tab",
                sel!(syndeoDuplicateTab:),
                Some(to_tab),
            ),
            tabs_macos::item(mtm, pin, sel!(syndeoTogglePin:), Some(to_tab)),
            NSMenuItem::separatorItem(mtm),
            tabs_macos::item(mtm, "Close Tab", sel!(syndeoCloseTab:), Some(to_tab)),
            tabs_macos::item(
                mtm,
                "Close Other Tabs",
                sel!(syndeoCloseOtherTabs:),
                Some(to_tab),
            ),
            tabs_macos::item(
                mtm,
                "Move Tab to New Window",
                sel!(moveTabToNewWindow:),
                Some(to_window),
            ),
            NSMenuItem::separatorItem(mtm),
            tabs_macos::item(mtm, "Show All Tabs", sel!(syndeoShowAllTabs:), Some(to_app)),
        ] {
            menu.addItem(&entry);
        }
        tracing::info!("a tab's menu shown");
        Some(menu)
    }

    /// A window that closed: its controller already let go of its web view,
    /// and handed what it showed, to open again. Kept until the next window
    /// opens or closes, so nothing is freed while AppKit is still closing it.
    fn forget(&self, window: &NSWindow, snapshot: Option<Snapshot>) {
        let state = self.ivars();
        if let Some(snapshot) = snapshot {
            state.closed.borrow_mut().push(snapshot);
            self.show_recent();
        }
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
    fn new_window(&self, start: Start) -> Retained<NSWindow> {
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
        // Syndeo opens its tabs again itself (`open_first_windows`): macOS's
        // own window restoration is left out of it.
        window.setRestorable(false);
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
                match start {
                    Start::Page => controller.show_start_page(),
                    Start::Request(request) => controller.load(&request),
                    Start::Again(snapshot) => controller.restore(snapshot),
                    Start::Later { address, title } => controller.open_later(address, title),
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

/// One menu item, as [`item`] makes it, with `modifiers` and not Command
/// alone.
fn item_with(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    key: &str,
    modifiers: NSEventModifierFlags,
) -> Retained<NSMenuItem> {
    let entry = item(mtm, title, action, key);
    entry.setKeyEquivalentModifierMask(modifiers);
    entry
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
/// browsing to do. With it, History > Recently Closed's menu, which changes
/// as tabs close.
fn main_menu(
    mtm: MainThreadMarker,
    app: &NSApplication,
    delegate: &AnyObject,
) -> (Retained<NSMenu>, Retained<NSMenu>) {
    let command = NSEventModifierFlags::Command;
    // ⇧⌘\, Show All Tabs as Safari has it: AppKit's own item, which it adds
    // to View, has no shortcut. Not shown, beside it, and named apart from it
    // so that AppKit still adds its own.
    let show_all_tabs = item_with(
        mtm,
        "Tab Overview",
        sel!(syndeoShowAllTabs:),
        "\\",
        NSEventModifierFlags::Shift | command,
    );
    unsafe { show_all_tabs.setTarget(Some(delegate)) };
    show_all_tabs.setHidden(true);
    show_all_tabs.setAllowsKeyEquivalentWhenHidden(true);
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
            // With tabs, AppKit makes this Close Tab, and adds Close Window,
            // Close All and Close Other Tabs (⌥⌘W) beside it; a pinned tab
            // stays open through the last (`windowShouldClose:`).
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
            show_all_tabs,
        ],
    );
    let recent = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Recently Closed"));
    let recently_closed = NSMenuItem::new(mtm);
    recently_closed.setTitle(ns_string!("Recently Closed"));
    recently_closed.setSubmenu(Some(&recent));
    submenu(
        mtm,
        &bar,
        "History",
        &[
            item(mtm, "Back", sel!(syndeoBack:), "["),
            item(mtm, "Forward", sel!(syndeoForward:), "]"),
            NSMenuItem::separatorItem(mtm),
            item_with(
                mtm,
                "Reopen Last Closed Tab",
                sel!(syndeoReopenClosedTab:),
                "t",
                NSEventModifierFlags::Shift | command,
            ),
            item(
                mtm,
                "Reopen All Windows from Last Session",
                sel!(syndeoReopenSession:),
                "",
            ),
            recently_closed,
        ],
    );
    // ⌘1 to ⌘9, which choose a tab, as Safari's and every other browser's
    // do: in the menu, so they work as any shortcut does, but not shown.
    let choose_tab: Vec<Retained<NSMenuItem>> = (1..=9)
        .map(|digit| {
            let title = if digit == 9 {
                "Show Last Tab".to_string()
            } else {
                format!("Show Tab {digit}")
            };
            let entry = item(mtm, &title, sel!(syndeoSelectTab:), &digit.to_string());
            entry.setTag(digit);
            entry.setHidden(true);
            entry.setAllowsKeyEquivalentWhenHidden(true);
            entry
        })
        .collect();
    let window = submenu(
        mtm,
        &bar,
        "Window",
        &[
            item(mtm, "Minimize", sel!(performMiniaturize:), "m"),
            item(mtm, "Zoom", sel!(performZoom:), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Duplicate Tab", sel!(syndeoDuplicateTab:), ""),
            item(mtm, "Pin Tab", sel!(syndeoTogglePin:), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Bring All to Front", sel!(arrangeInFront:), ""),
        ],
    );
    for entry in &choose_tab {
        window.addItem(entry);
    }
    app.setWindowsMenu(Some(&window));
    (bar, recent)
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
            closed: RefCell::new(Closed::default()),
            last_session: RefCell::new(Session::default()),
            recent: RefCell::new(None),
            tab_clicks: RefCell::new(None),
            launched: Cell::new(false),
            asked: RefCell::new(Vec::new()),
        },
    );
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let (bar, recent) = main_menu(mtm, &app, delegate.as_ref());
    *delegate.ivars().recent.borrow_mut() = Some(recent);
    app.setMainMenu(Some(&bar));
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
            ("tabs_macos.rs", include_str!("tabs_macos.rs")),
            ("tabs.rs", include_str!("tabs.rs")),
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
