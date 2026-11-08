//! A browser window's chrome, as Arc arranged a browser's: a sidebar down
//! the left (`sidebar_macos`) with the tabs, Back and Forward and the
//! address field in it, and the page beside it as a card — inset, with
//! rounded corners — over a soft gradient backdrop (`Backdrop`) that the
//! title bar lets show through. There is no toolbar: the title bar holds
//! only the window's traffic lights, over the sidebar's top.
//!
//! - **Back and Forward**, and Hide Sidebar, are buttons across the
//!   sidebar's top, at its right, clear of the traffic lights.
//! - **The address field** is a pill below them, as wide as the sidebar.
//!   Inside it, at its trailing end, are the page's route and Reload, which
//!   is Stop while a page loads; along its bottom edge is the page's
//!   loading progress.
//! - **The route** (`address::Route`) is a symbol. Its words are its help
//!   tag, and are shown beside it too for the two routes Syndeo does not
//!   cache, Direct and Local, so that those are never left to a help tag.
//!   Accessibility reads the words, whichever is shown.
//!
//! Every control keeps the accessibility identifier `ci/test-app.sh` drives
//! it by: `syndeo.back`, `syndeo.forward`, `syndeo.address`, `syndeo.route`
//! and `syndeo.reload`; and the loading progress is `syndeo.progress`, there
//! while a page loads and gone once it has.
//!
//! AppKit's own controls and colours for the controls, which follow the
//! system's appearance, light or dark, and its accent colour; the backdrop
//! and the sidebar's rows are the two things drawn here. Nothing needs more
//! than macOS 13.

use crate::address::Route;
use crate::sidebar_macos::{self, Sidebar};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, AnyThread, DeclaredClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSBezierPath, NSButton, NSColor, NSControlSize,
    NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSGradient, NSImage, NSImageScaling,
    NSImageSymbolConfiguration, NSImageView, NSLayoutConstraint, NSLineBreakMode, NSTextField,
    NSTextFieldBezelStyle, NSTextFieldCell, NSTextFieldDelegate, NSView, NSWindow,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{ns_string, NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString, NSTimer};
use objc2_web_kit::WKWebView;
use std::cell::{Cell, RefCell};

/// Inside the address field: the margin at either end, Reload's size, and
/// the gap between the route and Reload, and between the text and the route.
const INSET: f64 = 6.0;
const RELOAD: f64 = 22.0;
const GAP: f64 = 4.0;

/// The route's height, and the size of its symbol and its words.
const ROUTE_HEIGHT: f64 = 18.0;
const ROUTE_SYMBOL: f64 = 11.0;
const ROUTE_PAD: f64 = 6.0;

/// The page's card: its margin from the window's top, right and bottom,
/// and its corner radius.
const CARD_MARGIN: f64 = 8.0;
const CARD_RADIUS: f64 = 12.0;

/// With the sidebar hidden: the strip kept along the top for Show Sidebar,
/// and where that button sits, clear of the traffic lights.
const SHOW_STRIP: f64 = 44.0;
const SHOW_X: f64 = 84.0;

/// The loading progress: its thickness, and how often it follows the page.
const PROGRESS: f64 = 2.0;
const PROGRESS_EVERY: f64 = 1.0 / 30.0;

// ---------------------------------------------------------- the backdrop

define_class!(
    /// The window's backdrop, behind the page's card and under the title
    /// bar: a soft diagonal gradient, lavender into blush in light
    /// appearance and the same hues deepened in dark. Nothing in it is
    /// read by Accessibility: it is decoration.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoBackdrop"]
    pub struct Backdrop;

    impl Backdrop {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let dark = sidebar_macos::is_dark(self);
            let (from, to) = if dark {
                (rgb(0.165, 0.145, 0.235), rgb(0.235, 0.165, 0.215))
            } else {
                (rgb(0.851, 0.831, 0.969), rgb(0.969, 0.851, 0.890))
            };
            if let Some(gradient) =
                NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &from, &to)
            {
                gradient.drawInRect_angle(self.bounds(), -35.0);
            }
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }
    }
);

impl Backdrop {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

/// An sRGB colour, opaque.
fn rgb(r: f64, g: f64, b: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

/// Round `view`'s corners, clipping what is inside it to them, with the
/// continuous curve macOS draws its own rounded corners with.
fn round_corners(view: &NSView, radius: f64) {
    view.setWantsLayer(true);
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![layer, setCornerRadius: radius];
        let _: () = msg_send![layer, setMasksToBounds: true];
        let _: () = msg_send![layer, setCornerCurve: &*ns_string!("continuous")];
    }
}

// ------------------------------------------------------------- the route

pub struct RouteIvars {
    /// What Accessibility reads: the route's words, or nothing.
    words: RefCell<String>,
    /// Its help tag, which Accessibility reads as its help.
    help: RefCell<String>,
    /// Whether the words are shown beside the symbol, on a tinted capsule.
    capsule: Cell<bool>,
    symbol: RefCell<Option<Retained<NSImageView>>>,
    label: RefCell<Option<Retained<NSTextField>>>,
}

define_class!(
    /// The route, inside the address field: a symbol, with its words beside
    /// it for Direct and Local. To Accessibility it is one piece of static
    /// text, the route's words, whatever is drawn.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoRouteBadge"]
    #[ivars = RouteIvars]
    pub struct RouteBadge;

    impl RouteBadge {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if !self.ivars().capsule.get() {
                return;
            }
            let bounds = self.bounds();
            let radius = bounds.size.height / 2.0;
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, radius, radius);
            NSColor::systemOrangeColor().colorWithAlphaComponent(0.16).setFill();
            path.fill();
        }

        /// Only while it shows something: hidden, it is not there at all.
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            !self.isHidden()
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str("AXStaticText"))
        }

        #[unsafe(method_id(accessibilityValue))]
        fn accessibility_value(&self) -> Option<Retained<AnyObject>> {
            Some(Retained::into_super(Retained::into_super(NSString::from_str(&self.ivars().words.borrow()))))
        }

        #[unsafe(method_id(accessibilityHelp))]
        fn accessibility_help(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().help.borrow()))
        }

        /// The symbol and the words are drawn, not read: the badge is read.
        #[unsafe(method_id(accessibilityChildren))]
        fn accessibility_children(&self) -> Option<Retained<NSArray<AnyObject>>> {
            Some(NSArray::new())
        }
    }
);

impl RouteBadge {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RouteIvars {
            words: RefCell::new(String::new()),
            help: RefCell::new(String::new()),
            capsule: Cell::new(false),
            symbol: RefCell::new(None),
            label: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(ROUTE_HEIGHT, ROUTE_HEIGHT))]
        };
        let symbol = NSImageView::new(mtm);
        symbol.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        let config =
            NSImageSymbolConfiguration::configurationWithPointSize_weight(ROUTE_SYMBOL, unsafe {
                NSFontWeightSemibold
            });
        symbol.setSymbolConfiguration(Some(&config));
        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, unsafe {
            NSFontWeightMedium
        })));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label.setLineBreakMode(NSLineBreakMode::ByClipping);
        this.addSubview(&symbol);
        this.addSubview(&label);
        *this.ivars().symbol.borrow_mut() = Some(symbol);
        *this.ivars().label.borrow_mut() = Some(label);
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.route")));
        this.setHidden(true);
        this
    }

    /// Show `route`, or nothing; returns the width it now takes.
    fn show(&self, route: Option<Route>) -> f64 {
        let Some(route) = route else {
            self.ivars().words.borrow_mut().clear();
            self.ivars().help.borrow_mut().clear();
            self.setToolTip(None);
            self.setHidden(true);
            return 0.0;
        };
        *self.ivars().words.borrow_mut() = route.label().to_string();
        *self.ivars().help.borrow_mut() = route.help();
        self.setToolTip(Some(&NSString::from_str(&route.help())));
        let capsule = route.words_shown();
        self.ivars().capsule.set(capsule);
        let (pad, gap) = if capsule {
            (ROUTE_PAD, GAP)
        } else {
            (0.0, 0.0)
        };
        let mut width = pad;
        if let Some(symbol) = &*self.ivars().symbol.borrow() {
            let (name, fallback) = route.symbol();
            let image =
                symbol_image(name, route.label()).or_else(|| symbol_image(fallback, route.label()));
            symbol.setImage(image.as_deref());
            let tint = if capsule {
                NSColor::systemOrangeColor()
            } else {
                NSColor::controlAccentColor()
            };
            symbol.setContentTintColor(Some(&tint));
            symbol.setFrame(NSRect::new(
                NSPoint::new(width, 0.0),
                NSSize::new(ROUTE_HEIGHT, ROUTE_HEIGHT),
            ));
            width += ROUTE_HEIGHT;
        }
        if let Some(label) = &*self.ivars().label.borrow() {
            label.setStringValue(&NSString::from_str(if capsule {
                route.label()
            } else {
                ""
            }));
            label.setHidden(!capsule);
            if capsule {
                let size = label.fittingSize();
                label.setFrame(NSRect::new(
                    NSPoint::new(width + gap, (ROUTE_HEIGHT - size.height) / 2.0),
                    size,
                ));
                width += gap + size.width;
            }
        }
        width += pad;
        self.setFrameSize(NSSize::new(width, ROUTE_HEIGHT));
        self.setHidden(false);
        self.setNeedsDisplay(true);
        width
    }
}

// ------------------------------------------------------------- the progress

pub struct ProgressIvars {
    /// How much of the page has loaded, from 0 to 1.
    done: Cell<f64>,
}

define_class!(
    /// The page's loading progress: a line along the bottom of the address
    /// field, in the accent colour. To Accessibility, a progress indicator
    /// whose value is how much has loaded.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoProgressLine"]
    #[ivars = ProgressIvars]
    pub struct ProgressLine;

    impl ProgressLine {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let width = bounds.size.width * self.ivars().done.get().clamp(0.0, 1.0);
            if width <= 0.0 {
                return;
            }
            let line = NSRect::new(bounds.origin, NSSize::new(width, bounds.size.height));
            let radius = bounds.size.height / 2.0;
            NSColor::controlAccentColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(line, radius, radius).fill();
        }

        /// Only while it shows something: hidden, it is not there at all.
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            !self.isHidden()
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str("AXProgressIndicator"))
        }

        #[unsafe(method_id(accessibilityValue))]
        fn accessibility_value(&self) -> Option<Retained<AnyObject>> {
            Some(Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_f64(self.ivars().done.get())))))
        }
    }
);

impl ProgressLine {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ProgressIvars {
            done: Cell::new(0.0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.progress")));
        this.setHidden(true);
        this
    }

    fn set(&self, done: f64) {
        if (self.ivars().done.get() - done).abs() > f64::EPSILON {
            self.ivars().done.set(done);
            self.setNeedsDisplay(true);
        }
    }
}

// ------------------------------------------------------------- the field

pub struct CellIvars {
    /// Room kept clear at the field's trailing end, for the route and Reload.
    trailing: Cell<f64>,
}

define_class!(
    /// The address field's cell: a rounded field whose text, and the text
    /// being typed, keep clear of the route and Reload at its trailing end.
    #[unsafe(super(NSTextFieldCell, objc2_app_kit::NSActionCell, objc2_app_kit::NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoAddressCell"]
    #[ivars = CellIvars]
    pub struct AddressCell;

    impl AddressCell {
        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            let rect: NSRect = unsafe { msg_send![super(self), drawingRectForBounds: bounds] };
            // The text ends where the route, or Reload, begins.
            let end = (bounds.origin.x + bounds.size.width - self.ivars().trailing.get())
                .min(rect.origin.x + rect.size.width);
            NSRect::new(
                rect.origin,
                NSSize::new((end - rect.origin.x).max(0.0), rect.size.height),
            )
        }
    }
);

impl AddressCell {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CellIvars {
            trailing: Cell::new(RELOAD + INSET),
        });
        unsafe { msg_send![super(this), initTextCell: ns_string!("")] }
    }
}

// ------------------------------------------------------------- the toolbar

// ------------------------------------------------------------- the window

/// The controls of a browser window.
pub struct Ui {
    pub address: Retained<NSTextField>,
    pub back: Retained<NSButton>,
    pub forward: Retained<NSButton>,
    reload: Retained<NSButton>,
    /// How the page on screen was reached: see [`Route`].
    route: Retained<RouteBadge>,
    progress: Retained<ProgressLine>,
    cell: Retained<AddressCell>,
    /// What holds the address field, its route, Reload and progress.
    field: Retained<NSView>,
    /// The page's card: the web view, inset and with rounded corners.
    card: Retained<NSView>,
    /// The sidebar, with the tabs, Back and Forward and the field in it.
    pub sidebar: Retained<Sidebar>,
    /// The sidebar's width, which Hide Sidebar takes to nothing, and the
    /// card's distance from it.
    sidebar_width: RefCell<Option<Retained<NSLayoutConstraint>>>,
    card_leading: RefCell<Option<Retained<NSLayoutConstraint>>>,
    card_top: RefCell<Option<Retained<NSLayoutConstraint>>>,
    /// Show Sidebar: beside the traffic lights while the sidebar is hidden,
    /// and gone while it shows.
    show: Retained<NSButton>,
    /// Following the page's loading progress, while it loads.
    following: RefCell<Option<Retained<NSTimer>>>,
}

impl Ui {
    /// Say in the address field how the page on screen was reached, or
    /// nothing for one of Syndeo's own pages.
    pub fn show_route(&self, route: Option<Route>) {
        let width = self.route.show(route);
        let field = self.field.bounds().size;
        let reload_x = field.width - INSET - RELOAD;
        let mut trailing = RELOAD + INSET + GAP;
        if width > 0.0 {
            self.route.setFrameOrigin(NSPoint::new(
                reload_x - GAP - width,
                (field.height - ROUTE_HEIGHT) / 2.0,
            ));
            trailing += width + GAP;
        }
        self.cell.ivars().trailing.set(trailing);
        let address: &NSView = &self.address;
        address.setNeedsDisplay(true);
    }

    /// Reload or Stop, and the loading progress, as `web` is loading or not.
    pub fn show_loading(&self, loading: bool, web: &WKWebView) {
        let (symbol, label) = if loading {
            ("xmark", "Stop")
        } else {
            ("arrow.clockwise", "Reload")
        };
        if let Some(image) = symbol_image(symbol, label) {
            self.reload.setImage(Some(&image));
        }
        self.reload.setToolTip(Some(&NSString::from_str(label)));
        if loading {
            self.progress.set(unsafe { web.estimatedProgress() });
            self.progress.setHidden(false);
            self.follow(web);
        } else {
            if let Some(timer) = self.following.borrow_mut().take() {
                timer.invalidate();
            }
            self.progress.setHidden(true);
            self.progress.set(0.0);
        }
    }

    /// While `web` loads, the progress line follows how much has loaded.
    fn follow(&self, web: &WKWebView) {
        if self.following.borrow().is_some() {
            return;
        }
        let web = Weak::from_retained(&objc2::Message::retain(web));
        let line = Weak::from_retained(&self.progress);
        let tick = block2::RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| {
            match (web.load(), line.load()) {
                (Some(web), Some(line)) => line.set(unsafe { web.estimatedProgress() }),
                // The window is gone: so is anything to follow.
                _ => unsafe { timer.as_ref() }.invalidate(),
            }
        });
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(PROGRESS_EVERY, true, &tick)
        };
        *self.following.borrow_mut() = Some(timer);
    }

    /// Give `window` a title bar that is see-through to the backdrop and
    /// holds only the traffic lights, over a full-size content view, and
    /// lay out the sidebar down its left and the page's card beside it,
    /// inset from the window's other edges. `web` fills the card, and the
    /// sidebar and the card have to be in the window's content view already.
    pub fn dress(&self, window: &NSWindow, _web: &WKWebView) {
        window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setTitlebarAppearsTransparent(true);
        window.setContentMinSize(NSSize::new(sidebar_macos::WIDTH + 360.0, 320.0));
        let Some(content) = window.contentView() else {
            return;
        };
        let sidebar: &NSView = &self.sidebar;
        let card = &self.card;
        card.setTranslatesAutoresizingMaskIntoConstraints(false);
        let width = sidebar
            .widthAnchor()
            .constraintEqualToConstant(sidebar_macos::WIDTH);
        let leading = card
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&sidebar.trailingAnchor(), 0.0);
        let top = card
            .topAnchor()
            .constraintEqualToAnchor_constant(&content.topAnchor(), CARD_MARGIN);
        let show: &NSView = &self.show;
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            sidebar
                .topAnchor()
                .constraintEqualToAnchor(&content.topAnchor()),
            sidebar
                .bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
            sidebar
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            width.clone(),
            top.clone(),
            show.leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), SHOW_X),
            show.topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), 8.0),
            card.bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -CARD_MARGIN),
            leading.clone(),
            card.trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -CARD_MARGIN),
        ]));
        *self.sidebar_width.borrow_mut() = Some(width);
        *self.card_leading.borrow_mut() = Some(leading);
        *self.card_top.borrow_mut() = Some(top);
    }

    /// Hide the sidebar, giving the page its width but for a strip along
    /// the top with Show Sidebar in it, beside the traffic lights; or show
    /// it again.
    pub fn toggle_sidebar(&self) {
        let (width, leading, top) = (
            self.sidebar_width.borrow(),
            self.card_leading.borrow(),
            self.card_top.borrow(),
        );
        let (Some(width), Some(leading), Some(top)) =
            (width.as_ref(), leading.as_ref(), top.as_ref())
        else {
            return;
        };
        let hide = width.constant() > 0.0;
        width.setConstant(if hide { 0.0 } else { sidebar_macos::WIDTH });
        leading.setConstant(if hide { CARD_MARGIN } else { 0.0 });
        top.setConstant(if hide { SHOW_STRIP } else { CARD_MARGIN });
        let sidebar: &NSView = &self.sidebar;
        sidebar.setHidden(hide);
        self.show.setHidden(!hide);
    }
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
pub fn symbol_image(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

/// A button across the sidebar's top: a symbol, described by `label`,
/// sending `action` to `target`, with no border of its own.
fn nav_button(
    mtm: MainThreadMarker,
    symbol: &str,
    label: &str,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSButton> {
    let image = symbol_image(symbol, label).unwrap_or_default();
    let button =
        unsafe { NSButton::buttonWithImage_target_action(&image, Some(target), Some(action), mtm) };
    button.setBordered(false);
    button.setSymbolConfiguration(Some(
        &NSImageSymbolConfiguration::configurationWithPointSize_weight(14.0, unsafe {
            NSFontWeightMedium
        }),
    ));
    button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    button.setToolTip(Some(&NSString::from_str(label)));
    button.setTranslatesAutoresizingMaskIntoConstraints(false);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        button.widthAnchor().constraintEqualToConstant(28.0),
        button.heightAnchor().constraintEqualToConstant(28.0),
    ]));
    button
}

/// The window's content and its controls: the backdrop, filling `frame`,
/// with the sidebar and the page's card on it, laid out by [`Ui::dress`];
/// `webview` fills the card, and the sidebar's controls send their actions
/// to `target`, the window's controller, with `delegate` told of typing in
/// the address field.
pub fn browser_content(
    mtm: MainThreadMarker,
    frame: NSRect,
    webview: &WKWebView,
    target: &AnyObject,
    delegate: &ProtocolObject<dyn NSTextFieldDelegate>,
) -> (Retained<NSView>, Ui) {
    let hide = nav_button(
        mtm,
        "sidebar.left",
        "Hide Sidebar",
        target,
        sel!(syndeoToggleSidebar:),
    );
    let show = nav_button(
        mtm,
        "sidebar.left",
        "Show Sidebar",
        target,
        sel!(syndeoToggleSidebar:),
    );
    show.setHidden(true);
    let back = nav_button(mtm, "chevron.left", "Back", target, sel!(syndeoBack:));
    let forward = nav_button(
        mtm,
        "chevron.right",
        "Forward",
        target,
        sel!(syndeoForward:),
    );

    // The address field: its cell keeps the text clear of what is inside it.
    let cell = AddressCell::new(mtm);
    cell.setEditable(true);
    cell.setSelectable(true);
    cell.setBezeled(true);
    cell.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
    cell.setScrollable(true);
    cell.setWraps(false);
    cell.setUsesSingleLineMode(true);
    cell.setLineBreakMode(NSLineBreakMode::ByClipping);
    // Only Return opens what was typed; leaving the field does not.
    cell.setSendsActionOnEndEditing(false);
    let address = NSTextField::new(mtm);
    address.setCell(Some(&cell));
    address.setControlSize(NSControlSize::Large);
    address.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    address.setPlaceholderString(Some(ns_string!("Enter a web address")));
    unsafe {
        // Told when someone types there, and when they leave.
        address.setDelegate(Some(delegate));
        address.setTarget(Some(target));
        address.setAction(Some(sel!(navigateTo:)));
    }
    let height = address.fittingSize().height.max(30.0);
    let width = sidebar_macos::WIDTH - 20.0;
    let field = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)),
    );
    address.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(width, height),
    ));
    address.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

    let reload_symbol = symbol_image("arrow.clockwise", "Reload").unwrap_or_default();
    let reload = unsafe {
        NSButton::buttonWithImage_target_action(
            &reload_symbol,
            Some(target),
            Some(sel!(syndeoReloadOrStop:)),
            mtm,
        )
    };
    reload.setBordered(false);
    reload.setSymbolConfiguration(Some(
        &NSImageSymbolConfiguration::configurationWithPointSize_weight(12.0, unsafe {
            NSFontWeightSemibold
        }),
    ));
    reload.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    reload.setToolTip(Some(ns_string!("Reload")));
    reload.setFrame(NSRect::new(
        NSPoint::new(width - INSET - RELOAD, (height - RELOAD) / 2.0),
        NSSize::new(RELOAD, RELOAD),
    ));
    reload.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

    let route = RouteBadge::new(mtm);
    route.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

    // Along the bottom of the field, clear of its rounded ends.
    let round = height / 2.0;
    let progress = ProgressLine::new(
        mtm,
        NSRect::new(
            NSPoint::new(round, 1.0),
            NSSize::new(width - 2.0 * round, PROGRESS),
        ),
    );
    progress.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

    // What Accessibility, and the UI test, find them by.
    for (control, name) in [
        (&*back as &NSView, "syndeo.back"),
        (&*forward, "syndeo.forward"),
        (&*reload, "syndeo.reload"),
        (&*address, "syndeo.address"),
    ] {
        control.setAccessibilityIdentifier(Some(&NSString::from_str(name)));
    }
    for view in [&*address as &NSView, &*route, &*reload, &*progress] {
        field.addSubview(view);
    }
    // The sidebar sizes the field's width; its height is its own.
    field
        .heightAnchor()
        .constraintEqualToConstant(height)
        .setActive(true);

    let sidebar = Sidebar::new(mtm, &[&*hide, &*back, &*forward], &field);

    // The backdrop fills the window; the sidebar and the page's card sit on
    // it, and the web view fills the card.
    let container = Backdrop::new(mtm, frame);
    let card = NSView::initWithFrame(NSView::alloc(mtm), frame);
    round_corners(&card, CARD_RADIUS);
    webview.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    webview.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    card.addSubview(webview);
    container.addSubview(&sidebar);
    container.addSubview(&card);
    container.addSubview(&show);

    let ui = Ui {
        address,
        back,
        forward,
        reload,
        route,
        progress,
        cell,
        field,
        card,
        sidebar,
        sidebar_width: RefCell::new(None),
        card_leading: RefCell::new(None),
        card_top: RefCell::new(None),
        show,
        following: RefCell::new(None),
    };
    ui.show_route(None);
    (Retained::into_super(container), ui)
}
