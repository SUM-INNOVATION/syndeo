//! A browser window's chrome: a sidebar down
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
//! system's appearance, light or dark, and its accent colour; the backdrop,
//! a gradient Core Animation draws, and the sidebar's rows are the two
//! things of Syndeo's own here. Nothing needs more than macOS 13.

use crate::address::Route;
use crate::sidebar_macos::{self, EdgeSensor, Sidebar};
use crate::{spaces, spaces_macos};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSBezierPath, NSButton, NSColor, NSControlSize,
    NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSImage, NSImageScaling,
    NSImageSymbolConfiguration, NSImageView, NSLayoutConstraint, NSLineBreakMode, NSTextField,
    NSTextFieldBezelStyle, NSTextFieldCell, NSTextFieldDelegate, NSView, NSWindow, NSWindowButton,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_core_graphics::CGColor;
use objc2_foundation::{ns_string, NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString, NSTimer};
use objc2_quartz_core::{CAGradientLayer, CALayer};
use objc2_web_kit::WKWebView;
use std::cell::{Cell, RefCell};

/// Inside the address field: the margin at either end, the size of Copy
/// Link at its trailing end, and the gap between the route and Copy Link,
/// and between the text and the route.
const INSET: f64 = 6.0;
const END_BUTTON: f64 = 22.0;
const GAP: f64 = 4.0;

/// The route's height, and the size of its symbol and its words.
const ROUTE_HEIGHT: f64 = 18.0;
const ROUTE_SYMBOL: f64 = 11.0;
const ROUTE_PAD: f64 = 6.0;

/// The page's card: its margin from the window's top, right and bottom,
/// and its corner radius.
const CARD_MARGIN: f64 = 8.0;
pub(crate) const CARD_RADIUS: f64 = 12.0;

/// The loading progress: its thickness, and how often it follows the page.
const PROGRESS: f64 = 2.0;
const PROGRESS_EVERY: f64 = 1.0 / 30.0;

// ---------------------------------------------------------- the backdrop

pub struct BackdropIvars {
    /// The space's hue, and whether the appearance was dark, that the
    /// colours were last set for.
    shown: Cell<Option<(usize, bool)>>,
}

define_class!(
    /// The window's backdrop, behind the page's card and under the title
    /// bar: a soft diagonal gradient, lavender into blush in light
    /// appearance and the same hues deepened in dark, in the current
    /// space's colours. Nothing in it is read by Accessibility: it is
    /// decoration.
    ///
    /// Its layer is a `CAGradientLayer`, which Core Animation draws: there
    /// is no bitmap of the window's size behind it, as there was when it
    /// drew itself (42 MB a window at 1440 by 900 points on a Retina
    /// display, kept while its tab was behind another). Its colours are set
    /// on the layer, when the space or the appearance changes, and where
    /// the gradient starts and ends when its size does.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoBackdrop"]
    #[ivars = BackdropIvars]
    pub struct Backdrop;

    impl Backdrop {
        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            Retained::into_super(CAGradientLayer::layer())
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            false
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            sidebar_macos::fill_gradient(self, self.bounds(), None);
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.paint();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.paint();
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {
            let _: () = unsafe { msg_send![super(self), viewDidChangeEffectiveAppearance] };
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }
    }
);

impl Backdrop {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(BackdropIvars {
            shown: Cell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // Layer-backed from the start, so that its layer is the gradient.
        this.setWantsLayer(true);
        this
    }

    /// Take the current space's colours, if it shows another's.
    pub fn follow_space(&self) {
        let hue = spaces_macos::read(|layout| layout.current_space().hue);
        if self.ivars().shown.get() != Some((hue, sidebar_macos::is_dark(self))) {
            self.paint();
        }
    }

    /// Set the gradient's colours, the current space's in this appearance,
    /// and where it starts and ends for the backdrop's size.
    fn paint(&self) {
        let layer: Option<Retained<CALayer>> = unsafe { msg_send![self, layer] };
        let Some(layer) = layer.and_then(|l| l.downcast::<CAGradientLayer>().ok()) else {
            return;
        };
        let dark = sidebar_macos::is_dark(self);
        let hue = spaces_macos::read(|layout| layout.current_space().hue);
        let (from, to) = spaces::hue_colors(hue, dark);
        let colour = |c: [f64; 3]| CGColor::new_srgb(c[0], c[1], c[2], 1.0);
        let (from, to) = (colour(from), colour(to));
        let colours: [&AnyObject; 2] = [(*from).as_ref(), (*to).as_ref()];
        let size = self.bounds().size;
        let ((sx, sy), (ex, ey)) =
            spaces::gradient_ends(size.width, size.height, spaces::GRADIENT_ANGLE);
        unsafe { layer.setColors(Some(&NSArray::from_slice(&colours))) };
        layer.setStartPoint(NSPoint::new(sx, sy));
        layer.setEndPoint(NSPoint::new(ex, ey));
        self.ivars().shown.set(Some((hue, dark)));
    }
}

/// Round `view`'s corners, clipping what is inside it to them, with the
/// continuous curve macOS draws its own rounded corners with.
pub(crate) fn round_corners(view: &NSView, radius: f64) {
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
            trailing: Cell::new(END_BUTTON + INSET),
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
    /// What holds the address field, its route, Copy Link and progress.
    field: Retained<NSView>,
    /// Copy Link, inside the address field.
    copy: Retained<NSButton>,
    /// The page's card: the web view, inset and with rounded corners.
    card: Retained<NSView>,
    /// Glass over the card while the command bar shows (`command_bar_macos`).
    shade: Retained<crate::command_bar_macos::PageShade>,
    /// The sidebar, with the tabs, Back and Forward and the field in it.
    pub sidebar: Retained<Sidebar>,
    /// The sidebar's width, which Hide Sidebar takes to nothing, and the
    /// card's distance from it.
    sidebar_width: RefCell<Option<Retained<NSLayoutConstraint>>>,
    card_leading: RefCell<Option<Retained<NSLayoutConstraint>>>,
    card_top: RefCell<Option<Retained<NSLayoutConstraint>>>,
    /// The sidebar is hidden: the page has the window, and the traffic
    /// lights are out of sight until the sidebar peeks.
    hidden: Cell<bool>,
    /// The window, whose traffic lights come and go with the sidebar.
    window: RefCell<Option<Weak<NSWindow>>>,
    /// Where the traffic lights sit by default, to move them into the panel
    /// while the sidebar floats and back again.
    lights: RefCell<Vec<(NSWindowButton, NSPoint)>>,
    /// The window's left edge, which brings a hidden sidebar back over the
    /// page while the pointer is there.
    edge: Retained<EdgeSensor>,
    /// The sidebar is hidden, and peeking over the page.
    peeking: Cell<bool>,
    /// Following the page's loading progress, while it loads.
    following: RefCell<Option<Retained<NSTimer>>>,
}

impl Ui {
    /// Say in the address field how the page on screen was reached, or
    /// nothing for one of Syndeo's own pages.
    pub fn show_route(&self, route: Option<Route>) {
        // Copy Link has something to copy only for a page with an address.
        self.copy.setEnabled(route.is_some());
        let width = self.route.show(route);
        let field = self.field.bounds().size;
        let reload_x = field.width - INSET - END_BUTTON;
        let mut trailing = END_BUTTON + INSET + GAP;
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
            .constraintEqualToAnchor_constant(&content.leadingAnchor(), sidebar_macos::WIDTH);
        let sidebar_top = sidebar
            .topAnchor()
            .constraintEqualToAnchor_constant(&content.topAnchor(), 0.0);
        // The traffic lights: the buttons across the sidebar's top line up
        // with their centre, and Show Sidebar sits just past them.
        let lights_center = traffic_lights(window).unwrap_or(22.0);
        self.sidebar.align_nav(lights_center);
        *self.window.borrow_mut() = Some(Weak::from_retained(&window.retain()));
        *self.lights.borrow_mut() = LIGHTS
            .iter()
            .filter_map(|which| {
                window
                    .standardWindowButton(*which)
                    .map(|button| (*which, button.frame().origin))
            })
            .collect();
        let edge: &NSView = &self.edge;
        let shade: &NSView = &self.shade;
        let top = card
            .topAnchor()
            .constraintEqualToAnchor_constant(&content.topAnchor(), CARD_MARGIN);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            sidebar_top.clone(),
            sidebar
                .bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
            sidebar
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            width.clone(),
            top.clone(),
            card.bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -CARD_MARGIN),
            leading.clone(),
            card.trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -CARD_MARGIN),
            edge.leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            edge.topAnchor()
                .constraintEqualToAnchor(&content.topAnchor()),
            edge.bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
            edge.widthAnchor()
                .constraintEqualToConstant(sidebar_macos::EDGE),
            shade.topAnchor().constraintEqualToAnchor(&card.topAnchor()),
            shade
                .bottomAnchor()
                .constraintEqualToAnchor(&card.bottomAnchor()),
            shade
                .leadingAnchor()
                .constraintEqualToAnchor(&card.leadingAnchor()),
            shade
                .trailingAnchor()
                .constraintEqualToAnchor(&card.trailingAnchor()),
        ]));
        *self.sidebar_width.borrow_mut() = Some(width);
        *self.card_leading.borrow_mut() = Some(leading);
        *self.card_top.borrow_mut() = Some(top);
    }

    /// Glass over the page, while the command bar shows; or the page as it
    /// is.
    pub fn set_shaded(&self, shaded: bool) {
        self.shade.setHidden(!shaded);
    }

    /// Where the page's card is, on screen: what the command bar centres on.
    pub fn page_rect_on_screen(&self, window: &NSWindow) -> NSRect {
        let in_window = self.card.convertRect_toView(self.card.bounds(), None);
        window.convertRectToScreen(in_window)
    }

    /// Hide the sidebar, giving the page the window, with the traffic lights
    /// out of sight; or show it again. Hidden, it comes back over the page
    /// while the pointer is at the window's left edge (`peek_sidebar`).
    pub fn toggle_sidebar(&self) {
        let (width, leading) = (self.sidebar_width.borrow(), self.card_leading.borrow());
        let (Some(width), Some(leading)) = (width.as_ref(), leading.as_ref()) else {
            return;
        };
        let hide = !self.hidden.get();
        if self.peeking.get() {
            self.unpeek_sidebar();
        }
        self.hidden.set(hide);
        width.setConstant(if hide { 0.0 } else { sidebar_macos::WIDTH });
        leading.setConstant(if hide {
            CARD_MARGIN
        } else {
            sidebar_macos::WIDTH
        });
        let sidebar: &NSView = &self.sidebar;
        sidebar.setHidden(hide);
        self.edge.setHidden(!hide);
        self.set_lights(!hide);
    }

    /// The pointer is at the window's left edge with the sidebar hidden:
    /// show it over the page, as a floating panel, traffic lights and all.
    pub fn peek_sidebar(&self) {
        if !self.hidden.get() || self.peeking.get() {
            return;
        }
        let width = self.sidebar_width.borrow();
        let Some(width) = width.as_ref() else { return };
        self.peeking.set(true);
        self.sidebar.set_floating(true);
        width.setConstant(sidebar_macos::WIDTH);
        let sidebar: &NSView = &self.sidebar;
        sidebar.setHidden(false);
        self.edge.setHidden(true);
        self.set_lights(true);
        self.offset_lights(sidebar_macos::FLOATING_INSET, sidebar_macos::FLOATING_TOP);
    }

    /// The pointer left a peeking sidebar: put it away again.
    pub fn unpeek_sidebar(&self) {
        if !self.peeking.get() {
            return;
        }
        let width = self.sidebar_width.borrow();
        let Some(width) = width.as_ref() else { return };
        self.peeking.set(false);
        self.sidebar.set_floating(false);
        width.setConstant(0.0);
        let sidebar: &NSView = &self.sidebar;
        sidebar.setHidden(true);
        self.edge.setHidden(false);
        self.offset_lights(0.0, 0.0);
        self.set_lights(false);
    }

    /// The window's traffic lights, shown or not: they belong to the
    /// sidebar's top, and go with it.
    fn set_lights(&self, shown: bool) {
        let Some(window) = self.window.borrow().as_ref().and_then(Weak::load) else {
            return;
        };
        for which in LIGHTS {
            if let Some(button) = window.standardWindowButton(which) {
                button.setHidden(!shown);
            }
        }
    }

    /// The traffic lights moved `right` and `down` from where macOS puts
    /// them: into the floating panel, out of its corner; nothing for back.
    fn offset_lights(&self, right: f64, down: f64) {
        let Some(window) = self.window.borrow().as_ref().and_then(Weak::load) else {
            return;
        };
        for (which, origin) in self.lights.borrow().iter() {
            if let Some(button) = window.standardWindowButton(*which) {
                button.setFrameOrigin(NSPoint::new(origin.x + right, origin.y - down));
            }
        }
    }
}

/// The traffic lights, in order.
const LIGHTS: [NSWindowButton; 3] = [
    NSWindowButton::CloseButton,
    NSWindowButton::MiniaturizeButton,
    NSWindowButton::ZoomButton,
];

/// Where `window`'s traffic lights are: the centre of the close button
/// below the window's top.
fn traffic_lights(window: &NSWindow) -> Option<f64> {
    let close = window.standardWindowButton(NSWindowButton::CloseButton)?;
    let rect = close.convertRect_toView(close.bounds(), None);
    let center = window.frame().size.height - (rect.origin.y + rect.size.height / 2.0);
    (center > 0.0).then_some(center)
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
    // The field is as tall as its bezel wants; its holder a little taller,
    // with the field centred in it.
    let fitting = address.fittingSize().height;
    let height = fitting.max(32.0);
    let width = sidebar_macos::WIDTH - 2.0 * sidebar_macos::SIDE;
    let field = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)),
    );
    address.setFrame(NSRect::new(
        NSPoint::new(0.0, ((height - fitting) / 2.0).floor()),
        NSSize::new(width, fitting),
    ));
    address.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

    let reload = nav_button(
        mtm,
        "arrow.clockwise",
        "Reload",
        target,
        sel!(syndeoReloadOrStop:),
    );

    // Copy Link, at the field's trailing end: the page's address, to the
    // clipboard.
    let copy_symbol = symbol_image("link", "Copy Link").unwrap_or_default();
    let copy = unsafe {
        NSButton::buttonWithImage_target_action(
            &copy_symbol,
            Some(target),
            Some(sel!(syndeoCopyAddress:)),
            mtm,
        )
    };
    copy.setBordered(false);
    copy.setSymbolConfiguration(Some(
        &NSImageSymbolConfiguration::configurationWithPointSize_weight(12.0, unsafe {
            NSFontWeightSemibold
        }),
    ));
    copy.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    copy.setToolTip(Some(ns_string!("Copy Link")));
    copy.setFrame(NSRect::new(
        NSPoint::new(width - INSET - END_BUTTON, (height - END_BUTTON) / 2.0),
        NSSize::new(END_BUTTON, END_BUTTON),
    ));
    copy.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

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
        (&*copy, "syndeo.copy"),
        (&*address, "syndeo.address"),
    ] {
        control.setAccessibilityIdentifier(Some(&NSString::from_str(name)));
    }
    for view in [&*address as &NSView, &*route, &*copy, &*progress] {
        field.addSubview(view);
    }
    // The sidebar sizes the field's width; its height is its own.
    field
        .heightAnchor()
        .constraintEqualToConstant(height)
        .setActive(true);

    let sidebar = Sidebar::new(mtm, &[&*hide, &*back, &*forward, &*reload], &field, target);
    let edge = EdgeSensor::new(mtm, target);

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
    // The shade over the card, and the sidebar above both, so that it can
    // peek over the page.
    let shade = crate::command_bar_macos::PageShade::new(mtm);
    container.addSubview(&card);
    container.addSubview(&shade);
    container.addSubview(&sidebar);
    container.addSubview(&edge);

    let ui = Ui {
        address,
        back,
        forward,
        reload,
        route,
        progress,
        cell,
        field,
        copy,
        card,
        shade,
        sidebar,
        sidebar_width: RefCell::new(None),
        card_leading: RefCell::new(None),
        card_top: RefCell::new(None),
        hidden: Cell::new(false),
        window: RefCell::new(None),
        lights: RefCell::new(Vec::new()),
        edge,
        peeking: Cell::new(false),
        following: RefCell::new(None),
    };
    ui.show_route(None);
    (Retained::into_super(container), ui)
}
