//! The sidebar: a browser window's tabs down its left side, as Arc has them,
//! with Back, Forward and Reload across its top and the address field as a
//! pill below them.
//!
//! Every tab is still a window of its own, tabbed with the others by AppKit
//! (`tabs_macos`): the sidebar is a view of that tab group, drawn in each of
//! its windows alike, so that choosing a tab, which brings its window to
//! the front, changes nothing but which row is lit. AppKit's own tab bar is
//! kept hidden (`tabs_macos::hide_tab_bar`).
//!
//! - **Pinned tabs** come first, then a rule, then the rest, in the order
//!   their windows are in the group.
//! - **A row** is the page's icon, or a globe for a page without one, and
//!   its title; the row in front is lit, a row pointed at is lit less, and
//!   either shows Close at its end. A pinned tab shows a pin there instead,
//!   and is not closed from the sidebar.
//! - **New Tab** is the last row, always.
//! - **A secondary click** on a row opens the tab's own menu, the one the
//!   application gives (`menu_for`).
//!
//! Nothing here fetches anything: icons are the ones each tab's badge has.
//! To Accessibility each row is a button named for its tab, and the UI test
//! finds the rows by `syndeo.sidebar-tab`.

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    define_class, msg_send, sel, AnyThread, DeclaredClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSBezierPath, NSBox, NSBoxType, NSButton, NSClipView, NSColor, NSEvent, NSFont, NSImage,
    NSImageScaling, NSImageView, NSLayoutAttribute, NSLayoutConstraint, NSLineBreakMode, NSMenu,
    NSScrollView, NSStackView, NSTextField, NSTrackingArea, NSTrackingAreaOptions,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow,
};
use objc2_foundation::{ns_string, NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The sidebar's width, open.
pub const WIDTH: f64 = 248.0;

/// Inside the sidebar: the margin at either side, where the top row of
/// buttons sits below the window's top (clear of the traffic lights, which
/// take the first 80 points of its width), and the gap below the address
/// field before the tabs.
const SIDE: f64 = 10.0;
const NAV_TOP: f64 = 8.0;
const NAV_HEIGHT: f64 = 28.0;
const ADDRESS_TOP: f64 = NAV_TOP + NAV_HEIGHT + 10.0;
const LIST_GAP: f64 = 14.0;

/// A row: its height, its corner radius, the icon's size, and the gaps.
const ROW_HEIGHT: f64 = 32.0;
const ROW_RADIUS: f64 = 8.0;
const ROW_PAD: f64 = 9.0;
const ICON: f64 = 16.0;
const CLOSE: f64 = 20.0;
const ROW_GAP: f64 = 2.0;

/// The menu for a tab's window, from the application.
pub type MenuFor = Rc<dyn Fn(&NSWindow) -> Option<Retained<NSMenu>>>;

/// A tab, as the sidebar shows it.
pub struct TabInfo {
    pub window: Retained<NSWindow>,
    pub title: String,
    pub icon: Option<Retained<NSImage>>,
    pub pinned: bool,
    pub loading: bool,
    /// The tab in front of its window.
    pub active: bool,
}

/// Whether `view` is drawn in dark appearance.
pub fn is_dark(view: &NSView) -> bool {
    view.effectiveAppearance()
        .name()
        .isEqualToString(unsafe { NSAppearanceNameDarkAqua })
}

/// White, at `alpha`: what rows are lit with, over the backdrop.
fn lit(alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, alpha)
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
fn symbol(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

// ---------------------------------------------------------------- a row

pub struct RowIvars {
    /// The tab's window; none for the New Tab row.
    tab: RefCell<Option<Weak<NSWindow>>>,
    title: RefCell<String>,
    active: Cell<bool>,
    hover: Cell<bool>,
    pinned: Cell<bool>,
    close: RefCell<Option<Retained<NSButton>>>,
    menu_for: RefCell<Option<MenuFor>>,
}

define_class!(
    /// One tab in the sidebar, or the New Tab row.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoTabRow"]
    #[ivars = RowIvars]
    pub struct TabRow;

    impl TabRow {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let ivars = self.ivars();
            let dark = is_dark(self);
            let alpha = match (ivars.active.get(), ivars.hover.get()) {
                (true, _) if dark => 0.16,
                (true, _) => 0.62,
                (false, true) if dark => 0.07,
                (false, true) => 0.30,
                _ => return,
            };
            lit(alpha).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                self.bounds(),
                ROW_RADIUS,
                ROW_RADIUS,
            )
            .fill();
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
            self.choose();
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let ivars = self.ivars();
            let (Some(tab), Some(menu_for)) = (
                ivars.tab.borrow().as_ref().and_then(Weak::load),
                ivars.menu_for.borrow().clone(),
            ) else {
                return;
            };
            if let Some(menu) = menu_for(&tab) {
                NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.set_hover(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_hover(false);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            for area in self.trackingAreas().iter() {
                self.removeTrackingArea(&area);
            }
            let options = NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    options,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str("AXButton"))
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn accessibility_label(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().title.borrow()))
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            self.choose();
            true
        }

        /// Close, at the row's end.
        #[unsafe(method(syndeoCloseRow:))]
        fn close_row(&self, _sender: Option<&AnyObject>) {
            if let Some(tab) = self.ivars().tab.borrow().as_ref().and_then(Weak::load) {
                tab.performClose(None);
            }
        }
    }
);

impl TabRow {
    /// A row for `tab`, or for New Tab when there is none.
    fn new(
        mtm: MainThreadMarker,
        width: f64,
        tab: Option<&TabInfo>,
        menu_for: Option<MenuFor>,
    ) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, ROW_HEIGHT));
        let this = Self::alloc(mtm).set_ivars(RowIvars {
            tab: RefCell::new(tab.map(|t| Weak::from_retained(&t.window))),
            title: RefCell::new(tab.map_or("New Tab".to_string(), |t| t.title.clone())),
            active: Cell::new(tab.is_some_and(|t| t.active)),
            hover: Cell::new(false),
            pinned: Cell::new(tab.is_some_and(|t| t.pinned)),
            close: RefCell::new(None),
            menu_for: RefCell::new(menu_for),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.heightAnchor()
            .constraintEqualToConstant(ROW_HEIGHT)
            .setActive(true);
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.sidebar-tab")));

        // The icon: the page's, a globe without one, an hourglass while
        // the page loads, and a plus for New Tab.
        let image = match tab {
            None => symbol("plus", "New Tab"),
            Some(t) if t.loading => symbol("hourglass", "Loading"),
            Some(t) => t.icon.clone().or_else(|| symbol("globe", "Page")),
        };
        let icon = NSImageView::new(mtm);
        icon.setImage(image.as_deref());
        icon.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        icon.setFrame(NSRect::new(
            NSPoint::new(ROW_PAD, (ROW_HEIGHT - ICON) / 2.0),
            NSSize::new(ICON, ICON),
        ));
        this.addSubview(&icon);

        // The title, clipped at its end.
        let label =
            NSTextField::labelWithString(&NSString::from_str(&this.ivars().title.borrow()), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        let colour = if tab.is_none() {
            NSColor::secondaryLabelColor()
        } else {
            NSColor::labelColor()
        };
        label.setTextColor(Some(&colour));
        let text_x = ROW_PAD + ICON + 8.0;
        let text_w = width - text_x - CLOSE - ROW_PAD;
        label.setFrame(NSRect::new(
            NSPoint::new(text_x, (ROW_HEIGHT - 17.0) / 2.0),
            NSSize::new(text_w.max(10.0), 17.0),
        ));
        label.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
        this.addSubview(&label);

        // Close, or a pin, at the end.
        if let Some(t) = tab {
            let end = NSRect::new(
                NSPoint::new(width - ROW_PAD - CLOSE, (ROW_HEIGHT - CLOSE) / 2.0),
                NSSize::new(CLOSE, CLOSE),
            );
            if t.pinned {
                let pin = NSImageView::new(mtm);
                pin.setImage(symbol("pin.fill", "Pinned").as_deref());
                pin.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
                pin.setFrame(end);
                pin.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinXMargin);
                this.addSubview(&pin);
            } else {
                let image = symbol("xmark", "Close Tab").unwrap_or_default();
                let target: &AnyObject = (*this).as_ref();
                let close = unsafe {
                    NSButton::buttonWithImage_target_action(
                        &image,
                        Some(target),
                        Some(sel!(syndeoCloseRow:)),
                        mtm,
                    )
                };
                close.setBordered(false);
                close.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
                close.setToolTip(Some(ns_string!("Close Tab")));
                close.setFrame(end);
                close.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinXMargin);
                close.setHidden(!t.active);
                this.addSubview(&close);
                *this.ivars().close.borrow_mut() = Some(close);
            }
        }
        this
    }

    /// Bring the tab in front, or open a new one.
    fn choose(&self) {
        let tab = self.ivars().tab.borrow().as_ref().and_then(Weak::load);
        match tab {
            Some(window) => window.makeKeyAndOrderFront(None),
            None => {
                let app = NSApplication::sharedApplication(self.mtm());
                unsafe { app.sendAction_to_from(sel!(newWindowForTab:), None, None) };
            }
        }
    }

    fn set_hover(&self, hover: bool) {
        let ivars = self.ivars();
        ivars.hover.set(hover);
        if let Some(close) = &*ivars.close.borrow() {
            close.setHidden(!(hover || ivars.active.get()) || ivars.pinned.get());
        }
        self.setNeedsDisplay(true);
    }
}

// ------------------------------------------------------------ the sidebar

define_class!(
    /// A view that lays its content out from the top, for the scroll view's
    /// document: the tab rows stack from the top down, however few.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoTopDownView"]
    pub struct TopDown;

    impl TopDown {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

pub struct SidebarIvars {
    list: Retained<NSStackView>,
    menu_for: RefCell<Option<MenuFor>>,
}

define_class!(
    /// The sidebar itself: see the module's notes. It draws nothing of its
    /// own, so the window's backdrop shows through it.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoSidebar"]
    #[ivars = SidebarIvars]
    pub struct Sidebar;

    impl Sidebar {
        /// Dragging the sidebar's empty parts moves the window, as dragging
        /// a title bar does.
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            true
        }
    }
);

impl Sidebar {
    /// The sidebar, with `nav` (Back, Forward, Reload and the rest) across
    /// its top, right-aligned, and `field`, the address field, below them.
    pub fn new(mtm: MainThreadMarker, nav: &[&NSView], field: &NSView) -> Retained<Self> {
        let list = NSStackView::new(mtm);
        list.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        list.setAlignment(NSLayoutAttribute::Width);
        list.setSpacing(ROW_GAP);
        list.setEdgeInsets(NSEdgeInsets {
            top: 0.0,
            left: 0.0,
            bottom: 8.0,
            right: 0.0,
        });
        let this = Self::alloc(mtm).set_ivars(SidebarIvars {
            list: list.clone(),
            menu_for: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);

        // The buttons across the top, right-aligned, clear of the traffic
        // lights at the left.
        let buttons: Vec<&NSView> = nav.to_vec();
        let row = NSStackView::stackViewWithViews(&NSArray::from_slice(&buttons), mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(2.0);
        row.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.addSubview(&row);

        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.addSubview(field);

        // The tabs, scrolling when there are more than fit.
        let scroll = NSScrollView::new(mtm);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        let document: Retained<TopDown> = unsafe { msg_send![TopDown::alloc(mtm), init] };
        document.setTranslatesAutoresizingMaskIntoConstraints(false);
        list.setTranslatesAutoresizingMaskIntoConstraints(false);
        document.addSubview(&list);
        scroll.setDocumentView(Some(&document));
        let clip: Retained<NSClipView> = scroll.contentView();
        clip.setDrawsBackground(false);
        this.addSubview(&scroll);

        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            row.topAnchor()
                .constraintEqualToAnchor_constant(&this.topAnchor(), NAV_TOP),
            row.trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -SIDE),
            row.heightAnchor().constraintEqualToConstant(NAV_HEIGHT),
            field
                .topAnchor()
                .constraintEqualToAnchor_constant(&this.topAnchor(), ADDRESS_TOP),
            field
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&this.leadingAnchor(), SIDE),
            field
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -SIDE),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&field.bottomAnchor(), LIST_GAP),
            scroll
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&this.leadingAnchor(), SIDE - 2.0),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -(SIDE - 2.0)),
            scroll
                .bottomAnchor()
                .constraintEqualToAnchor(&this.bottomAnchor()),
            document
                .leadingAnchor()
                .constraintEqualToAnchor(&clip.leadingAnchor()),
            document
                .trailingAnchor()
                .constraintEqualToAnchor(&clip.trailingAnchor()),
            document
                .topAnchor()
                .constraintEqualToAnchor(&clip.topAnchor()),
            list.leadingAnchor()
                .constraintEqualToAnchor(&document.leadingAnchor()),
            list.trailingAnchor()
                .constraintEqualToAnchor(&document.trailingAnchor()),
            list.topAnchor()
                .constraintEqualToAnchor(&document.topAnchor()),
            list.bottomAnchor()
                .constraintEqualToAnchor(&document.bottomAnchor()),
        ]));
        this
    }

    /// Answer a secondary click on a row with the application's menu for
    /// its tab.
    pub fn set_menu(&self, menu_for: MenuFor) {
        *self.ivars().menu_for.borrow_mut() = Some(menu_for);
    }

    /// Show `tabs`, in their order: pinned, a rule, the rest, New Tab.
    pub fn reload(&self, tabs: &[TabInfo]) {
        let mtm = self.mtm();
        let list = &self.ivars().list;
        for view in list.arrangedSubviews().iter() {
            list.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        let menu_for = self.ivars().menu_for.borrow().clone();
        let width = WIDTH - 2.0 * (SIDE - 2.0);
        let pinned = tabs.iter().filter(|t| t.pinned).count();
        for (i, tab) in tabs.iter().enumerate() {
            if i == pinned && pinned > 0 {
                let rule = NSBox::new(mtm);
                rule.setBoxType(NSBoxType::Separator);
                rule.setTranslatesAutoresizingMaskIntoConstraints(false);
                list.addArrangedSubview(&rule);
            }
            let row = TabRow::new(mtm, width, Some(tab), menu_for.clone());
            list.addArrangedSubview(&row);
        }
        let new_tab = TabRow::new(mtm, width, None, None);
        list.addArrangedSubview(&new_tab);
    }
}
