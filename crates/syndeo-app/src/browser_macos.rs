//! A browser window's controller: one per window, and three things at once.
//!
//! - **The web view's navigation delegate.** It answers every TLS challenge
//!   exactly as `syndeo_webkit::pin` does, with the proxy's authority as the
//!   only anchor; lets a window's main frame show only the web's own pages
//!   and Syndeo's (`address::allowed_in_main_frame`), and no IPv6 address
//!   with a zone, which it refuses as the address field does; keeps the
//!   window's controls in step with loading; says in the bar how the page on
//!   screen was reached (`address::route`: Direct, Local or Through Syndeo),
//!   from its address once it is committed, after any redirect; and shows a
//!   failure page when a page cannot be opened.
//! - **Its UI delegate.** A page that asks for a new window gets a new tab,
//!   made the way every other is, never a web view WebKit made for it. A
//!   link followed with ⌘ or the middle button opens in a tab the same way,
//!   behind this one, or in front with ⇧, and with ⌘⌥ in a new window
//!   (`tabs::opening`), but only to an address a window may show.
//! - **The window's delegate**, so a window that closes is let go, and kept
//!   as a closed tab (`Snapshot`) to open again; and a tab from the last
//!   session opens its page when it is first shown.
//!
//! It also keeps its tab's badge (`tabs_macos::Badge`) in step: pinned, a
//! page loading, and the page's icon.
//!
//! Menu items with no target reach it through the responder chain, so Back,
//! Forward, Reload, Stop and Open Location work on the window in front, and
//! a window that cannot browse has no controller with a web view to do them,
//! so they are disabled there.

use crate::address::{self, Route, Search, Typed};
use crate::tabs;
use crate::tabs_macos::{self, Badge};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSButton, NSColor, NSControlSize,
    NSControlTextEditingDelegate, NSEventModifierFlags, NSFont, NSImage, NSMenuItem,
    NSMenuItemValidation, NSProgressIndicator, NSProgressIndicatorStyle, NSTextAlignment,
    NSTextField, NSTextFieldDelegate, NSView, NSWindow, NSWindowDelegate,
};
use objc2_foundation::{
    ns_string, NSError, NSKeyValueObservingOptions, NSNotification, NSNumber,
    NSObjectNSKeyValueObserverRegistration, NSPoint, NSRect, NSSize, NSString,
    NSURLAuthenticationChallenge, NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationType, WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use syndeo_webkit::pin;

/// The height of the bar above the page.
pub const BAR: f64 = 36.0;

/// The width of the bar's route, at its right, before the spinner.
const ROUTE_WIDTH: f64 = 190.0;

/// How a new tab starts.
pub enum Start {
    /// With the start page.
    Page,
    /// With a page's request.
    Request(Retained<NSURLRequest>),
    /// As a tab that was closed, or is being duplicated, was.
    Again(Snapshot),
    /// A tab from the last session: its address and title, its page opened
    /// when the tab is first shown.
    Later { address: String, title: String },
}

/// Where a new tab goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// After the tab it is opened from, and in front.
    Front,
    /// After the tab it is opened from, and any it opened behind it since,
    /// with that tab kept in front.
    Behind,
    /// A window of its own.
    Window,
}

impl Place {
    fn of(opening: tabs::Opening) -> Option<Place> {
        match opening {
            tabs::Opening::Here => None,
            tabs::Opening::Tab { front: true } => Some(Place::Front),
            tabs::Opening::Tab { front: false } => Some(Place::Behind),
            tabs::Opening::Window => Some(Place::Window),
        }
    }
}

/// A tab, as it was when it was closed or duplicated.
pub struct Snapshot {
    /// Its page's address, the address that failed to open, or nothing for
    /// the start page.
    pub address: String,
    pub title: String,
    pub pinned: bool,
    /// WebKit's own record of its history, for a web page, kept in memory
    /// and never written anywhere.
    state: Option<Retained<AnyObject>>,
}

/// Open a tab: from `parent`, after `after`, starting as `Start` says and
/// placed as `Place` says. The new tab's window, if one was made.
pub type OpenTab = Box<dyn Fn(&NSWindow, &NSWindow, Start, Place) -> Option<Retained<NSWindow>>>;

/// A window is closing, as the tab in the snapshot, if it browsed.
pub type OnClosed = Box<dyn Fn(&NSWindow, Option<Snapshot>)>;

/// What a controller asks of the application.
pub struct Hooks {
    /// Open a tab beside a window.
    pub open_tab: OpenTab,
    /// `window` is closing, as the tab in the snapshot, if it browsed; let
    /// it go.
    pub closed: OnClosed,
    /// The start page's HTML.
    pub start_page: Box<dyn Fn() -> String>,
    /// The search engine chosen in Settings, as it is now.
    pub search: Box<dyn Fn() -> Search>,
}

/// The web view a window browses in, and the delegate that pins it.
struct Web {
    view: Retained<WKWebView>,
    pinner: Retained<pin::Pinner>,
}

/// The controls above the page.
pub struct Ui {
    address: Retained<NSTextField>,
    back: Retained<NSButton>,
    forward: Retained<NSButton>,
    reload: Retained<NSButton>,
    spinner: Retained<NSProgressIndicator>,
    /// How the page on screen was reached: see [`Route`].
    route: Retained<NSTextField>,
}

#[derive(Default)]
pub struct Ivars {
    /// The window this controller is the delegate of.
    window: RefCell<Option<Weak<NSWindow>>>,
    web: RefCell<Option<Web>>,
    ui: RefCell<Option<Ui>>,
    hooks: RefCell<Option<Rc<Hooks>>>,
    /// The address shown for a page that failed to open, kept in the field
    /// while the failure page stands in for it.
    failed: RefCell<Option<String>>,
    loading: Cell<bool>,
    /// This controller refused the last navigation a page started, which
    /// WebKit may then report as a load it interrupted: the page stays as it
    /// was, and there is no failure to show.
    refused: Cell<bool>,
    /// The address the field shows when nobody is typing in it: the page's,
    /// as navigation reports it, an address that failed, or nothing for the
    /// start page. Kept apart from the field, so that what someone types is
    /// never overwritten, and the field can show it again when they leave.
    shown: RefCell<String>,
    /// Someone has typed in the address field, and has not yet pressed
    /// Return or left it.
    typing: Cell<bool>,
    /// The tab is pinned.
    pinned: Cell<bool>,
    /// A tab from the last session not yet shown: its address and title.
    later: RefCell<Option<(String, String)>>,
    /// The tab's badge.
    badge: RefCell<Option<Badge>>,
    /// The page's icon was looked for and not found: found again when the
    /// tab is shown.
    icon_missed: Cell<bool>,
    /// Which page is on screen, counted, so that an icon found for one page
    /// is never shown for the next.
    page: Cell<u64>,
    /// The last tab this one opened behind itself, after which the next goes.
    behind: RefCell<Option<Weak<NSWindow>>>,
}

/// `NSReturnTextMovement`: the address field was left by pressing Return.
const RETURN: isize = 0x10;

/// `NSURLErrorCancelled`: a load stopped by Stop, or replaced by another.
const CANCELLED: isize = -999;

/// `WebKitErrorFrameLoadInterruptedByPolicyChange`: WebKit stopped a load
/// itself, for a file it does not show or an address a window may not open.
const INTERRUPTED: isize = 102;

/// What the bar's route says when someone points at it.
const ROUTE_HELP: &str = "How this page was reached. Through Syndeo: through Syndeo's proxy, \
    which may cache it. Direct: this Mac's own loopback, which WebKit opens without the proxy, \
    so Syndeo does not cache it. Local: a local-network address, which WebKit may open \
    directly or through the proxy; either way Syndeo does not cache it. Each image, script \
    and other part of the page is classified by its own address, so parts of it can be \
    reached another way.";

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoBrowserController"]
    #[ivars = Ivars]
    pub struct BrowserController;

    unsafe impl NSObjectProtocol for BrowserController {}

    unsafe impl WKNavigationDelegate for BrowserController {
        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        fn did_receive_challenge(
            &self,
            _webview: &WKWebView,
            challenge: &NSURLAuthenticationChallenge,
            handler: &block2::DynBlock<dyn Fn(isize, *mut AnyObject)>,
        ) {
            // Only ever made with a pinner; without one, refuse.
            let (disposition, credential) = match &*self.ivars().web.borrow() {
                Some(web) => web.pinner.judge(challenge),
                None => (2, std::ptr::null_mut()),
            };
            handler.call((disposition, credential));
        }

        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            _webview: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let main_frame = unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() });
            let request = unsafe { action.request() };
            let scheme = scheme_of(&request);
            let target = address_of(&request);
            // Refused wherever it would load, as typing it is.
            let zoned = address::has_zone(&target);
            // A link followed with ⌘ or the middle button: elsewhere, as a
            // new tab is made, or, for an address no tab may show, here,
            // where it is refused as any other.
            if let Some(place) = Place::of(tabs::opening(click_of(action))) {
                if !zoned && self.open_beside(request.clone(), place) {
                    self.ivars().refused.set(main_frame);
                    handler.call((WKNavigationActionPolicy::Cancel,));
                    return;
                }
            }
            let allowed = (!main_frame || address::allowed_in_main_frame(&scheme)) && !zoned;
            if !allowed && main_frame {
                self.ivars().refused.set(true);
                tracing::info!(scheme = %scheme, zoned, "refused a page in a window");
            }
            handler.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
            if zoned && main_frame {
                self.show_page(&target, address::ZONED);
            }
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn did_start(&self, _webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().refused.set(false);
            self.ivars().loading.set(true);
            // Tabs this one opens behind it from the next page go after it.
            self.ivars().behind.borrow_mut().take();
            self.refresh();
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, _webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            // Another page: the last one's icon goes, and any still coming.
            self.ivars().page.set(self.ivars().page.get() + 1);
            if let Some(badge) = &*self.ivars().badge.borrow() {
                badge.set_icon(None);
            }
            self.refresh_address();
            self.refresh();
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, _webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().loading.set(false);
            // Again here: a page brought back from the back-forward cache
            // may not be committed anew.
            self.refresh_address();
            self.refresh();
            self.find_icon();
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, _webview: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.ivars().loading.set(false);
            let refused = error.code() == INTERRUPTED && self.ivars().refused.replace(false);
            if error.code() != CANCELLED && !refused {
                self.show_failure(error);
            }
            self.refresh();
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, _webview: &WKWebView, _navigation: Option<&WKNavigation>, _error: &NSError) {
            self.ivars().loading.set(false);
            self.refresh();
        }
    }

    unsafe impl WKUIDelegate for BrowserController {
        /// A page asking for a new window gets a new tab, made the way every
        /// tab is; WebKit is never handed a web view for it.
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _webview: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> *mut WKWebView {
            let request = unsafe { action.request() };
            self.open_beside(request, Place::Front);
            std::ptr::null_mut()
        }
    }

    unsafe impl NSWindowDelegate for BrowserController {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            // As it was, to open again, before anything of it goes.
            let snapshot = self.snapshot();
            // Taken out first: stopping may call back into this controller,
            // which then finds no web view and leaves it alone.
            let web = self.ivars().web.borrow_mut().take();
            if let Some(web) = web {
                unsafe {
                    web.view.removeObserver_forKeyPath(self, ns_string!("title"));
                    web.view.stopLoading();
                    web.view.setNavigationDelegate(None);
                    web.view.setUIDelegate(None);
                }
            }
            let window = notification.object().and_then(|object| object.downcast::<NSWindow>().ok());
            if let (Some(window), Some(badge)) = (&window, self.ivars().badge.borrow_mut().take()) {
                badge.detach(window);
            }
            if let (Some(window), Some(hooks)) = (window, self.hooks()) {
                (hooks.closed)(&window, snapshot);
            }
        }

        /// A tab is shown: one from the last session opens its page now;
        /// one opened behind finds its page's icon, which WebKit does not
        /// draw for a page never shown.
        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            let later = self.ivars().later.borrow_mut().take();
            if let Some((address, _)) = later {
                self.go(&address);
            } else if self.ivars().icon_missed.get() && !self.ivars().loading.get() {
                self.find_icon();
            }
        }

        /// A pinned tab closes only from itself, in front: not with Close
        /// Other Tabs, AppKit's or this one's, nor with the rest of its
        /// window, which AppKit closes by asking each tab in turn.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            let keep = self.ivars().pinned.get() && !sender.isKeyWindow();
            if keep {
                tracing::info!("a pinned tab kept open");
            }
            !keep
        }
    }

    unsafe impl NSControlTextEditingDelegate for BrowserController {
        /// Someone typed in the address field: until they press Return or
        /// leave it, nothing a page does overwrites what they typed.
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.ivars().typing.set(true);
        }

        /// The address field was left. By Return, `navigateTo:` opens what
        /// was typed; any other way, the field shows the page's address again.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, notification: &NSNotification) {
            let movement = notification
                .userInfo()
                .and_then(|info| info.objectForKey(ns_string!("NSTextMovement")))
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .map(|number| number.integerValue());
            self.ivars().typing.set(false);
            if movement != Some(RETURN) {
                self.show_address();
            }
        }
    }

    unsafe impl NSTextFieldDelegate for BrowserController {}

    unsafe impl NSMenuItemValidation for BrowserController {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.validate(item)
        }
    }

    impl BrowserController {
        #[unsafe(method(syndeoBack:))]
        fn back(&self, _sender: Option<&AnyObject>) {
            if let Some(web) = &*self.ivars().web.borrow() {
                unsafe { web.view.goBack() };
            }
        }

        #[unsafe(method(syndeoForward:))]
        fn forward(&self, _sender: Option<&AnyObject>) {
            if let Some(web) = &*self.ivars().web.borrow() {
                unsafe { web.view.goForward() };
            }
        }

        /// The bar's one button: Stop while loading, Reload otherwise.
        #[unsafe(method(syndeoReloadOrStop:))]
        fn reload_or_stop(&self, _sender: Option<&AnyObject>) {
            if self.ivars().loading.get() {
                self.stop();
            } else {
                self.reload();
            }
        }

        #[unsafe(method(reloadPage:))]
        fn reload_page(&self, _sender: Option<&AnyObject>) {
            self.reload();
        }

        #[unsafe(method(stopPage:))]
        fn stop_page(&self, _sender: Option<&AnyObject>) {
            self.stop();
        }

        /// The address field's action: Return.
        #[unsafe(method(navigateTo:))]
        fn navigate_to(&self, _sender: Option<&AnyObject>) {
            let typed = self.ivars().ui.borrow().as_ref().map(|ui| ui.address.stringValue().to_string());
            if let Some(typed) = typed {
                self.go(&typed);
            }
        }

        /// File > Open Location: the address field, with what is in it chosen.
        #[unsafe(method(openLocation:))]
        fn open_location(&self, _sender: Option<&AnyObject>) {
            if let (Some(ui), Some(window)) = (&*self.ivars().ui.borrow(), self.window()) {
                window.makeFirstResponder(Some(&ui.address));
                unsafe { ui.address.selectText(None) };
            }
        }

        /// File > New Tab, and the tab bar's button.
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&AnyObject>) {
            if let (Some(window), Some(hooks)) = (self.window(), self.hooks()) {
                (hooks.open_tab)(&window, &window, Start::Page, Place::Front);
            }
        }

        /// Window > Duplicate Tab: a tab after this one, as this one is,
        /// history and all, and not pinned.
        #[unsafe(method(syndeoDuplicateTab:))]
        fn duplicate_tab(&self, _sender: Option<&AnyObject>) {
            if let (Some(window), Some(hooks), Some(snapshot)) = (self.window(), self.hooks(), self.snapshot()) {
                let copy = Snapshot { pinned: false, ..snapshot };
                (hooks.open_tab)(&window, &window, Start::Again(copy), Place::Front);
                tracing::info!("a tab duplicated");
            }
        }

        /// Window > Pin Tab, or Unpin Tab.
        #[unsafe(method(syndeoTogglePin:))]
        fn toggle_pin(&self, _sender: Option<&AnyObject>) {
            self.set_pinned(!self.ivars().pinned.get());
        }

        /// A tab's own menu > Close Tab: that tab, pinned or not.
        #[unsafe(method(syndeoCloseTab:))]
        fn close_tab(&self, _sender: Option<&AnyObject>) {
            if let Some(window) = self.window() {
                window.close();
            }
        }

        /// A tab's own menu > Close Other Tabs: every other tab of this
        /// window that is not pinned.
        #[unsafe(method(syndeoCloseOtherTabs:))]
        fn close_other_tabs(&self, _sender: Option<&AnyObject>) {
            let closing = self.others_to_close();
            let tabs = self.window().map(|w| tabs_macos::tabs_of(&w).len()).unwrap_or(0);
            tracing::info!(tabs, closing = closing.len(), "other tabs closed");
            for window in closing {
                window.close();
            }
        }

        /// The page's title changed, which it may do at any time: the
        /// window's, and so its tab's, follows it.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            _key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut std::ffi::c_void,
        ) {
            self.refresh();
        }

        /// ⌘1 to ⌘8 show that tab, and ⌘9 the last: the item's tag is the
        /// digit.
        #[unsafe(method(syndeoSelectTab:))]
        fn select_tab(&self, sender: Option<&AnyObject>) {
            let digit = sender
                .and_then(|s| s.downcast_ref::<NSMenuItem>())
                .and_then(|item| usize::try_from(item.tag()).ok());
            let (Some(window), Some(digit)) = (self.window(), digit) else {
                return;
            };
            let tabs = tabs_macos::tabs_of(&window);
            if let Some(index) = tabs::chosen_tab(digit, tabs.len()) {
                tabs[index].makeKeyAndOrderFront(None);
            }
        }
    }
);

/// What WebKit says of a navigation, for `tabs::opening`.
fn click_of(action: &WKNavigationAction) -> tabs::Click {
    let kind = unsafe { action.navigationType() };
    let keys = unsafe { action.modifierFlags() };
    tabs::Click {
        followed: kind == WKNavigationType::LinkActivated
            || kind == WKNavigationType::FormSubmitted,
        command: keys.contains(NSEventModifierFlags::Command),
        shift: keys.contains(NSEventModifierFlags::Shift),
        option: keys.contains(NSEventModifierFlags::Option),
        buttons: unsafe { action.buttonNumber() },
    }
}

/// The controller of `window`, if it is a browser window.
pub fn controller_of(window: &NSWindow) -> Option<Retained<BrowserController>> {
    let delegate = window.delegate()?;
    let object: &AnyObject = (*delegate).as_ref();
    object
        .downcast_ref::<BrowserController>()
        .map(|c| c.retain())
}

/// Whether `window`'s tab is pinned.
fn pinned(window: &NSWindow) -> bool {
    controller_of(window).is_some_and(|c| c.ivars().pinned.get())
}

/// Whether `action` is one of the window's browsing actions, which a window
/// that cannot browse does not offer.
fn browsing_action(action: Option<Sel>) -> bool {
    action.is_some_and(|a| {
        [
            sel!(syndeoBack:),
            sel!(syndeoForward:),
            sel!(reloadPage:),
            sel!(stopPage:),
            sel!(openLocation:),
            sel!(syndeoDuplicateTab:),
            sel!(syndeoTogglePin:),
        ]
        .contains(&a)
    })
}

/// A request's URL, as text, or empty.
fn address_of(request: &NSURLRequest) -> String {
    request
        .URL()
        .and_then(|url| url.absoluteString())
        .map(|text| text.to_string())
        .unwrap_or_default()
}

/// The scheme of a request's URL, lowercase, or empty.
fn scheme_of(request: &NSURLRequest) -> String {
    request
        .URL()
        .and_then(|url| url.scheme())
        .map(|scheme| scheme.to_string().to_ascii_lowercase())
        .unwrap_or_default()
}

impl BrowserController {
    /// A controller for a window that browses in `view`, pinned by `pinner`.
    pub fn new(
        mtm: MainThreadMarker,
        view: Retained<WKWebView>,
        pinner: Retained<pin::Pinner>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        *this.ivars().web.borrow_mut() = Some(Web { view, pinner });
        this
    }

    /// A controller for `window`, which cannot browse: it opens tabs beside
    /// it and lets it go when it closes, and offers no browsing action.
    pub fn inert(mtm: MainThreadMarker, window: &NSWindow, hooks: Rc<Hooks>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        *this.ivars().window.borrow_mut() = Some(Weak::from_retained(&window.retain()));
        *this.ivars().hooks.borrow_mut() = Some(hooks);
        this
    }

    /// Make this the web view's navigation and UI delegate, and say whether
    /// the web view reports it as its navigation delegate afterwards.
    pub fn take_over(&self) -> bool {
        let Some(web) = &*self.ivars().web.borrow() else {
            return false;
        };
        let delegate: &ProtocolObject<dyn WKNavigationDelegate> = ProtocolObject::from_ref(self);
        unsafe {
            web.view.setNavigationDelegate(Some(delegate));
            web.view.setUIDelegate(Some(ProtocolObject::from_ref(self)));
        }
        unsafe { web.view.navigationDelegate() }.is_some_and(|set| {
            std::ptr::eq(
                Retained::as_ptr(&set).cast::<AnyObject>(),
                (self as *const Self).cast::<AnyObject>(),
            )
        })
    }

    /// Give the controller its window, the window's controls and the
    /// application's hooks.
    pub fn attach(&self, window: &NSWindow, ui: Ui, hooks: Rc<Hooks>) {
        *self.ivars().window.borrow_mut() = Some(Weak::from_retained(&window.retain()));
        *self.ivars().ui.borrow_mut() = Some(ui);
        *self.ivars().hooks.borrow_mut() = Some(hooks);
        *self.ivars().badge.borrow_mut() = Some(Badge::new(self.mtm(), window));
        if let Some(web) = &*self.ivars().web.borrow() {
            // Removed again when the window closes.
            unsafe {
                web.view.addObserver_forKeyPath_options_context(
                    self,
                    ns_string!("title"),
                    NSKeyValueObservingOptions::New,
                    std::ptr::null_mut(),
                )
            };
        }
        self.refresh();
    }

    /// Open `request` in a new tab, or window, beside this one, where
    /// `place` says, made as every tab is; but only an address a window may
    /// show. Whether it was opened.
    fn open_beside(&self, request: Retained<NSURLRequest>, place: Place) -> bool {
        if address::allowed_in_main_frame(&scheme_of(&request)) {
            if let (Some(window), Some(hooks)) = (self.window(), self.hooks()) {
                // Behind this tab, after the last it opened behind itself
                // that is still beside it.
                let last = self.ivars().behind.borrow().as_ref().and_then(Weak::load);
                let after = match (place, last) {
                    (Place::Behind, Some(last))
                        if tabs_macos::tabs_of(&window)
                            .iter()
                            .any(|w| std::ptr::eq(&**w, &*last)) =>
                    {
                        last
                    }
                    _ => window.clone(),
                };
                let opened = (hooks.open_tab)(&window, &after, Start::Request(request), place);
                if place == Place::Behind {
                    *self.ivars().behind.borrow_mut() =
                        opened.as_deref().map(|w| Weak::from_retained(&w.retain()));
                }
                return opened.is_some();
            }
        }
        false
    }

    /// The tabs Close Other Tabs closes: every other of this window's that
    /// is not pinned.
    fn others_to_close(&self) -> Vec<Retained<NSWindow>> {
        let Some(window) = self.window() else {
            return Vec::new();
        };
        tabs_macos::tabs_of(&window)
            .into_iter()
            .filter(|w| !std::ptr::eq(&**w, &*window) && !pinned(w))
            .collect()
    }

    pub fn is_pinned(&self) -> bool {
        self.ivars().pinned.get()
    }

    /// Pin the tab, or unpin it: pinned tabs come first in their window, in
    /// the order they were pinned; one unpinned goes first after them.
    pub fn set_pinned(&self, pin: bool) {
        self.ivars().pinned.set(pin);
        if let Some(badge) = &*self.ivars().badge.borrow() {
            badge.set_pinned(pin);
        }
        let Some(window) = self.window() else { return };
        let others: Vec<Retained<NSWindow>> = tabs_macos::tabs_of(&window)
            .into_iter()
            .filter(|w| !std::ptr::eq(&**w, &*window))
            .collect();
        let pinned_others = others.iter().filter(|w| pinned(w)).count();
        if let Some(beside) = others.first() {
            if tabs_macos::index_of(&window) != tabs::pinned_place(pinned_others) {
                tabs_macos::place(beside, &window, tabs::pinned_place(pinned_others));
                window.makeKeyAndOrderFront(None);
            }
        }
        tracing::info!(pinned = pin, "a tab pinned or unpinned");
    }

    /// The tab as it is: what to open it again with.
    pub fn snapshot(&self) -> Option<Snapshot> {
        let web = self.ivars().web.borrow();
        let web = web.as_ref()?;
        let pinned = self.ivars().pinned.get();
        if let Some((address, title)) = self.ivars().later.borrow().clone() {
            return Some(Snapshot {
                address,
                title,
                pinned,
                state: None,
            });
        }
        let title = unsafe { web.view.title() }
            .map(|t| t.to_string())
            .unwrap_or_default();
        if let Some(address) = self.ivars().failed.borrow().clone() {
            return Some(Snapshot {
                address,
                title,
                pinned,
                state: None,
            });
        }
        let page = unsafe { web.view.URL() }
            .and_then(|url| url.absoluteString())
            .map(|text| text.to_string())
            .filter(|text| tabs::savable(text));
        Some(match page {
            Some(address) => Snapshot {
                address,
                title,
                pinned,
                state: unsafe { web.view.interactionState() },
            },
            // The start page, or one of Syndeo's own.
            None => Snapshot {
                address: String::new(),
                title: String::new(),
                pinned,
                state: None,
            },
        })
    }

    /// Open the tab as `snapshot` has it: its history, through the same
    /// navigation policy as any page, or its address, as if typed, or the
    /// start page. Pinning it again is for whoever places it.
    pub fn restore(&self, snapshot: Snapshot) {
        match (&*self.ivars().web.borrow(), snapshot.state) {
            (Some(web), Some(state)) if !snapshot.address.is_empty() => {
                unsafe { web.view.setInteractionState(Some(&state)) };
            }
            _ if !snapshot.address.is_empty() => self.go(&snapshot.address),
            _ => self.show_start_page(),
        }
        if !snapshot.address.is_empty() {
            self.set_shown(&snapshot.address);
        }
    }

    /// A tab from the last session: it shows `address` and `title`, and
    /// opens the page when it is first shown.
    pub fn open_later(&self, address: String, title: String) {
        self.set_shown(&address);
        *self.ivars().later.borrow_mut() = Some((address, title));
        self.refresh();
    }

    /// The page's icon, in the tab's badge, if the page names one.
    fn find_icon(&self) {
        let web = self.ivars().web.borrow();
        let Some(web) = web.as_ref() else { return };
        let is_web = unsafe { web.view.URL() }
            .and_then(|url| url.scheme())
            .is_some_and(|scheme| {
                matches!(
                    scheme.to_string().to_ascii_lowercase().as_str(),
                    "http" | "https"
                )
            });
        if !is_web {
            return;
        }
        let page = self.ivars().page.get();
        let me = Weak::from_retained(&self.retain());
        tabs_macos::fetch_icon(self.mtm(), &web.view, move |icon| {
            let Some(me) = me.load() else { return };
            // Still the page it was found for.
            if me.ivars().page.get() == page {
                me.ivars().icon_missed.set(icon.is_none());
                if let Some(badge) = &*me.ivars().badge.borrow() {
                    badge.set_icon(icon.as_deref());
                    tracing::info!(found = icon.is_some(), "a page's icon, for its tab");
                }
            }
        });
    }

    /// Whether the window offers `item`, and its title: Pin Tab or Unpin
    /// Tab as the tab is; ⌘1 to ⌘9 where there is such a tab; Close Other
    /// Tabs where there is one to close; and the rest as [`Self::offers`].
    fn validate(&self, item: &NSMenuItem) -> bool {
        let action = item.action();
        if action == Some(sel!(syndeoTogglePin:)) {
            let title = if self.ivars().pinned.get() {
                "Unpin Tab"
            } else {
                "Pin Tab"
            };
            item.setTitle(&NSString::from_str(title));
        }
        if action == Some(sel!(syndeoSelectTab:)) {
            let count = self
                .window()
                .map(|w| tabs_macos::tabs_of(&w).len())
                .unwrap_or(0);
            return usize::try_from(item.tag())
                .is_ok_and(|digit| tabs::chosen_tab(digit, count).is_some());
        }
        if action == Some(sel!(syndeoCloseOtherTabs:)) {
            return !self.others_to_close().is_empty();
        }
        self.offers(action)
    }

    /// Whether the window offers the menu item that sends `action`: no
    /// browsing action where there is no web view, Back and Forward where
    /// there is somewhere to go, and Stop while a page is loading.
    fn offers(&self, action: Option<Sel>) -> bool {
        let web = self.ivars().web.borrow();
        let Some(web) = web.as_ref() else {
            return !browsing_action(action);
        };
        match action {
            Some(a) if a == sel!(syndeoBack:) => unsafe { web.view.canGoBack() },
            Some(a) if a == sel!(syndeoForward:) => unsafe { web.view.canGoForward() },
            Some(a) if a == sel!(stopPage:) => self.ivars().loading.get(),
            _ => true,
        }
    }

    /// Reload the page. The failure page stands in for a page that never
    /// opened, so reloading it means trying that page again.
    ///
    /// From the origin: someone pressing Reload wants the page as the site has
    /// it now, so the reload asks past every cache, Syndeo's proxy included,
    /// which then has to consult the origin (a 304 for what it holds, or a new
    /// answer) rather than serve its own fresh copy. A plain `reload` let the
    /// proxy answer from its cache without asking (PR #36's journey).
    fn reload(&self) {
        let failed = self.ivars().failed.borrow().clone();
        match failed {
            Some(address) => self.go(&address),
            None => {
                if let Some(web) = &*self.ivars().web.borrow() {
                    unsafe { web.view.reloadFromOrigin() };
                }
            }
        }
    }

    /// Stop the page loading, as Stop does.
    pub fn stop_loading(&self) {
        self.stop();
    }

    /// The window's web view, routed through the proxy and pinned by this
    /// controller; none in a window that cannot browse, or once it closed.
    pub fn web_view(&self) -> Option<Retained<WKWebView>> {
        self.ivars()
            .web
            .borrow()
            .as_ref()
            .map(|web| web.view.clone())
    }

    fn stop(&self) {
        if let Some(web) = &*self.ivars().web.borrow() {
            unsafe { web.view.stopLoading() };
        }
        self.ivars().loading.set(false);
        self.refresh();
    }

    fn hooks(&self) -> Option<Rc<Hooks>> {
        self.ivars().hooks.borrow().clone()
    }

    fn window(&self) -> Option<Retained<NSWindow>> {
        self.ivars().window.borrow().as_ref().and_then(Weak::load)
    }

    /// The search engine chosen in Settings.
    fn search(&self) -> Search {
        self.hooks()
            .map(|hooks| (hooks.search)())
            .unwrap_or(Search::Off)
    }

    /// Open the start page.
    pub fn show_start_page(&self) {
        let page = self
            .hooks()
            .map(|hooks| (hooks.start_page)())
            .unwrap_or_default();
        if let Some(web) = &*self.ivars().web.borrow() {
            unsafe {
                web.view
                    .loadHTMLString_baseURL(&NSString::from_str(&page), None)
            };
        }
        *self.ivars().failed.borrow_mut() = None;
        self.set_shown("");
        self.show_route(None);
    }

    /// Load `request`, as a page asked for it.
    pub fn load(&self, request: &NSURLRequest) {
        if let Some(web) = &*self.ivars().web.borrow() {
            unsafe { web.view.loadRequest(request) };
        }
    }

    /// Say why `address`, which another application asked this window to
    /// open, is refused: Syndeo's page, with the words typing it gets.
    pub fn refuse(&self, address: &str, why: &str) {
        self.show_page(address, why);
    }

    /// Open what was typed, or say why not.
    fn go(&self, typed: &str) {
        // What was typed has been given: it is no longer being typed.
        self.ivars().typing.set(false);
        let search = self.search();
        let url = match address::interpret(typed, search) {
            Typed::Empty => return self.show_address(),
            Typed::Refused(why) => return self.show_page(typed, &why),
            Typed::Load(url) => url,
            Typed::Search(url) => {
                // Which engine, and nothing of what was asked.
                tracing::info!(engine = search.id(), "a search, on Return");
                url
            }
        };
        *self.ivars().failed.borrow_mut() = None;
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) else {
            return self.show_page(typed, "That address could not be read.");
        };
        let request = NSURLRequest::requestWithURL(&nsurl);
        self.set_shown(url.as_str());
        self.load(&request);
    }

    /// The failure page for an error WebKit reported.
    fn show_failure(&self, error: &NSError) {
        let failing: Option<Retained<NSString>> = error
            .userInfo()
            .objectForKey(ns_string!("NSErrorFailingURLStringKey"))
            .and_then(|value| value.downcast::<NSString>().ok());
        let address = failing.map(|s| s.to_string()).unwrap_or_default();
        let why = if address::has_zone(&address) {
            address::ZONED.to_string()
        } else if error.domain().to_string() == "WebKitErrorDomain" && error.code() == INTERRUPTED {
            "Syndeo opens web pages. It does not download files or open other kinds of address."
                .to_string()
        } else {
            error.localizedDescription().to_string()
        };
        self.show_page(&address, &why);
    }

    /// Show the failure page for `address`, keeping it in the field. The bar
    /// says nothing of a route: no page came from there, either way.
    fn show_page(&self, address: &str, why: &str) {
        *self.ivars().failed.borrow_mut() = (!address.is_empty()).then(|| address.to_string());
        if let Some(web) = &*self.ivars().web.borrow() {
            let page = address::failure_page(address, why);
            unsafe {
                web.view
                    .loadHTMLString_baseURL(&NSString::from_str(&page), None)
            };
        }
        self.set_shown(address);
        self.show_route(None);
    }

    /// Say in the bar how the page on screen was reached, or nothing for one
    /// of Syndeo's own pages.
    fn show_route(&self, route: Option<Route>) {
        if let Some(ui) = &*self.ivars().ui.borrow() {
            let label = route.map(Route::label).unwrap_or_default();
            ui.route.setStringValue(&NSString::from_str(label));
            ui.route.setHidden(route.is_none());
        }
    }

    /// Keep `text` as the address to show, and show it, unless someone is
    /// typing in the field.
    fn set_shown(&self, text: &str) {
        *self.ivars().shown.borrow_mut() = text.to_string();
        self.show_address();
    }

    /// The address field shows the address kept for it, unless someone is
    /// typing there: then it is left as they have it.
    fn show_address(&self) {
        if self.ivars().typing.get() {
            return;
        }
        if let Some(ui) = &*self.ivars().ui.borrow() {
            ui.address
                .setStringValue(&NSString::from_str(&self.ivars().shown.borrow()));
        }
    }

    /// The page's address, for the web's own pages, kept as the field's and
    /// shown unless someone is typing there, with its route in the bar;
    /// Syndeo's own pages leave the kept address as it is (empty, or the
    /// address that failed).
    fn refresh_address(&self) {
        let web = self.ivars().web.borrow();
        let Some(web) = web.as_ref() else { return };
        let Some(url) = (unsafe { web.view.URL() }) else {
            return;
        };
        let Some(text) = url.absoluteString() else {
            return;
        };
        let text = text.to_string();
        let scheme = url
            .scheme()
            .map(|s| s.to_string().to_ascii_lowercase())
            .unwrap_or_default();
        if scheme == "http" || scheme == "https" {
            *self.ivars().failed.borrow_mut() = None;
            self.set_shown(&text);
            // From the committed address, which is where any redirect led.
            self.show_route(address::route_of(&text));
        } else {
            // A page that is not the web's, which is reached no way at all.
            self.show_route(None);
        }
    }

    /// The controls, as the page is now: Back and Forward, Reload or Stop,
    /// the spinner, and the window's title.
    fn refresh(&self) {
        let web = self.ivars().web.borrow();
        let ui = self.ivars().ui.borrow();
        let (Some(web), Some(ui)) = (web.as_ref(), ui.as_ref()) else {
            return;
        };
        let loading = self.ivars().loading.get();
        if let Some(badge) = &*self.ivars().badge.borrow() {
            badge.set_loading(loading);
        }
        ui.back.setEnabled(unsafe { web.view.canGoBack() });
        ui.forward.setEnabled(unsafe { web.view.canGoForward() });
        let (symbol, label) = if loading {
            ("xmark", "Stop")
        } else {
            ("arrow.clockwise", "Reload")
        };
        if let Some(image) = symbol_image(symbol, label) {
            ui.reload.setImage(Some(&image));
        }
        ui.reload.setToolTip(Some(&NSString::from_str(label)));
        unsafe {
            if loading {
                ui.spinner.startAnimation(None);
            } else {
                ui.spinner.stopAnimation(None);
            }
        }
        if let Some(window) = self.window() {
            // A tab from the last session has its title until it opens.
            let later = self
                .ivars()
                .later
                .borrow()
                .as_ref()
                .map(|(_, title)| title.clone());
            let title = later
                .or_else(|| unsafe { web.view.title() }.map(|t| t.to_string()))
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Syndeo".to_string());
            window.setTitle(&NSString::from_str(&title));
        }
    }
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
fn symbol_image(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

/// A bar button: a symbol, described by `label`, sending `action` to `target`.
fn button(
    mtm: MainThreadMarker,
    symbol: &str,
    label: &str,
    target: &AnyObject,
    action: Sel,
    x: f64,
) -> Retained<NSButton> {
    let image = symbol_image(symbol, label).unwrap_or_default();
    let button =
        unsafe { NSButton::buttonWithImage_target_action(&image, Some(target), Some(action), mtm) };
    button.setFrame(NSRect::new(NSPoint::new(x, 6.0), NSSize::new(28.0, 24.0)));
    button.setToolTip(Some(&NSString::from_str(label)));
    button
}

/// The window's content: the bar of controls above `webview`, sized to
/// `frame`, with the controls sending to `controller`.
pub fn browser_content(
    mtm: MainThreadMarker,
    frame: NSRect,
    webview: &WKWebView,
    controller: &BrowserController,
) -> (Retained<NSView>, Ui) {
    let width = frame.size.width;
    let height = frame.size.height;
    let target: &AnyObject = controller.as_ref();

    let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
    let bar = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, height - BAR), NSSize::new(width, BAR)),
    );
    bar.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    let back = button(mtm, "chevron.left", "Back", target, sel!(syndeoBack:), 8.0);
    let forward = button(
        mtm,
        "chevron.right",
        "Forward",
        target,
        sel!(syndeoForward:),
        38.0,
    );
    let reload = button(
        mtm,
        "arrow.clockwise",
        "Reload",
        target,
        sel!(syndeoReloadOrStop:),
        68.0,
    );

    let address = NSTextField::textFieldWithString(ns_string!(""), mtm);
    address.setFrame(NSRect::new(
        NSPoint::new(104.0, 6.0),
        NSSize::new(width - 104.0 - 40.0 - ROUTE_WIDTH - 8.0, 24.0),
    ));
    address.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    address.setPlaceholderString(Some(ns_string!("Enter a web address")));
    // Only Return opens what was typed; leaving the field does not.
    if let Some(cell) = address.cell() {
        cell.setSendsActionOnEndEditing(false);
    }
    unsafe {
        // Told when someone types there, and when they leave.
        address.setDelegate(Some(ProtocolObject::from_ref(controller)));
        address.setTarget(Some(target));
        address.setAction(Some(sel!(navigateTo:)));
    }

    let route = NSTextField::labelWithString(ns_string!(""), mtm);
    route.setFrame(NSRect::new(
        NSPoint::new(width - 40.0 - ROUTE_WIDTH, 10.0),
        NSSize::new(ROUTE_WIDTH, 16.0),
    ));
    route.setAlignment(NSTextAlignment::Right);
    route.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    route.setTextColor(Some(&NSColor::secondaryLabelColor()));
    route.setToolTip(Some(&NSString::from_str(ROUTE_HELP)));
    route.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);
    route.setHidden(true);

    let spinner = NSProgressIndicator::initWithFrame(
        NSProgressIndicator::alloc(mtm),
        NSRect::new(NSPoint::new(width - 30.0, 10.0), NSSize::new(16.0, 16.0)),
    );
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setControlSize(NSControlSize::Small);
    spinner.setDisplayedWhenStopped(false);
    spinner.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

    // What Accessibility, and the UI test, find them by.
    for (control, name) in [
        (&*back as &NSView, "syndeo.back"),
        (&*forward, "syndeo.forward"),
        (&*reload, "syndeo.reload"),
        (&*address, "syndeo.address"),
        (&*route, "syndeo.route"),
    ] {
        control.setAccessibilityIdentifier(Some(&NSString::from_str(name)));
    }
    for control in [
        &*back as &NSView,
        &*forward,
        &*reload,
        &*address,
        &*route,
        &*spinner,
    ] {
        bar.addSubview(control);
    }

    webview.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(width, height - BAR),
    ));
    webview.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    container.addSubview(webview);
    container.addSubview(&bar);

    let ui = Ui {
        address,
        back,
        forward,
        reload,
        spinner,
        route,
    };
    (container, ui)
}
