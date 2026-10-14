//! Settings: the search engine, and Clear Browsing Data.
//!
//! The search engine is stored in the application's user defaults, under
//! [`SEARCH_KEY`], and read each time something is typed in an address
//! field, so a change applies at once to every window. Nothing is sent to an
//! engine when it is chosen.
//!
//! Clear Browsing Data empties both stores a page can leave something in:
//! WebKit's website data — its cache, cookies and storage, one store shared
//! by every window and kept from one launch to the next — and the cache of
//! this application's proxy, which it asks to clear down the pipe only it
//! holds (`syndeo_webkit::owned::ProxyControl`). Every window stops loading
//! first, and the proxy keeps nothing that was asked for before the clear,
//! so a page still arriving cannot put back what was cleared.

use crate::address::Search;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSBackingStoreType, NSButton,
    NSModalResponse, NSPopUpButton, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSDate, NSPoint, NSRect, NSSize, NSString, NSTimer, NSUserDefaults,
};
use objc2_web_kit::WKWebsiteDataStore;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use syndeo_webkit::owned::ProxyControl;

/// The user defaults key the search engine is stored under.
pub const SEARCH_KEY: &str = "SearchEngine";

/// The search engine chosen in Settings.
pub fn search() -> Search {
    let stored =
        NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str(SEARCH_KEY));
    Search::from_setting(stored.map(|id| id.to_string()).as_deref())
}

/// How long the proxy has to say it cleared its cache.
const PROXY_CLEAR: Duration = Duration::from_secs(30);

/// What clearing needs from the application.
pub struct Clearing {
    /// Stop every window's page loading.
    pub stop_loading: Box<dyn Fn()>,
    /// The proxy's pipe, when this application has a proxy.
    pub proxy: Option<ProxyControl>,
}

pub struct Ivars {
    window: RefCell<Option<Retained<NSWindow>>>,
    popup: RefCell<Option<Retained<NSPopUpButton>>>,
    clear: RefCell<Option<Retained<NSButton>>>,
    status: RefCell<Option<Retained<NSTextField>>>,
    clearing: RefCell<Option<Clearing>>,
    /// A clear has started and not finished.
    busy: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoSettingsController"]
    #[ivars = Ivars]
    pub struct SettingsController;

    unsafe impl NSObjectProtocol for SettingsController {}

    impl SettingsController {
        /// The pop-up's action: store the engine chosen.
        #[unsafe(method(searchChosen:))]
        fn search_chosen(&self, _sender: Option<&AnyObject>) {
            let Some(popup) = self.ivars().popup.borrow().clone() else {
                return;
            };
            let Some(search) = usize::try_from(popup.indexOfSelectedItem())
                .ok()
                .and_then(|index| Search::ALL.get(index).copied())
            else {
                return;
            };
            let defaults = NSUserDefaults::standardUserDefaults();
            unsafe {
                defaults.setObject_forKey(
                    Some(&NSString::from_str(search.id())),
                    &NSString::from_str(SEARCH_KEY),
                )
            };
            tracing::info!(engine = search.id(), "search engine chosen");
        }

        /// Clear Browsing Data…: ask, then clear.
        #[unsafe(method(clearBrowsingData:))]
        fn clear_browsing_data(&self, _sender: Option<&AnyObject>) {
            if self.ivars().busy.get() {
                return;
            }
            let Some(window) = self.ivars().window.borrow().clone() else {
                return;
            };
            let alert = NSAlert::new(self.mtm());
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.setMessageText(ns_string!("Clear browsing data?"));
            alert.setInformativeText(ns_string!(
                "This removes cookies, website storage and cached pages from every Syndeo \
                 window, and empties Syndeo's own cache. Websites you are signed in to will \
                 sign you out."
            ));
            let clear = alert.addButtonWithTitle(ns_string!("Clear"));
            clear.setAccessibilityIdentifier(Some(ns_string!("syndeo.clear-confirm")));
            let cancel = alert.addButtonWithTitle(ns_string!("Cancel"));
            cancel.setAccessibilityIdentifier(Some(ns_string!("syndeo.clear-cancel")));
            let me = objc2::rc::Weak::from_retained(&objc2::Message::retain(self));
            let answered = block2::RcBlock::new(move |response: NSModalResponse| {
                if response == NSAlertFirstButtonReturn {
                    if let Some(me) = me.load() {
                        me.clear();
                    }
                }
            });
            alert.beginSheetModalForWindow_completionHandler(&window, Some(&answered));
        }
    }
);

impl SettingsController {
    pub fn new(mtm: MainThreadMarker, clearing: Clearing) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            window: RefCell::new(None),
            popup: RefCell::new(None),
            clear: RefCell::new(None),
            status: RefCell::new(None),
            clearing: RefCell::new(Some(clearing)),
            busy: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Bring the Settings window to the front, made the first time.
    pub fn show(&self) {
        let window = self.ivars().window.borrow().clone();
        let window = window.unwrap_or_else(|| self.make_window());
        if let Some(popup) = &*self.ivars().popup.borrow() {
            let index = Search::ALL.iter().position(|&s| s == search()).unwrap_or(0);
            popup.selectItemAtIndex(index as isize);
        }
        window.makeKeyAndOrderFront(None);
    }

    fn make_window(&self) -> Retained<NSWindow> {
        let mtm = self.mtm();
        let target: &AnyObject = self.as_ref();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(480.0, 280.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Settings"));
        let content = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(480.0, 280.0)),
        );

        let label = NSTextField::labelWithString(ns_string!("Search engine:"), mtm);
        label.setFrame(NSRect::new(
            NSPoint::new(20.0, 234.0),
            NSSize::new(110.0, 20.0),
        ));
        let popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(134.0, 228.0), NSSize::new(320.0, 28.0)),
            false,
        );
        for search in Search::ALL {
            popup.addItemWithTitle(&NSString::from_str(search.title()));
        }
        unsafe {
            popup.setTarget(Some(target));
            popup.setAction(Some(sel!(searchChosen:)));
        }
        popup.setAccessibilityIdentifier(Some(ns_string!("syndeo.search")));
        let about_search = NSTextField::wrappingLabelWithString(
            ns_string!(
                "Words typed in the address field are sent to the search engine when you \
                 press Return, and only then. Syndeo makes no suggestions, so nothing is \
                 sent while you type."
            ),
            mtm,
        );
        about_search.setFrame(NSRect::new(
            NSPoint::new(20.0, 168.0),
            NSSize::new(440.0, 50.0),
        ));

        let clear = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Clear Browsing Data…"),
                Some(target),
                Some(sel!(clearBrowsingData:)),
                mtm,
            )
        };
        clear.setFrame(NSRect::new(
            NSPoint::new(16.0, 124.0),
            NSSize::new(200.0, 32.0),
        ));
        clear.setAccessibilityIdentifier(Some(ns_string!("syndeo.clear")));
        let about_clear = NSTextField::wrappingLabelWithString(
            ns_string!(
                "Removes cookies, website storage and cached pages from every Syndeo window, \
                 and empties Syndeo's own cache."
            ),
            mtm,
        );
        about_clear.setFrame(NSRect::new(
            NSPoint::new(20.0, 72.0),
            NSSize::new(440.0, 44.0),
        ));
        let status = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        status.setFrame(NSRect::new(
            NSPoint::new(20.0, 20.0),
            NSSize::new(440.0, 44.0),
        ));
        status.setAccessibilityIdentifier(Some(ns_string!("syndeo.clear-status")));

        for view in [
            &*label as &NSView,
            &*popup,
            &*about_search,
            &*clear,
            &*about_clear,
            &*status,
        ] {
            content.addSubview(view);
        }
        window.setContentView(Some(&content));
        window.center();
        *self.ivars().window.borrow_mut() = Some(window.clone());
        *self.ivars().popup.borrow_mut() = Some(popup);
        *self.ivars().clear.borrow_mut() = Some(clear);
        *self.ivars().status.borrow_mut() = Some(status);
        window
    }

    fn say(&self, text: &str) {
        if let Some(status) = &*self.ivars().status.borrow() {
            status.setStringValue(&NSString::from_str(text));
        }
    }

    /// Clear WebKit's website data and the proxy's cache, at once, and say
    /// when both are done and how each went.
    fn clear(&self) {
        let mtm = self.mtm();
        self.ivars().busy.set(true);
        if let Some(clear) = &*self.ivars().clear.borrow() {
            clear.setEnabled(false);
        }
        self.say("Clearing…");
        let proxy = match &*self.ivars().clearing.borrow() {
            Some(clearing) => {
                (clearing.stop_loading)();
                clearing.proxy.clone()
            }
            None => None,
        };

        // The proxy's cache, on a thread of its own: it can take a moment,
        // and the window keeps answering meanwhile.
        let proxy_done: Arc<Mutex<Option<Result<u64, String>>>> = Arc::new(Mutex::new(None));
        match proxy {
            Some(proxy) => {
                let done = proxy_done.clone();
                let spawned = std::thread::Builder::new()
                    .name("clear-proxy-cache".into())
                    .spawn(move || {
                        let cleared = proxy
                            .clear_cache(PROXY_CLEAR)
                            .map_err(|err| format!("{err:#}"));
                        *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(cleared);
                    });
                if spawned.is_err() {
                    *proxy_done.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Err("Syndeo could not start clearing its cache".into()));
                }
            }
            None => {
                *proxy_done.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Err("Syndeo's proxy is not running".into()));
            }
        }

        // WebKit's website data: everything, from any time.
        let webkit_done = Rc::new(Cell::new(false));
        let done = webkit_done.clone();
        let finished = block2::RcBlock::new(move || done.set(true));
        unsafe {
            let store = WKWebsiteDataStore::defaultDataStore(mtm);
            store.removeDataOfTypes_modifiedSince_completionHandler(
                &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
                &NSDate::distantPast(),
                &finished,
            );
        }

        // Both, when both are done.
        let me = objc2::rc::Weak::from_retained(&objc2::Message::retain(self));
        let waiting = block2::RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| {
            if !webkit_done.get() {
                return;
            }
            let Some(proxy) = proxy_done.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                return;
            };
            unsafe { timer.as_ref() }.invalidate();
            if let Some(me) = me.load() {
                me.finished(proxy);
            }
        });
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &waiting) };
    }

    fn finished(&self, proxy: Result<u64, String>) {
        match &proxy {
            Ok(entries) => {
                tracing::info!(
                    entries,
                    "browsing data cleared: WebKit's website data and the proxy's cache"
                );
                self.say(
                    "Browsing data cleared: website data in every window, and Syndeo's cache.",
                );
            }
            Err(why) => {
                tracing::warn!("WebKit's website data cleared; the proxy's cache was not: {why}");
                self.say(&format!(
                    "Website data was cleared, but Syndeo's cache was not: {why}. Quit Syndeo \
                     and open it again, then clear again."
                ));
            }
        }
        self.ivars().busy.set(false);
        if let Some(clear) = &*self.ivars().clear.borrow() {
            clear.setEnabled(true);
        }
    }
}
