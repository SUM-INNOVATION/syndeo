//! The command bar: New Tab, as Arc has it. ⌘T, or the sidebar's New Tab,
//! opens not a page but a wide field floating over the window; what is
//! typed there — an address, or words to search for — opens in a new tab
//! on Return, and Escape, or a click anywhere else, puts the bar away
//! with nothing opened.
//!
//! The bar is a window of its own, borderless and translucent, keyed so
//! that typing goes to it, placed over the upper part of the browser
//! window it was asked from. It sends what was typed to the application
//! (`syndeoOpenTyped:`, with the field as the sender), which opens the tab
//! beside the window in front, the way New Tab always did; the typed text
//! is read by `address::interpret`, as the address field's is.
//!
//! One bar at a time: asking again while it shows just brings it back.

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSColor, NSFocusRingType, NSFont, NSImage, NSImageScaling,
    NSImageView, NSTextField, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSNotification, NSPoint, NSRect, NSSize};
use std::cell::RefCell;

/// The bar: its width at most, its margin from the window's sides when the
/// window is narrower, its height, how far below the window's top it sits,
/// and its corner radius.
const WIDTH: f64 = 640.0;
const MARGIN: f64 = 40.0;
const HEIGHT: f64 = 72.0;
const BELOW_TOP: f64 = 120.0;
const RADIUS: f64 = 16.0;

/// Inside: the field's inset, the symbol's size and the gap after it.
const INSET: f64 = 22.0;
const SYMBOL: f64 = 22.0;
const GAP: f64 = 12.0;

/// Above every ordinary window, as a floating panel is.
const FLOATING: isize = 3;

thread_local! {
    /// The one bar, while it shows.
    static SHOWING: RefCell<Option<Retained<CommandBar>>> = const { RefCell::new(None) };
}

define_class!(
    /// The bar's window: borderless, and able to take the keyboard, which
    /// a borderless window is not by default. Escape puts it away.
    #[unsafe(super(NSWindow, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoCommandBarWindow"]
    pub struct BarWindow;

    impl BarWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            dismiss();
        }
    }
);

pub struct BarIvars {
    window: Retained<BarWindow>,
    field: Retained<NSTextField>,
    /// The application, which opens what was typed.
    app: Weak<AnyObject>,
}

define_class!(
    /// The bar: its window, its field, and what happens to what is typed.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoCommandBar"]
    #[ivars = BarIvars]
    pub struct CommandBar;

    unsafe impl NSObjectProtocol for CommandBar {}

    unsafe impl NSWindowDelegate for CommandBar {
        /// A click elsewhere: away, with nothing opened.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            dismiss();
        }
    }

    impl CommandBar {
        /// Return in the field: what was typed, to the application, then
        /// away.
        #[unsafe(method(syndeoCommandBarGo:))]
        fn go(&self, _sender: Option<&AnyObject>) {
            let typed = self.ivars().field.stringValue().to_string();
            dismiss();
            if typed.trim().is_empty() {
                return;
            }
            if let Some(app) = self.ivars().app.load() {
                let field: &AnyObject = self.ivars().field.as_ref();
                let application = objc2_app_kit::NSApplication::sharedApplication(self.mtm());
                unsafe {
                    application.sendAction_to_from(sel!(syndeoOpenTyped:), Some(&app), Some(field))
                };
            }
        }
    }
);

/// Show the bar over `over`, sending what is typed to `app`.
pub fn show(mtm: MainThreadMarker, over: &NSWindow, app: &AnyObject) {
    let showing = SHOWING.with(|slot| slot.borrow().clone());
    if let Some(bar) = showing {
        bar.ivars().window.makeKeyAndOrderFront(None);
        return;
    }
    let bar = CommandBar::new(mtm, over, app);
    SHOWING.with(|slot| *slot.borrow_mut() = Some(bar.clone()));
    bar.ivars().window.makeKeyAndOrderFront(None);
    let field: &NSView = &bar.ivars().field;
    bar.ivars().window.makeFirstResponder(Some(field));
}

/// Put the bar away, if it shows.
fn dismiss() {
    let showing = SHOWING.with(|slot| slot.borrow_mut().take());
    if let Some(bar) = showing {
        let window = &bar.ivars().window;
        window.setDelegate(None);
        window.orderOut(None);
    }
}

impl CommandBar {
    fn new(mtm: MainThreadMarker, over: &NSWindow, app: &AnyObject) -> Retained<Self> {
        let frame = over.frame();
        let width = WIDTH.min(frame.size.width - 2.0 * MARGIN).max(240.0);
        let rect = NSRect::new(
            NSPoint::new(
                frame.origin.x + (frame.size.width - width) / 2.0,
                frame.origin.y + frame.size.height - BELOW_TOP - HEIGHT,
            ),
            NSSize::new(width, HEIGHT),
        );
        let window: Retained<BarWindow> = unsafe {
            msg_send![
                BarWindow::alloc(mtm),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
        window.setLevel(FLOATING);
        window.setRestorable(false);

        // A translucent card, rounded, with the symbol and the field in it.
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), rect.size);
        let card = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), bounds);
        card.setMaterial(NSVisualEffectMaterial::Popover);
        card.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        card.setState(NSVisualEffectState::Active);
        crate::chrome_macos::round_corners(&card, RADIUS);

        let symbol = NSImageView::new(mtm);
        symbol.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("magnifyingglass"),
                Some(ns_string!("Search or open")),
            )
            .as_deref(),
        );
        symbol.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        symbol.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        symbol.setFrame(NSRect::new(
            NSPoint::new(INSET, (HEIGHT - SYMBOL) / 2.0),
            NSSize::new(SYMBOL, SYMBOL),
        ));
        card.addSubview(&symbol);

        let field = NSTextField::new(mtm);
        field.setBezeled(false);
        field.setDrawsBackground(false);
        field.setFocusRingType(NSFocusRingType::None);
        field.setFont(Some(&NSFont::systemFontOfSize(22.0)));
        field.setPlaceholderString(Some(ns_string!("Search or enter a web address")));
        field.setUsesSingleLineMode(true);
        let field_x = INSET + SYMBOL + GAP;
        let field_height = 30.0;
        field.setFrame(NSRect::new(
            NSPoint::new(field_x, (HEIGHT - field_height) / 2.0),
            NSSize::new(width - field_x - INSET, field_height),
        ));
        field.setAccessibilityIdentifier(Some(ns_string!("syndeo.command-bar")));
        card.addSubview(&field);
        window.setContentView(Some(&card));

        let this = Self::alloc(mtm).set_ivars(BarIvars {
            window: window.clone(),
            field: field.clone(),
            app: Weak::from_retained(&app.retain()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let target: &AnyObject = (*this).as_ref();
        unsafe {
            field.setTarget(Some(target));
            field.setAction(Some(sel!(syndeoCommandBarGo:)));
        }
        window.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this
    }
}

/// What was typed in the bar's field, when the application is sent
/// `syndeoOpenTyped:` with the field as the sender.
pub fn typed(sender: Option<&AnyObject>) -> Option<String> {
    let field = sender?.downcast_ref::<NSTextField>()?;
    let text = field.stringValue().to_string();
    (!text.trim().is_empty()).then_some(text)
}
