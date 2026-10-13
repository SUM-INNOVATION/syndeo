//! The application: AppKit, with WebKit in its windows.
//!
//! Everything a Mac user expects of an application comes from AppKit itself,
//! not from a window library: a Dock icon, the Syndeo, File, Edit and Window
//! menus with their usual shortcuts, a Dock click that brings a window back,
//! and closing the last window leaving the application running, as Mac
//! applications do, until it is quit.
//!
//! The web views are configured exactly as `syndeo-webkit`'s: their data
//! store sends HTTP and HTTPS through the proxy this application started, with
//! that proxy's per-launch credential, and only that proxy's authority is
//! accepted for TLS (`syndeo_webkit::{proxied, pin}`). The proxy is the one
//! helper, started once at launch from the bundle's `Contents/Helpers`
//! (`syndeo_webkit::owned`), and stopped when the application quits. If the
//! application dies without quitting, the proxy sees its stdin close and
//! exits on its own.
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
    NSEventModifierFlags, NSMenu, NSMenuItem, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSNotification, NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use syndeo_shell::supervisor::InstallDir;
use syndeo_webkit::{owned, pin, proxied};

/// The first-launch page, with `{{PROXY}}` and `{{TRUST}}` filled in.
const FIRST_LAUNCH: &str = include_str!("first_launch.html");

/// What starting the proxy came to, for the first-launch page to say.
enum Proxy {
    Running,
    Failed(String),
}

pub struct State {
    install: InstallDir,
    home: PathBuf,
    /// The proxy this application started, and owns until it quits.
    proxy: RefCell<Option<owned::OwnedProxy>>,
    /// Where the web views send their traffic, with the proxy's credential.
    target: RefCell<Option<proxied::ProxyTarget>>,
    /// The delegate that accepts the proxy's authority and no other.
    pinner: RefCell<Option<Retained<pin::Pinner>>>,
    started: RefCell<Option<Proxy>>,
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
            self.start_proxy();
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

        /// A Dock click with no window showing brings one back.
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

    /// Start the proxy from the bundle, and remember how to reach it.
    fn start_proxy(&self) {
        let state = self.ivars();
        let started = owned::start_proxy(state.install, &state.home).map(|(owned, credential)| {
            let endpoint = owned.endpoint;
            let target = proxied::ProxyTarget {
                host: endpoint.ip().to_string(),
                port: endpoint.port().to_string(),
                credential: Some(credential),
            };
            (owned, target, endpoint)
        });
        match started {
            Ok((owned, target, endpoint)) => {
                tracing::info!(proxy = %endpoint, pid = owned.pid(), "started this application's proxy");
                *state.proxy.borrow_mut() = Some(owned);
                *state.target.borrow_mut() = Some(target);
                *state.started.borrow_mut() = Some(Proxy::Running);
                match self.pin() {
                    Ok(pinner) => *state.pinner.borrow_mut() = Some(pinner),
                    Err(err) => tracing::warn!("not pinning the proxy's authority: {err:#}"),
                }
            }
            Err(err) => {
                tracing::error!("could not start the proxy: {err:#}");
                *state.started.borrow_mut() = Some(Proxy::Failed(format!("{err:#}")));
            }
        }
    }

    /// The delegate that accepts the proxy's authority, read from the home the
    /// proxy keeps it in.
    fn pin(&self) -> Result<Retained<pin::Pinner>> {
        let path = authority(&self.ivars().home);
        let pem = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        pin::Pinner::new(self.mtm(), &pem)
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
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1200.0, 800.0));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // Closed windows are kept and brought back, not freed under us.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Syndeo"));

        let configuration = match &*state.target.borrow() {
            Some(target) => proxied::configuration(target, mtm).unwrap_or_else(|err| {
                tracing::error!("could not configure the web view's proxy: {err:#}");
                unsafe { WKWebViewConfiguration::new(mtm) }
            }),
            None => unsafe { WKWebViewConfiguration::new(mtm) },
        };
        let webview = unsafe {
            WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &configuration)
        };
        if let Some(pinner) = &*state.pinner.borrow() {
            let delegate = pin::as_delegate(pinner);
            unsafe { webview.setNavigationDelegate(Some(&delegate)) };
        }
        let page = NSString::from_str(&self.first_launch_page());
        unsafe { webview.loadHTMLString_baseURL(&page, None) };

        window.setContentView(Some(&webview));
        window.center();
        window.makeKeyAndOrderFront(None);
        state.windows.borrow_mut().push(window);
    }

    /// The first-launch page, saying what is running and what is trusted.
    fn first_launch_page(&self) -> String {
        let state = self.ivars();
        let proxy = match &*state.started.borrow() {
            Some(Proxy::Running) => "Syndeo's local proxy is running.".to_string(),
            Some(Proxy::Failed(err)) => format!("Syndeo could not start its local proxy: {err}"),
            None => "Syndeo's local proxy has not started.".to_string(),
        };
        let trust = if owned::authority_is_trusted(&authority(&state.home)) {
            "Syndeo's local certificate authority is trusted in your login keychain."
        } else {
            "Syndeo's local certificate authority is not trusted yet. Secure (https) websites \
             will not open until it is, and this preview of Syndeo cannot yet ask for that."
        };
        FIRST_LAUNCH
            .replace("{{PROXY}}", &escape(&proxy))
            .replace("{{TRUST}}", &escape(trust))
    }
}

/// Where the proxy keeps its authority's certificate, under `home`.
fn authority(home: &Path) -> PathBuf {
    home.join("proxy").join("syndeo-ca.pem")
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
            target: RefCell::new(None),
            pinner: RefCell::new(None),
            started: RefCell::new(None),
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

    #[test]
    fn the_page_is_given_text_never_markup() {
        assert_eq!(
            escape("<b>\"&\"</b>"),
            "&lt;b&gt;&quot;&amp;&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn the_first_launch_page_has_both_places_to_fill() {
        assert!(FIRST_LAUNCH.contains("{{PROXY}}"));
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
