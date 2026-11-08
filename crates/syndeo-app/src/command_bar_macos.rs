//! The command bar: New Tab, as Arc has it. ⌘T, or the sidebar's New Tab,
//! opens not a page but a wide field floating over the middle of the
//! window, with the page behind it — and only the page: the sidebar stays
//! as it is — under a pane of glass; what is typed there — an address, or
//! words to search for — opens in a new tab on Return, and Escape, or a
//! click anywhere else, puts the bar away with nothing opened.
//!
//! Under the field, as the person types, are suggestions: open tabs whose
//! title or address matches, which Return brings to the front rather than
//! opening again, and favorites, which it opens. Up and Down move among
//! them; with none chosen, Return opens what was typed.
//!
//! The glass is macOS's own, `NSGlassEffectView`, on a Mac that has it
//! (macOS 26 and later); a translucent material stands in for it before
//! that. The shade over the page is the window's own view (`PageShade`,
//! kept by the window's chrome and shown by `browser_macos::set_shaded`),
//! so that it covers the page's card exactly, corners and all. The bar is
//! a window of its own, keyed so that typing goes to it. It sends what was
//! chosen to the application (`syndeoOpenTyped:`, with the field as the
//! sender), which opens the tab beside the window in front, the way New
//! Tab always did; the typed text is read by `address::interpret`, as the
//! address field's is.
//!
//! One bar at a time: asking again while it shows just brings it back.

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSBezierPath, NSColor, NSControl,
    NSControlTextEditingDelegate, NSEvent, NSFocusRingType, NSFont, NSImage, NSImageScaling,
    NSImageView, NSLineBreakMode, NSTextField, NSTextFieldDelegate, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSNotification, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The bar: its width at most, its margin from the window's sides when the
/// window is narrower, the field's height, and its corner radius. It sits a
/// little above the window's middle, where the eye rests.
const WIDTH: f64 = 640.0;
const MARGIN: f64 = 40.0;
const HEIGHT: f64 = 72.0;
const RADIUS: f64 = 18.0;
const ABOVE_MIDDLE: f64 = 60.0;

/// Inside: the field's inset, the symbol's size and the gap after it.
const INSET: f64 = 22.0;
const SYMBOL: f64 = 22.0;
const GAP: f64 = 12.0;

/// The suggestions: how many at most, a row's height, and the room above
/// the first and below the last.
const MOST: usize = 6;
const ROW: f64 = 44.0;
const LIST_PAD: f64 = 8.0;

/// Above every ordinary window, as a floating panel is.
const FLOATING: isize = 3;

/// How much the shade darkens the page, over the glass: a little.
const DIM: f64 = 0.14;

/// What a suggestion does when chosen.
#[derive(Clone)]
pub enum Action {
    /// Open an address, or search, in a new tab.
    Open(String),
    /// Bring an open tab to the front.
    Switch(Retained<NSWindow>),
}

#[derive(Clone)]
pub struct Suggestion {
    pub title: String,
    pub detail: String,
    pub action: Action,
}

/// What the application suggests for some typed text.
pub type Suggest = Rc<dyn Fn(&str) -> Vec<Suggestion>>;

thread_local! {
    /// The one bar, while it shows.
    static SHOWING: RefCell<Option<Retained<CommandBar>>> = const { RefCell::new(None) };
}

/// Whether this Mac draws Liquid Glass.
fn has_glass() -> bool {
    AnyClass::get(c"NSGlassEffectView").is_some()
}

/// A pane of glass over `frame`, with corners rounded by `radius`: macOS's
/// own where it has it, a translucent material otherwise. `within` says
/// whether what it shows through is in the same window (the shade) or
/// behind it (the bar).
fn glass(mtm: MainThreadMarker, frame: NSRect, radius: f64, within: bool) -> Retained<NSView> {
    if let Some(class) = AnyClass::get(c"NSGlassEffectView") {
        let allocated: *mut AnyObject = unsafe { msg_send![class, alloc] };
        let raw: *mut NSView = unsafe { msg_send![allocated, initWithFrame: frame] };
        let Some(view) = (unsafe { Retained::from_raw(raw) }) else {
            return NSView::new(mtm);
        };
        unsafe {
            let _: () = msg_send![&*view, setCornerRadius: radius];
        }
        return view;
    }
    let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    effect.setMaterial(if within {
        NSVisualEffectMaterial::Sheet
    } else {
        NSVisualEffectMaterial::Popover
    });
    effect.setBlendingMode(if within {
        NSVisualEffectBlendingMode::WithinWindow
    } else {
        NSVisualEffectBlendingMode::BehindWindow
    });
    effect.setState(NSVisualEffectState::Active);
    crate::chrome_macos::round_corners(&effect, radius);
    Retained::into_super(effect)
}

// -------------------------------------------------------------- the shade

define_class!(
    /// A slight darkening, drawn over the glass; a click on it dismisses.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoCommandBarDim"]
    pub struct Dim;

    impl Dim {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, DIM).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                self.bounds(),
                crate::chrome_macos::CARD_RADIUS,
                crate::chrome_macos::CARD_RADIUS,
            )
            .fill();
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

define_class!(
    /// The shade over the page's card while the bar shows: glass, then a
    /// slight darkening. The window's chrome keeps one, over the card and
    /// under the sidebar, hidden until the bar shows.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoPageShade"]
    pub struct PageShade;
);

impl PageShade {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let this: Retained<Self> = unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        let fill = objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable;
        let pane = glass(mtm, frame, crate::chrome_macos::CARD_RADIUS, true);
        pane.setAutoresizingMask(fill);
        this.addSubview(&pane);
        let dim: Retained<Dim> = unsafe { msg_send![Dim::alloc(mtm), initWithFrame: frame] };
        dim.setAutoresizingMask(fill);
        this.addSubview(&dim);
        this.setHidden(true);
        this
    }
}

// ---------------------------------------------------------- a suggestion

pub struct RowIvars {
    index: usize,
    selected: Cell<bool>,
    bar: Weak<CommandBar>,
}

define_class!(
    /// One suggestion: its title over its address, lit when chosen.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoSuggestionRow"]
    #[ivars = RowIvars]
    pub struct SuggestionRow;

    impl SuggestionRow {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if !self.ivars().selected.get() {
                return;
            }
            NSColor::labelColor().colorWithAlphaComponent(0.09).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 10.0, 10.0).fill();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(bar) = self.ivars().bar.load() {
                bar.accept(Some(self.ivars().index));
            }
        }
    }
);

impl SuggestionRow {
    fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        index: usize,
        suggestion: &Suggestion,
        selected: bool,
        bar: Weak<CommandBar>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RowIvars {
            index,
            selected: Cell::new(selected),
            bar,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let width = frame.size.width;
        let (symbol, label) = match suggestion.action {
            Action::Switch(_) => ("arrow.right.square", "Switch to tab"),
            Action::Open(_) => ("star", "Favorite"),
        };
        let icon = NSImageView::new(mtm);
        icon.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                Some(&NSString::from_str(label)),
            )
            .as_deref(),
        );
        icon.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        icon.setFrame(NSRect::new(
            NSPoint::new(INSET, (ROW - 18.0) / 2.0),
            NSSize::new(18.0, 18.0),
        ));
        this.addSubview(&icon);
        let text_x = INSET + SYMBOL + GAP;
        let text_w = (width - text_x - INSET).max(10.0);
        let title = NSTextField::labelWithString(&NSString::from_str(&suggestion.title), mtm);
        title.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        title.setFrame(NSRect::new(
            NSPoint::new(text_x, 6.0),
            NSSize::new(text_w, 18.0),
        ));
        this.addSubview(&title);
        let detail = NSTextField::labelWithString(&NSString::from_str(&suggestion.detail), mtm);
        detail.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        detail.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        detail.setFrame(NSRect::new(
            NSPoint::new(text_x, 25.0),
            NSSize::new(text_w, 14.0),
        ));
        this.addSubview(&detail);
        this
    }
}

// ---------------------------------------------------------------- the bar

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
    /// The glass the field and the suggestions sit on.
    card: Retained<NSView>,
    field: Retained<NSTextField>,
    /// The window the bar is over, whose page is shaded.
    over: Weak<NSWindow>,
    /// The application, which opens what was typed, and suggests.
    app: Weak<AnyObject>,
    suggest: Suggest,
    suggestions: RefCell<Vec<Suggestion>>,
    rows: RefCell<Vec<Retained<SuggestionRow>>>,
    selected: Cell<Option<usize>>,
    /// The bar's top edge on screen, which stays put as the bar grows.
    top: Cell<f64>,
    width: Cell<f64>,
    left: Cell<f64>,
}

define_class!(
    /// The bar: its window, its field, its suggestions, and what happens
    /// to what is typed or chosen.
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

    unsafe impl NSTextFieldDelegate for CommandBar {}

    unsafe impl NSControlTextEditingDelegate for CommandBar {
        /// Typing: the suggestions, for what is typed now.
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.refresh();
        }

        /// Up and Down move among the suggestions.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(
            &self,
            _control: &NSControl,
            _text_view: &AnyObject,
            command: Sel,
        ) -> objc2::runtime::Bool {
            let count = self.ivars().suggestions.borrow().len();
            if count == 0 {
                return objc2::runtime::Bool::NO;
            }
            let at = self.ivars().selected.get();
            let next = if command == sel!(moveDown:) {
                Some(at.map_or(0, |i| (i + 1).min(count - 1)))
            } else if command == sel!(moveUp:) {
                match at {
                    Some(0) | None => None,
                    Some(i) => Some(i - 1),
                }
            } else {
                return objc2::runtime::Bool::NO;
            };
            self.select(next);
            objc2::runtime::Bool::YES
        }
    }

    impl CommandBar {
        /// Return in the field: the chosen suggestion, or what was typed.
        #[unsafe(method(syndeoCommandBarGo:))]
        fn go(&self, _sender: Option<&AnyObject>) {
            self.accept(self.ivars().selected.get());
        }
    }
);

/// Show the bar over `over`, sending what is typed to `app`, with `suggest`
/// for the suggestions.
pub fn show(mtm: MainThreadMarker, over: &NSWindow, app: &AnyObject, suggest: Suggest) {
    let showing = SHOWING.with(|slot| slot.borrow().clone());
    if let Some(bar) = showing {
        bar.ivars().window.makeKeyAndOrderFront(None);
        return;
    }
    let bar = CommandBar::new(mtm, over, app, suggest);
    SHOWING.with(|slot| *slot.borrow_mut() = Some(bar.clone()));
    crate::browser_macos::set_shaded(over, true);
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
        if let Some(over) = bar.ivars().over.load() {
            crate::browser_macos::set_shaded(&over, false);
        }
    }
}

impl CommandBar {
    fn new(
        mtm: MainThreadMarker,
        over: &NSWindow,
        app: &AnyObject,
        suggest: Suggest,
    ) -> Retained<Self> {
        let frame = over.frame();
        let width = WIDTH.min(frame.size.width - 2.0 * MARGIN).max(240.0);
        let left = frame.origin.x + (frame.size.width - width) / 2.0;
        let top = frame.origin.y + (frame.size.height + HEIGHT) / 2.0 + ABOVE_MIDDLE;
        let rect = NSRect::new(NSPoint::new(left, top - HEIGHT), NSSize::new(width, HEIGHT));
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

        // The glass, with the symbol and the field across its top; the
        // suggestions go below them as the bar grows.
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), rect.size);
        let card = glass(mtm, bounds, RADIUS, false);
        card.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let content = NSView::initWithFrame(NSView::alloc(mtm), bounds);
        content.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        if has_glass() {
            unsafe {
                let _: () = msg_send![&*card, setContentView: &*content];
            }
        } else {
            card.addSubview(&content);
        }

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
            NSPoint::new(INSET, bounds.size.height - (HEIGHT + SYMBOL) / 2.0),
            NSSize::new(SYMBOL, SYMBOL),
        ));
        symbol.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin);
        content.addSubview(&symbol);

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
            NSPoint::new(field_x, bounds.size.height - (HEIGHT + field_height) / 2.0),
            NSSize::new(width - field_x - INSET, field_height),
        ));
        field.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable,
        );
        field.setAccessibilityIdentifier(Some(ns_string!("syndeo.command-bar")));
        content.addSubview(&field);
        window.setContentView(Some(&card));

        let this = Self::alloc(mtm).set_ivars(BarIvars {
            window: window.clone(),
            card: content.clone(),
            field: field.clone(),
            over: Weak::from_retained(&over.retain()),
            app: Weak::from_retained(&app.retain()),
            suggest,
            suggestions: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            selected: Cell::new(None),
            top: Cell::new(top),
            width: Cell::new(width),
            left: Cell::new(left),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let target: &AnyObject = (*this).as_ref();
        unsafe {
            field.setTarget(Some(target));
            field.setAction(Some(sel!(syndeoCommandBarGo:)));
            field.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        }
        window.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this
    }

    /// The suggestions for what is typed now, shown under the field, and
    /// the bar sized to them, its top edge staying put.
    fn refresh(&self) {
        let ivars = self.ivars();
        let typed = ivars.field.stringValue().to_string();
        let mut found = (ivars.suggest)(&typed);
        found.truncate(MOST);
        let count = found.len();
        *ivars.suggestions.borrow_mut() = found;
        ivars.selected.set(None);

        let height = if count == 0 {
            HEIGHT
        } else {
            HEIGHT + LIST_PAD + count as f64 * ROW + LIST_PAD
        };
        let rect = NSRect::new(
            NSPoint::new(ivars.left.get(), ivars.top.get() - height),
            NSSize::new(ivars.width.get(), height),
        );
        ivars.window.setFrame_display(rect, true);
        self.lay_out_rows();
    }

    /// The rows, made afresh for the suggestions as they are.
    fn lay_out_rows(&self) {
        let ivars = self.ivars();
        let mtm = self.mtm();
        for row in ivars.rows.borrow_mut().drain(..) {
            row.removeFromSuperview();
        }
        let suggestions = ivars.suggestions.borrow();
        let selected = ivars.selected.get();
        let width = ivars.width.get();
        let me = Weak::from_retained(&self.retain());
        let mut rows = Vec::with_capacity(suggestions.len());
        for (index, suggestion) in suggestions.iter().enumerate() {
            // From the top of the list down; the card is not flipped.
            let height = ivars.card.bounds().size.height;
            let y = height - HEIGHT - LIST_PAD - (index as f64 + 1.0) * ROW;
            let frame = NSRect::new(
                NSPoint::new(LIST_PAD, y),
                NSSize::new(width - 2.0 * LIST_PAD, ROW),
            );
            let row = SuggestionRow::new(
                mtm,
                frame,
                index,
                suggestion,
                selected == Some(index),
                me.clone(),
            );
            row.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin);
            ivars.card.addSubview(&row);
            rows.push(row);
        }
        *ivars.rows.borrow_mut() = rows;
    }

    fn select(&self, index: Option<usize>) {
        self.ivars().selected.set(index);
        for (i, row) in self.ivars().rows.borrow().iter().enumerate() {
            row.ivars().selected.set(Some(i) == index);
            row.setNeedsDisplay(true);
        }
    }

    /// Open the suggestion at `index`, or what was typed when there is
    /// none, then put the bar away.
    fn accept(&self, index: Option<usize>) {
        let ivars = self.ivars();
        let chosen = index.and_then(|i| ivars.suggestions.borrow().get(i).cloned());
        let typed = ivars.field.stringValue().to_string();
        let app = ivars.app.load();
        let field = ivars.field.clone();
        dismiss();
        match chosen.map(|s| s.action) {
            Some(Action::Switch(window)) => {
                window.makeKeyAndOrderFront(None);
                tracing::info!("a suggested tab brought to the front");
            }
            Some(Action::Open(address)) => {
                field.setStringValue(&NSString::from_str(&address));
                open_typed(self.mtm(), app.as_deref(), &field);
            }
            None => {
                if typed.trim().is_empty() {
                    return;
                }
                open_typed(self.mtm(), app.as_deref(), &field);
            }
        }
    }
}

/// Send what is in `field` to the application, to open in a new tab.
fn open_typed(mtm: MainThreadMarker, app: Option<&AnyObject>, field: &NSTextField) {
    let Some(app) = app else { return };
    let sender: &AnyObject = field.as_ref();
    let application = objc2_app_kit::NSApplication::sharedApplication(mtm);
    unsafe { application.sendAction_to_from(sel!(syndeoOpenTyped:), Some(app), Some(sender)) };
}

/// What was typed in the bar's field, when the application is sent
/// `syndeoOpenTyped:` with the field as the sender.
pub fn typed(sender: Option<&AnyObject>) -> Option<String> {
    let field = sender?.downcast_ref::<NSTextField>()?;
    let text = field.stringValue().to_string();
    (!text.trim().is_empty()).then_some(text)
}
