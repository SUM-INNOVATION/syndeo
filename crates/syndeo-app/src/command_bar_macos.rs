//! The command bar: New Tab, as Arc has it. ⌘T, or the sidebar's New Tab,
//! opens not a page but a wide field floating over the middle of the
//! window, with the window behind it blurred and dimmed; what is typed
//! there — an address, or words to search for — opens in a new tab on
//! Return, and Escape, or a click anywhere else, puts the bar away with
//! nothing opened.
//!
//! The bar is two windows of its own: the shade, borderless and the size
//! of the browser window, which blurs and darkens what is behind it and
//! takes the click that dismisses; and the bar, keyed so that typing goes
//! to it. It sends what was typed to the application (`syndeoOpenTyped:`,
//! with the field as the sender), which opens the tab beside the window in
//! front, the way New Tab always did; the typed text is read by
//! `address::interpret`, as the address field's is.
//!
//! One bar at a time: asking again while it shows just brings it back.

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSFocusRingType, NSFont,
    NSImage, NSImageScaling, NSImageView, NSTextField, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSNotification, NSPoint, NSRect, NSSize};
use std::cell::RefCell;

/// The bar: its width at most, its margin from the window's sides when the
/// window is narrower, its height, and its corner radius. It sits a little
/// above the window's middle, where the eye rests.
const WIDTH: f64 = 640.0;
const MARGIN: f64 = 40.0;
const HEIGHT: f64 = 72.0;
const RADIUS: f64 = 16.0;
const ABOVE_MIDDLE: f64 = 60.0;

/// Inside: the field's inset, the symbol's size and the gap after it.
const INSET: f64 = 22.0;
const SYMBOL: f64 = 22.0;
const GAP: f64 = 12.0;

/// Above every ordinary window, as a floating panel is.
const FLOATING: isize = 3;

/// How dark the shade makes the window behind.
const DIM: f64 = 0.32;

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

define_class!(
    /// The dimming over the blur, which takes the click that dismisses.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoCommandBarShade"]
    pub struct Shade;

    impl Shade {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, DIM).setFill();
            NSBezierPath::fillRect(self.bounds());
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            dismiss();
        }
    }
);

pub struct BarIvars {
    window: Retained<BarWindow>,
    shade: Retained<NSWindow>,
    field: Retained<NSTextField>,
    /// The application, which opens what was typed.
    app: Weak<AnyObject>,
}

define_class!(
    /// The bar: its windows, its field, and what happens to what is typed.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoCommandBar"]
    #[ivars = BarIvars]
    pub struct CommandBar;

    unsafe impl NSObjectProtocol for CommandBar {}

    unsafe impl NSWindowDelegate for CommandBar {
        /// Another window took the keys: away, with nothing opened.
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
    bar.ivars().shade.orderFront(None);
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
        bar.ivars().shade.orderOut(None);
    }
}

/// A borderless, see-through window at `rect`, floating.
fn floating_window(mtm: MainThreadMarker, rect: NSRect) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setLevel(FLOATING);
    window.setRestorable(false);
    window
}

impl CommandBar {
    fn new(mtm: MainThreadMarker, over: &NSWindow, app: &AnyObject) -> Retained<Self> {
        let frame = over.frame();

        // The shade: the browser window's size, a blur of what is behind it
        // with a dimming over that.
        let shade = floating_window(mtm, frame);
        shade.setHasShadow(false);
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), bounds);
        blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
        blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        blur.setState(NSVisualEffectState::Active);
        let dim: Retained<Shade> = unsafe { msg_send![Shade::alloc(mtm), initWithFrame: bounds] };
        dim.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        blur.addSubview(&dim);
        shade.setContentView(Some(&blur));

        // The bar: in the middle, a little above it.
        let width = WIDTH.min(frame.size.width - 2.0 * MARGIN).max(240.0);
        let rect = NSRect::new(
            NSPoint::new(
                frame.origin.x + (frame.size.width - width) / 2.0,
                frame.origin.y + (frame.size.height - HEIGHT) / 2.0 + ABOVE_MIDDLE,
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
        let card_bounds = NSRect::new(NSPoint::new(0.0, 0.0), rect.size);
        let card = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), card_bounds);
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
            shade,
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
