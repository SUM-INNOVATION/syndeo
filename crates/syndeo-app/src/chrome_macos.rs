//! A browser window's chrome: one unified toolbar in the title bar, over a
//! full-size content view, as current macOS browsers have it.
//!
//! - **Back and Forward** lead the toolbar, as toolbar buttons.
//! - **The address field** is its centre item: a rounded field that grows
//!   with the window. Inside it, at its trailing end, are the page's route
//!   and Reload, which is Stop while a page loads; along its bottom edge is
//!   the page's loading progress.
//! - **The route** (`address::Route`) is a symbol. Its words are its help
//!   tag, and are shown beside it too for the two routes Syndeo does not
//!   cache, Direct and Local, so that those are never left to a help tag.
//!   Accessibility reads the words, whichever is shown.
//!
//! The page sits below the toolbar and any tab bar, never under them. Every
//! control keeps the accessibility identifier `ci/test-app.sh` drives it by:
//! `syndeo.back`, `syndeo.forward`, `syndeo.address`, `syndeo.route` and
//! `syndeo.reload`; and the loading progress is `syndeo.progress`, there
//! while a page loads and gone once it has.
//!
//! Only AppKit's own controls and colours, which follow the system's
//! appearance, light or dark, and its accent colour; nothing here needs more
//! than macOS 13.

use crate::address::Route;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSBezelStyle, NSBezierPath, NSButton, NSColor,
    NSControlSize, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSImage, NSImageScaling,
    NSImageSymbolConfiguration, NSImageView, NSLayoutConstraint, NSLayoutGuide, NSLineBreakMode,
    NSTextField, NSTextFieldBezelStyle, NSTextFieldCell, NSTextFieldDelegate, NSToolbar,
    NSToolbarDelegate, NSToolbarDisplayMode, NSToolbarItem, NSToolbarItemIdentifier, NSView,
    NSWindow, NSWindowStyleMask, NSWindowTitleVisibility, NSWindowToolbarStyle,
};
use objc2_foundation::{
    ns_string, NSArray, NSNumber, NSPoint, NSRect, NSSet, NSSize, NSString, NSTimer,
};
use objc2_web_kit::WKWebView;
use std::cell::{Cell, RefCell};

/// Every browser window's toolbar: the same items, in the same places.
const TOOLBAR: &str = "com.sum.syndeo.browser";
const BACK_ITEM: &str = "syndeo.toolbar.back";
const FORWARD_ITEM: &str = "syndeo.toolbar.forward";
const ADDRESS_ITEM: &str = "syndeo.toolbar.address";

/// The address field's width in the toolbar: never narrower, and never
/// wider, however wide the window.
const ADDRESS_MIN: f64 = 240.0;
const ADDRESS_MAX: f64 = 820.0;

/// Inside the address field: the margin at either end, Reload's size, and
/// the gap between the route and Reload, and between the text and the route.
const INSET: f64 = 6.0;
const RELOAD: f64 = 22.0;
const GAP: f64 = 4.0;

/// The route's height, and the size of its symbol and its words.
const ROUTE_HEIGHT: f64 = 18.0;
const ROUTE_SYMBOL: f64 = 11.0;
const ROUTE_PAD: f64 = 6.0;

/// The loading progress: its thickness, and how often it follows the page.
const PROGRESS: f64 = 2.0;
const PROGRESS_EVERY: f64 = 1.0 / 30.0;

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

pub struct ItemsIvars {
    back: Retained<NSButton>,
    forward: Retained<NSButton>,
    address: Retained<NSView>,
}

define_class!(
    /// The toolbar's delegate: Back, Forward and the address field.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoToolbarItems"]
    #[ivars = ItemsIvars]
    pub struct ToolbarItems;

    unsafe impl NSObjectProtocol for ToolbarItems {}

    unsafe impl NSToolbarDelegate for ToolbarItems {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSToolbarItemIdentifier,
            _inserted: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            let ivars = self.ivars();
            let found: Option<(&NSView, &str, bool)> = match identifier.to_string().as_str() {
                BACK_ITEM => Some((&ivars.back, "Back", true)),
                FORWARD_ITEM => Some((&ivars.forward, "Forward", true)),
                ADDRESS_ITEM => Some((&ivars.address, "Address", false)),
                _ => None,
            };
            found.map(|(view, label, navigational)| {
                let item = NSToolbarItem::initWithItemIdentifier(
                    NSToolbarItem::alloc(self.mtm()),
                    identifier,
                );
                item.setLabel(&NSString::from_str(label));
                item.setPaletteLabel(&NSString::from_str(label));
                item.setNavigational(navigational);
                item.setView(Some(view));
                item
            })
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(
            &self,
            _toolbar: &NSToolbar,
        ) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            items()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(
            &self,
            _toolbar: &NSToolbar,
        ) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            items()
        }
    }
);

/// The toolbar's items, in order.
fn items() -> Retained<NSArray<NSToolbarItemIdentifier>> {
    NSArray::from_retained_slice(&[
        NSString::from_str(BACK_ITEM),
        NSString::from_str(FORWARD_ITEM),
        NSString::from_str(ADDRESS_ITEM),
    ])
}

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
    /// The toolbar's delegate, which the toolbar does not keep.
    items: Retained<ToolbarItems>,
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

    /// Give `window` its toolbar, unified with its title bar, over a
    /// full-size content view, and keep `web` below the toolbar and any tab
    /// bar. `web` has to be in the window's content view already.
    pub fn dress(&self, window: &NSWindow, web: &WKWebView) {
        let mtm = self.address.mtm();
        let toolbar =
            NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &NSString::from_str(TOOLBAR));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&*self.items)));
        toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
        toolbar.setAllowsUserCustomization(false);
        toolbar.setCenteredItemIdentifiers(&NSSet::from_retained_slice(&[NSString::from_str(
            ADDRESS_ITEM,
        )]));
        window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setToolbarStyle(NSWindowToolbarStyle::Unified);
        window.setToolbar(Some(&toolbar));

        // The page below the toolbar and the tab bar, whatever they take.
        let (Some(content), Some(guide)) = (
            window.contentView(),
            window
                .contentLayoutGuide()
                .and_then(|guide| guide.downcast::<NSLayoutGuide>().ok()),
        ) else {
            return;
        };
        web.setTranslatesAutoresizingMaskIntoConstraints(false);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            web.topAnchor().constraintEqualToAnchor(&guide.topAnchor()),
            web.bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
            web.leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            web.trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
        ]));
    }
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
pub fn symbol_image(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

/// A button in the toolbar: a symbol, described by `label`, sending `action`
/// to `target`.
fn toolbar_button(
    mtm: MainThreadMarker,
    symbol: &str,
    label: &str,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSButton> {
    let image = symbol_image(symbol, label).unwrap_or_default();
    let button =
        unsafe { NSButton::buttonWithImage_target_action(&image, Some(target), Some(action), mtm) };
    button.setBezelStyle(NSBezelStyle::Toolbar);
    button.setToolTip(Some(&NSString::from_str(label)));
    button
}

/// The window's content and its controls: `webview`, filling `frame` until
/// [`Ui::dress`] puts it below the toolbar, and the toolbar's controls, which
/// send their actions to `target`, the window's controller, with `delegate`
/// told of typing in the address field.
pub fn browser_content(
    mtm: MainThreadMarker,
    frame: NSRect,
    webview: &WKWebView,
    target: &AnyObject,
    delegate: &ProtocolObject<dyn NSTextFieldDelegate>,
) -> (Retained<NSView>, Ui) {
    let back = toolbar_button(mtm, "chevron.left", "Back", target, sel!(syndeoBack:));
    let forward = toolbar_button(
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
    let height = address.fittingSize().height.max(28.0);
    let width = ADDRESS_MAX;
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
    // The toolbar sizes the field by these: as wide as there is room for,
    // between the two.
    field.setTranslatesAutoresizingMaskIntoConstraints(false);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        field
            .widthAnchor()
            .constraintGreaterThanOrEqualToConstant(ADDRESS_MIN),
        field
            .widthAnchor()
            .constraintLessThanOrEqualToConstant(ADDRESS_MAX),
        field.heightAnchor().constraintEqualToConstant(height),
    ]));

    let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
    webview.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    webview.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    container.addSubview(webview);

    let items = ToolbarItems::alloc(mtm).set_ivars(ItemsIvars {
        back: back.clone(),
        forward: forward.clone(),
        address: field.clone(),
    });
    let items: Retained<ToolbarItems> = unsafe { msg_send![super(items), init] };

    let ui = Ui {
        address,
        back,
        forward,
        reload,
        route,
        progress,
        cell,
        field,
        items,
        following: RefCell::new(None),
    };
    ui.show_route(None);
    (container, ui)
}
