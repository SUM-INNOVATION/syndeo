//! A browser window's controller: one per window, and three things at once.
//!
//! - **The web view's navigation delegate.** It answers every TLS challenge
//!   exactly as `syndeo_webkit::pin` does, with the proxy's authority as the
//!   only anchor; lets a window's main frame show only the web's own pages
//!   and Syndeo's (`address::allowed_in_main_frame`), and no IPv6 address
//!   with a zone, which it refuses as the address field does; keeps the
//!   window's controls in step with loading; says in the bar how the page on
//!   screen was reached (`address::route`), from its address once it is
//!   committed, after any redirect; and shows a failure page when a page
//!   cannot be opened.
//! - **Its UI delegate.** A page that asks for a new window gets a new tab,
//!   made the way every other is, never a web view WebKit made for it.
//! - **The window's delegate**, so a window that closes is let go.
//!
//! Menu items with no target reach it through the responder chain, so Back,
//! Forward, Reload, Stop and Open Location work on the window in front, and
//! a window that cannot browse has no controller with a web view to do them,
//! so they are disabled there.

use crate::address::{self, Route, Search, Typed};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSButton, NSColor, NSControlSize,
    NSControlTextEditingDelegate, NSFont, NSImage, NSMenuItem, NSMenuItemValidation,
    NSProgressIndicator, NSProgressIndicatorStyle, NSTextAlignment, NSTextField,
    NSTextFieldDelegate, NSView, NSWindow, NSWindowDelegate,
};
use objc2_foundation::{
    ns_string, NSError, NSNotification, NSNumber, NSPoint, NSRect, NSSize, NSString,
    NSURLAuthenticationChallenge, NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKUIDelegate,
    WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use syndeo_webkit::pin;

/// The height of the bar above the page.
pub const BAR: f64 = 36.0;

/// The width of the bar's route, at its right, before the spinner.
const ROUTE_WIDTH: f64 = 190.0;

/// Open a tab beside a window: a page's request, or the start page.
pub type OpenTab = Box<dyn Fn(&NSWindow, Option<Retained<NSURLRequest>>)>;

/// What a controller asks of the application.
pub struct Hooks {
    /// Open a tab beside `window`: `request`, or the start page.
    pub open_tab: OpenTab,
    /// `window` is closing; let it go.
    pub closed: Box<dyn Fn(&NSWindow)>,
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
}

/// `NSReturnTextMovement`: the address field was left by pressing Return.
const RETURN: isize = 0x10;

/// `NSURLErrorCancelled`: a load stopped by Stop, or replaced by another.
const CANCELLED: isize = -999;

/// `WebKitErrorFrameLoadInterruptedByPolicyChange`: WebKit stopped a load
/// itself, for a file it does not show or an address a window may not open.
const INTERRUPTED: isize = 102;

/// What the bar's route says when someone points at it.
const ROUTE_HELP: &str = "How this page was reached. Through Syndeo, it came through Syndeo's \
    proxy, which may cache it; directly, WebKit opened it without the proxy, so Syndeo does \
    not cache it. Each image, script and other part of the page is reached by its own \
    address, so parts of it can go the other way.";

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
            self.refresh();
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, _webview: &WKWebView, _navigation: Option<&WKNavigation>) {
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
            if address::allowed_in_main_frame(&scheme_of(&request)) {
                if let (Some(window), Some(hooks)) = (self.window(), self.hooks()) {
                    (hooks.open_tab)(&window, Some(request));
                }
            }
            std::ptr::null_mut()
        }
    }

    unsafe impl NSWindowDelegate for BrowserController {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            // Taken out first: stopping may call back into this controller,
            // which then finds no web view and leaves it alone.
            let web = self.ivars().web.borrow_mut().take();
            if let Some(web) = web {
                unsafe {
                    web.view.stopLoading();
                    web.view.setNavigationDelegate(None);
                    web.view.setUIDelegate(None);
                }
            }
            let window = notification.object().and_then(|object| object.downcast::<NSWindow>().ok());
            if let (Some(window), Some(hooks)) = (window, self.hooks()) {
                (hooks.closed)(&window);
            }
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
            self.offers(item.action())
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
                (hooks.open_tab)(&window, None);
            }
        }
    }
);

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
        self.refresh();
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
    fn reload(&self) {
        let failed = self.ivars().failed.borrow().clone();
        match failed {
            Some(address) => self.go(&address),
            None => {
                if let Some(web) = &*self.ivars().web.borrow() {
                    unsafe { web.view.reload() };
                }
            }
        }
    }

    /// Stop the page loading, as Stop does.
    pub fn stop_loading(&self) {
        self.stop();
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
            let title = unsafe { web.view.title() }
                .map(|t| t.to_string())
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
