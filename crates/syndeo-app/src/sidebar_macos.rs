//! The sidebar: a browser window's tabs down its left side, as Arc has them,
//! with Hide Sidebar, Back, Forward and Reload across its top, the address
//! field as a pill below them, the favorites as a grid of tiles under that,
//! and the tabs below.
//!
//! Every tab is still a window of its own, tabbed with the others by AppKit
//! (`tabs_macos`): the sidebar is a view of that tab group, drawn in each of
//! its windows alike, so that choosing a tab, which brings its window to
//! the front, changes nothing but which row is lit. AppKit's own tab bar is
//! kept hidden (`tabs_macos::hide_tab_bar`).
//!
//! - **Favorites** (`favorites`) are tiles, four to a row: the site's icon
//!   when a tab of that site is open and has one, its initial otherwise.
//!   Choosing one opens its page in this tab; a secondary click offers to
//!   take it out of the favorites.
//! - **Pinned tabs** come first among the rows, then a rule, then the rest,
//!   in the order their windows are in the group.
//! - **A row** is the page's icon, or a globe for a page without one, and
//!   its title; the row in front is lit, a row pointed at is lit less, and
//!   either shows Close at its end. A pinned tab shows a pin there instead,
//!   and is not closed from the sidebar.
//! - **New Tab** is the last row, always.
//! - **A secondary click** on a row opens the tab's own menu, the one the
//!   application gives (`menu_for`).
//! - **Hidden**, the sidebar comes back over the page while the pointer is
//!   at the window's left edge (`EdgeSensor`), as a floating panel, and
//!   goes again when the pointer leaves it: Arc's peek. The panel is drawn
//!   here; what shows and hides it is the window's chrome (`chrome_macos`),
//!   which the sensor and the sidebar tell through `syndeoPeekSidebar:` and
//!   `syndeoUnpeekSidebar:`, sent to the window's controller.
//!
//! Nothing here fetches anything: icons are the ones each tab's badge has.
//! To Accessibility each row is a button named for its tab, and the UI test
//! finds the rows by `syndeo.sidebar-tab` and the tiles by
//! `syndeo.favorite`.

use crate::favorites::{self, Favorite};
use crate::spaces::{Folder, Layout};
use crate::spaces_macos::{self, SpaceBar};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    define_class, msg_send, sel, AnyThread, DeclaredClass, MainThreadMarker, MainThreadOnly,
    Message,
};
use objc2_app_kit::{
    NSAccessibility, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSBezierPath, NSBox, NSBoxType, NSButton, NSClipView, NSColor, NSEvent, NSFont, NSGradient,
    NSImage, NSImageScaling, NSImageView, NSLayoutAttribute, NSLayoutConstraint, NSLineBreakMode,
    NSMenu, NSScrollView, NSStackView, NSTextAlignment, NSTextField, NSTrackingArea,
    NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
};
use objc2_foundation::{
    ns_string, NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString, NSUserDefaults,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The sidebar's width, open.
pub const WIDTH: f64 = 248.0;

/// Inside the sidebar: the margin at either side, where the top row of
/// buttons sits below the window's top (clear of the traffic lights, which
/// take the first 80 points of its width), and the gaps below the address
/// field and the favorites.
pub const SIDE: f64 = 10.0;
pub const NAV_TOP: f64 = 8.0;
pub const NAV_HEIGHT: f64 = 28.0;
const ADDRESS_TOP: f64 = NAV_TOP + NAV_HEIGHT + 10.0;
const LIST_GAP: f64 = 14.0;

/// The floating panel, when the sidebar peeks over the page: its margin
/// from the window's left and bottom, and its corner radius.
pub const PANEL_MARGIN: f64 = 8.0;
const PANEL_TOP: f64 = 6.0;
const PANEL_RADIUS: f64 = 12.0;

/// The edge of the window that brings a hidden sidebar back.
pub const EDGE: f64 = 6.0;

/// A row: its height, its corner radius, the icon's size, and the gaps.
const ROW_HEIGHT: f64 = 32.0;
const ROW_RADIUS: f64 = 8.0;
const ROW_PAD: f64 = 9.0;
const ICON: f64 = 16.0;
const CLOSE: f64 = 20.0;
const ROW_GAP: f64 = 2.0;

/// How far a press has to move to be a drag, not a click.
const DRAG_SLOP: f64 = 4.0;

/// A favorite's tile: four to a row, with this much between them.
const TILES_PER_ROW: usize = 4;
const TILE_GAP: f64 = 6.0;
const TILE_RADIUS: f64 = 10.0;
const TILE_ICON: f64 = 22.0;

/// The menu for a tab's window, from the application.
pub type MenuFor = Rc<dyn Fn(&NSWindow) -> Option<Retained<NSMenu>>>;

/// A tab, as the sidebar shows it.
pub struct TabInfo {
    pub window: Retained<NSWindow>,
    pub title: String,
    /// The page's address, as the address field shows it; empty for a page
    /// that has none, such as the start page.
    pub address: String,
    pub icon: Option<Retained<NSImage>>,
    pub pinned: bool,
    pub loading: bool,
    /// The tab in front of its window.
    pub active: bool,
    /// The space it is in, and the folder, if any.
    pub space: u64,
    pub folder: Option<u64>,
}

/// A favorite, as the sidebar shows it: with an icon when a tab of its
/// site has one.
pub struct FavoriteInfo {
    pub favorite: Favorite,
    pub icon: Option<Retained<NSImage>>,
}

/// Whether `view` is drawn in dark appearance.
pub fn is_dark(view: &NSView) -> bool {
    view.effectiveAppearance()
        .name()
        .isEqualToString(unsafe { NSAppearanceNameDarkAqua })
}

/// White, at `alpha`: what rows and tiles are lit with, over the backdrop.
fn lit(alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, alpha)
}

/// The backdrop's gradient, lavender into blush in light appearance and
/// the same hues deepened in dark, filling `rect` in `view`, with corners
/// rounded by `radius` if any.
pub fn fill_gradient(view: &NSView, rect: NSRect, radius: Option<f64>) {
    let (from, to) = spaces_macos::current_colors(is_dark(view));
    let Some(gradient) =
        NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &from, &to)
    else {
        return;
    };
    match radius {
        Some(radius) => {
            let path =
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius);
            gradient.drawInBezierPath_angle(&path, -35.0);
        }
        None => gradient.drawInRect_angle(rect, -35.0),
    }
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
fn symbol(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

/// Send `action` to `target`, from `sender`, as a control would.
fn send(
    mtm: MainThreadMarker,
    action: objc2::runtime::Sel,
    target: Option<&AnyObject>,
    sender: Option<&AnyObject>,
) {
    let app = NSApplication::sharedApplication(mtm);
    unsafe { app.sendAction_to_from(action, target, sender) };
}

/// A tracking area for `view`'s whole visible rect, told of the pointer
/// entering and leaving while the window is key.
fn track(view: &NSView) {
    for area in view.trackingAreas().iter() {
        view.removeTrackingArea(&area);
    }
    let options = NSTrackingAreaOptions::MouseEnteredAndExited
        | NSTrackingAreaOptions::ActiveInKeyWindow
        | NSTrackingAreaOptions::InVisibleRect;
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            view.bounds(),
            options,
            Some(view),
            None,
        )
    };
    view.addTrackingArea(&area);
}

// ------------------------------------------------------------ favorites kept

/// The defaults key the favorites are kept under, as `favorites::render`.
const FAVORITES_KEY: &str = "SyndeoFavorites";

/// The favorites, as kept.
/// The favorites of `space`: each space has its own. The first space's
/// are also read from where the favorites were kept before there were
/// spaces.
pub fn favorites_read(space: u64) -> Vec<Favorite> {
    let defaults = NSUserDefaults::standardUserDefaults();
    let kept = defaults
        .stringForKey(&NSString::from_str(&favorites_key(space)))
        .or_else(|| {
            (space == crate::spaces::FIRST_SPACE)
                .then(|| defaults.stringForKey(&NSString::from_str(FAVORITES_KEY)))
                .flatten()
        });
    kept.map(|json| favorites::parse(&json.to_string()))
        .unwrap_or_default()
}

fn favorites_key(space: u64) -> String {
    format!("{FAVORITES_KEY}.{space}")
}

/// Keep `favorites`.
pub fn favorites_write(space: u64, favorites: &[Favorite]) {
    let json = NSString::from_str(&favorites::render(favorites));
    let value: &AnyObject = json.as_ref();
    unsafe {
        NSUserDefaults::standardUserDefaults()
            .setObject_forKey(Some(value), &NSString::from_str(&favorites_key(space)))
    };
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
    /// The sidebar, told when the row is dragged: to the favorites, say.
    sidebar: RefCell<Option<Weak<Sidebar>>>,
    /// Where the mouse went down, in the window, while it is down; and
    /// whether it has moved far enough since to be a drag.
    press: Cell<Option<NSPoint>>,
    dragging: Cell<bool>,
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

        /// A press chooses the tab when it is let go, unless it became a
        /// drag: dragged up to the favorites, the tab's page is added there.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().press.set(Some(event.locationInWindow()));
            self.ivars().dragging.set(false);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let ivars = self.ivars();
            let (Some(press), Some(sidebar)) = (
                ivars.press.get(),
                ivars.sidebar.borrow().as_ref().and_then(Weak::load),
            ) else {
                return;
            };
            let at = event.locationInWindow();
            if !ivars.dragging.get() {
                if (at.x - press.x).abs() < DRAG_SLOP && (at.y - press.y).abs() < DRAG_SLOP {
                    return;
                }
                ivars.dragging.set(true);
                sidebar.drag_began();
            }
            sidebar.drag_moved(at);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let ivars = self.ivars();
            let was_drag = ivars.dragging.replace(false);
            ivars.press.set(None);
            if !was_drag {
                self.choose();
                return;
            }
            let tab = ivars.tab.borrow().as_ref().and_then(Weak::load);
            if let (Some(sidebar), Some(tab)) =
                (ivars.sidebar.borrow().as_ref().and_then(Weak::load), tab)
            {
                sidebar.drag_ended(event.locationInWindow(), &tab);
            }
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
            track(self);
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
        sidebar: Option<Weak<Sidebar>>,
        indent: f64,
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
            sidebar: RefCell::new(sidebar),
            press: Cell::new(None),
            dragging: Cell::new(false),
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
            NSPoint::new(ROW_PAD + indent, (ROW_HEIGHT - ICON) / 2.0),
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
        let text_x = ROW_PAD + indent + ICON + 8.0;
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
            None => send(self.mtm(), sel!(newWindowForTab:), None, None),
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

// --------------------------------------------------------------- a tile

pub struct TileIvars {
    address: String,
    title: String,
    hover: Cell<bool>,
    /// The window's controller, which opens the page and keeps the favorites.
    target: Weak<AnyObject>,
}

define_class!(
    /// One favorite, as a tile.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoFavoriteTile"]
    #[ivars = TileIvars]
    pub struct FavoriteTile;

    impl FavoriteTile {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let alpha = match (is_dark(self), self.ivars().hover.get()) {
                (true, true) => 0.18,
                (true, false) => 0.10,
                (false, true) => 0.72,
                (false, false) => 0.45,
            };
            lit(alpha).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                self.bounds(),
                TILE_RADIUS,
                TILE_RADIUS,
            )
            .fill();
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
            let mtm = self.mtm();
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Favorite"));
            let target = self.ivars().target.load();
            let me: &AnyObject = self.as_ref();
            let item = unsafe {
                objc2_app_kit::NSMenuItem::initWithTitle_action_keyEquivalent(
                    objc2_app_kit::NSMenuItem::alloc(mtm),
                    ns_string!("Remove from Favorites"),
                    Some(sel!(syndeoRemoveFavorite:)),
                    &NSString::new(),
                )
            };
            unsafe {
                item.setTarget(target.as_deref());
                item.setRepresentedObject(Some(me));
            }
            menu.addItem(&item);
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hover.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hover.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            track(self);
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
            let ivars = self.ivars();
            let label = if ivars.title.is_empty() {
                ivars.address.clone()
            } else {
                ivars.title.clone()
            };
            Some(NSString::from_str(&label))
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            self.choose();
            true
        }
    }
);

impl FavoriteTile {
    fn new(
        mtm: MainThreadMarker,
        size: f64,
        favorite: &FavoriteInfo,
        target: Weak<AnyObject>,
    ) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size, size));
        let this = Self::alloc(mtm).set_ivars(TileIvars {
            address: favorite.favorite.address.clone(),
            title: favorite.favorite.title.clone(),
            hover: Cell::new(false),
            target,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            this.widthAnchor().constraintEqualToConstant(size),
            this.heightAnchor().constraintEqualToConstant(size),
        ]));
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.favorite")));
        let title = if favorite.favorite.title.is_empty() {
            favorite.favorite.address.clone()
        } else {
            favorite.favorite.title.clone()
        };
        this.setToolTip(Some(&NSString::from_str(&title)));

        match &favorite.icon {
            Some(image) => {
                let icon = NSImageView::new(mtm);
                icon.setImage(Some(image));
                icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                icon.setFrame(NSRect::new(
                    NSPoint::new((size - TILE_ICON) / 2.0, (size - TILE_ICON) / 2.0),
                    NSSize::new(TILE_ICON, TILE_ICON),
                ));
                this.addSubview(&icon);
            }
            None => {
                let initial =
                    favorites::initial(&favorite.favorite.address, &favorite.favorite.title);
                let label = NSTextField::labelWithString(&NSString::from_str(&initial), mtm);
                label.setFont(Some(&NSFont::boldSystemFontOfSize(17.0)));
                label.setTextColor(Some(&NSColor::labelColor()));
                label.setAlignment(NSTextAlignment::Center);
                label.setFrame(NSRect::new(
                    NSPoint::new(0.0, (size - 22.0) / 2.0),
                    NSSize::new(size, 22.0),
                ));
                this.addSubview(&label);
            }
        }
        this
    }

    /// The favorite's address.
    pub fn address(&self) -> String {
        self.ivars().address.clone()
    }

    /// Open the page, in this tab.
    fn choose(&self) {
        let target = self.ivars().target.load();
        let me: &AnyObject = self.as_ref();
        send(
            self.mtm(),
            sel!(syndeoOpenFavorite:),
            target.as_deref(),
            Some(me),
        );
    }
}

// --------------------------------------------------------------- a folder

/// How far a folder's tabs are set in from its row.
const FOLDER_INDENT: f64 = 18.0;

pub struct FolderIvars {
    id: u64,
    hover: Cell<bool>,
    /// A dragged tab is over it.
    drop: Cell<bool>,
}

define_class!(
    /// A folder's row: a chevron, open or shut, the folder symbol and its
    /// name. A click folds it or opens it, a secondary click offers to
    /// rename or delete it, and a tab dragged onto it goes into it.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoFolderRow"]
    #[ivars = FolderIvars]
    pub struct FolderRow;

    impl FolderRow {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let ivars = self.ivars();
            let dark = is_dark(self);
            let alpha = match (ivars.drop.get(), ivars.hover.get()) {
                (true, _) if dark => 0.2,
                (true, _) => 0.7,
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
            let me: &AnyObject = self.as_ref();
            send(self.mtm(), sel!(syndeoToggleFolder:), None, Some(me));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let mtm = self.mtm();
            let id = self.ivars().id as isize;
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Folder"));
            for (title, action) in [
                ("Rename Folder…", sel!(syndeoRenameFolder:)),
                ("Delete Folder", sel!(syndeoDeleteFolder:)),
            ] {
                let item = unsafe {
                    objc2_app_kit::NSMenuItem::initWithTitle_action_keyEquivalent(
                        objc2_app_kit::NSMenuItem::alloc(mtm),
                        &NSString::from_str(title),
                        Some(action),
                        &NSString::new(),
                    )
                };
                item.setTag(id);
                menu.addItem(&item);
            }
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hover.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hover.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            track(self);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str("AXDisclosureTriangle"))
        }
    }
);

impl FolderRow {
    fn new(mtm: MainThreadMarker, width: f64, folder: &Folder) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, ROW_HEIGHT));
        let this = Self::alloc(mtm).set_ivars(FolderIvars {
            id: folder.id,
            hover: Cell::new(false),
            drop: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.heightAnchor()
            .constraintEqualToConstant(ROW_HEIGHT)
            .setActive(true);
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.folder")));
        this.setAccessibilityLabel(Some(&NSString::from_str(&folder.name)));

        let chevron = NSImageView::new(mtm);
        let (name, label) = if folder.collapsed {
            ("chevron.right", "Closed")
        } else {
            ("chevron.down", "Open")
        };
        chevron.setImage(symbol(name, label).as_deref());
        chevron.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
        chevron.setFrame(NSRect::new(
            NSPoint::new(2.0, (ROW_HEIGHT - 12.0) / 2.0),
            NSSize::new(12.0, 12.0),
        ));
        this.addSubview(&chevron);

        let icon = NSImageView::new(mtm);
        icon.setImage(symbol("folder.fill", "Folder").as_deref());
        icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        icon.setFrame(NSRect::new(
            NSPoint::new(ROW_PAD + 8.0, (ROW_HEIGHT - ICON) / 2.0),
            NSSize::new(ICON, ICON),
        ));
        this.addSubview(&icon);

        let title = NSTextField::labelWithString(&NSString::from_str(&folder.name), mtm);
        title.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        title.setTextColor(Some(&NSColor::labelColor()));
        let text_x = ROW_PAD + 8.0 + ICON + 8.0;
        title.setFrame(NSRect::new(
            NSPoint::new(text_x, (ROW_HEIGHT - 17.0) / 2.0),
            NSSize::new((width - text_x - ROW_PAD).max(10.0), 17.0),
        ));
        title.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
        this.addSubview(&title);
        this
    }

    /// The folder's id.
    pub fn id(&self) -> u64 {
        self.ivars().id
    }

    fn set_drop(&self, drop: bool) {
        if self.ivars().drop.replace(drop) != drop {
            self.setNeedsDisplay(true);
        }
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

pub struct EdgeIvars {
    target: Weak<AnyObject>,
}

define_class!(
    /// The window's left edge while the sidebar is hidden: the pointer
    /// arriving there asks the window's controller to show the sidebar
    /// over the page (`syndeoPeekSidebar:`).
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoEdgeSensor"]
    #[ivars = EdgeIvars]
    pub struct EdgeSensor;

    impl EdgeSensor {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            let target = self.ivars().target.load();
            send(self.mtm(), sel!(syndeoPeekSidebar:), target.as_deref(), None);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            track(self);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }
    }
);

impl EdgeSensor {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EdgeIvars {
            target: Weak::from_retained(&target.retain()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.setHidden(true);
        this
    }
}

/// The drop hint's least height, when there are no favorites yet.
const HINT_HEIGHT: f64 = 56.0;

pub struct HintIvars {
    /// A dragged tab is over it.
    active: Cell<bool>,
}

define_class!(
    /// Where a dragged tab becomes a favorite: a dashed outline over the
    /// favorites, filled while the tab is over it, saying what dropping
    /// does. Shown only while a tab is dragged.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoDropHint"]
    #[ivars = HintIvars]
    pub struct DropHint;

    impl DropHint {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let inset = NSRect::new(
                NSPoint::new(bounds.origin.x + 1.0, bounds.origin.y + 1.0),
                NSSize::new(bounds.size.width - 2.0, bounds.size.height - 2.0),
            );
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                inset,
                TILE_RADIUS,
                TILE_RADIUS,
            );
            let dark = is_dark(self);
            if self.ivars().active.get() {
                lit(if dark { 0.14 } else { 0.5 }).setFill();
                path.fill();
            }
            path.setLineWidth(1.5);
            let dashes: [f64; 2] = [5.0, 4.0];
            unsafe { path.setLineDash_count_phase(dashes.as_ptr(), 2, 0.0) };
            NSColor::secondaryLabelColor().setStroke();
            path.stroke();
        }
    }
);

impl DropHint {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(HintIvars {
            active: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.setHidden(true);
        let label = NSTextField::labelWithString(ns_string!("Add to Favorites"), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label.setAlignment(NSTextAlignment::Center);
        label.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.addSubview(&label);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            label
                .centerXAnchor()
                .constraintEqualToAnchor(&this.centerXAnchor()),
            label
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&this.bottomAnchor(), -8.0),
        ]));
        this
    }

    fn set_active(&self, active: bool) {
        if self.ivars().active.replace(active) != active {
            self.setNeedsDisplay(true);
        }
    }
}

pub struct SidebarIvars {
    list: Retained<NSStackView>,
    favorites: Retained<NSStackView>,
    menu_for: RefCell<Option<MenuFor>>,
    /// The window's controller, told of favorites chosen, and of the
    /// pointer leaving while the sidebar peeks.
    target: Weak<AnyObject>,
    /// Peeking over the page, as a floating panel, rather than beside it.
    floating: Cell<bool>,
    /// Where a dragged tab is dropped to become a favorite: over the
    /// favorites, shown while a tab is dragged.
    hint: Retained<DropHint>,
    /// The spaces, at the foot.
    spaces: Retained<SpaceBar>,
    /// Where the buttons across the top sit: lined up with the window's
    /// traffic lights (`align_nav`).
    nav_top: RefCell<Option<Retained<NSLayoutConstraint>>>,
}

define_class!(
    /// The sidebar itself: see the module's notes. Beside the page it draws
    /// nothing of its own, so the window's backdrop shows through it;
    /// peeking over the page, it draws itself as a floating panel.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoSidebar"]
    #[ivars = SidebarIvars]
    pub struct Sidebar;

    impl Sidebar {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if !self.ivars().floating.get() {
                return;
            }
            // The panel, in the space's colour as the sidebar always is, with
            // a hairline edge; the shadow is the layer's.
            let panel = self.panel_rect();
            fill_gradient(self, panel, Some(PANEL_RADIUS));
            let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                panel,
                PANEL_RADIUS,
                PANEL_RADIUS,
            );
            shape.setLineWidth(1.0);
            lit(if is_dark(self) { 0.14 } else { 0.6 }).setStroke();
            shape.stroke();
        }

        /// Dragging the sidebar's empty parts moves the window, as dragging
        /// a title bar does.
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            !self.ivars().floating.get()
        }

        /// Peeking, the sidebar goes when the pointer leaves it.
        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            if self.ivars().floating.get() {
                let target = self.ivars().target.load();
                send(self.mtm(), sel!(syndeoUnpeekSidebar:), target.as_deref(), None);
            }
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            track(self);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }
    }
);

impl Sidebar {
    /// The sidebar, with `nav` (Hide Sidebar, Back, Forward, Reload) across
    /// its top, right-aligned, and `field`, the address field, below them;
    /// its favorites and rows send to `target`, the window's controller.
    pub fn new(
        mtm: MainThreadMarker,
        nav: &[&NSView],
        field: &NSView,
        target: &AnyObject,
    ) -> Retained<Self> {
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
        let favorites = NSStackView::new(mtm);
        favorites.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        favorites.setAlignment(NSLayoutAttribute::Leading);
        favorites.setSpacing(TILE_GAP);
        favorites.setTranslatesAutoresizingMaskIntoConstraints(false);
        let hint = DropHint::new(mtm);
        let spaces = SpaceBar::new(mtm);
        let buttons: Vec<&NSView> = nav.to_vec();
        let row = NSStackView::stackViewWithViews(&NSArray::from_slice(&buttons), mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(2.0);
        row.setTranslatesAutoresizingMaskIntoConstraints(false);
        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        let this = Self::alloc(mtm).set_ivars(SidebarIvars {
            list: list.clone(),
            favorites: favorites.clone(),
            menu_for: RefCell::new(None),
            target: Weak::from_retained(&target.retain()),
            floating: Cell::new(false),
            hint: hint.clone(),
            spaces: spaces.clone(),
            nav_top: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        // Layer-backed, for the shadow it casts while it peeks.
        this.setWantsLayer(true);

        // The buttons across the top, right-aligned, clear of the traffic
        // lights at the left.
        this.addSubview(&row);
        this.addSubview(field);
        this.addSubview(&favorites);
        this.addSubview(&hint);
        this.addSubview(&spaces);

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
            this.nav_top_constraint(&row, NAV_TOP),
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
            favorites
                .topAnchor()
                .constraintEqualToAnchor_constant(&field.bottomAnchor(), LIST_GAP),
            favorites
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&this.leadingAnchor(), SIDE),
            favorites
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -SIDE),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&favorites.bottomAnchor(), LIST_GAP),
            scroll
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&this.leadingAnchor(), SIDE - 2.0),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -(SIDE - 2.0)),
            scroll
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&spaces.topAnchor(), -6.0),
            spaces
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&this.leadingAnchor(), SIDE),
            spaces
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -SIDE),
            spaces
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&this.bottomAnchor(), -(PANEL_MARGIN + 4.0)),
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
            hint.topAnchor()
                .constraintEqualToAnchor_constant(&favorites.topAnchor(), -6.0),
            hint.leadingAnchor()
                .constraintEqualToAnchor_constant(&this.leadingAnchor(), SIDE - 4.0),
            hint.trailingAnchor()
                .constraintEqualToAnchor_constant(&this.trailingAnchor(), -(SIDE - 4.0)),
            hint.bottomAnchor()
                .constraintGreaterThanOrEqualToAnchor_constant(&favorites.bottomAnchor(), 6.0),
            hint.heightAnchor()
                .constraintGreaterThanOrEqualToConstant(HINT_HEIGHT),
        ]));
        this
    }

    /// A tab's row is being dragged: show where it can be dropped.
    fn drag_began(&self) {
        let hint = &self.ivars().hint;
        hint.set_active(false);
        hint.setHidden(false);
    }

    /// The dragged row is at `at`, in the window.
    fn drag_moved(&self, at: NSPoint) {
        let hint = &self.ivars().hint;
        hint.set_active(self.over_hint(at));
        let under = self.folder_under(at);
        for row in self.folder_rows() {
            row.set_drop(under.as_deref().is_some_and(|u| std::ptr::eq(u, &*row)));
        }
    }

    /// The folder rows in the list.
    fn folder_rows(&self) -> Vec<Retained<FolderRow>> {
        self.ivars()
            .list
            .arrangedSubviews()
            .iter()
            .filter_map(|view| view.downcast::<FolderRow>().ok())
            .collect()
    }

    /// The folder row under `at`, in the window, if any.
    fn folder_under(&self, at: NSPoint) -> Option<Retained<FolderRow>> {
        self.folder_rows().into_iter().find(|row| {
            let point = row.convertPoint_fromView(at, None);
            let bounds = row.bounds();
            point.x >= bounds.origin.x
                && point.x <= bounds.origin.x + bounds.size.width
                && point.y >= bounds.origin.y
                && point.y <= bounds.origin.y + bounds.size.height
        })
    }

    /// The dragged row was let go at `at`, in the window: over the
    /// favorites, `tab`'s page becomes one; over a folder, the tab goes
    /// into it; both through the tab's controller.
    fn drag_ended(&self, at: NSPoint, tab: &NSWindow) {
        let hint = &self.ivars().hint;
        let over = self.over_hint(at);
        hint.set_active(false);
        hint.setHidden(true);
        let folder = self.folder_under(at);
        for row in self.folder_rows() {
            row.set_drop(false);
        }
        let Some(delegate) = tab.delegate() else {
            return;
        };
        let target: &AnyObject = (*delegate).as_ref();
        if over {
            send(self.mtm(), sel!(syndeoAddFavorite:), Some(target), None);
        } else if let Some(folder) = folder {
            let sender: &AnyObject = folder.as_ref();
            send(
                self.mtm(),
                sel!(syndeoDropOnFolder:),
                Some(target),
                Some(sender),
            );
        }
    }

    /// Whether `at`, in the window, is over the drop hint.
    fn over_hint(&self, at: NSPoint) -> bool {
        let point = self.convertPoint_fromView(at, None);
        let frame = self.ivars().hint.frame();
        point.x >= frame.origin.x
            && point.x <= frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y <= frame.origin.y + frame.size.height
    }

    /// The buttons' top constraint, made once and kept, to move them.
    fn nav_top_constraint(&self, row: &NSView, top: f64) -> Retained<NSLayoutConstraint> {
        let constraint = row
            .topAnchor()
            .constraintEqualToAnchor_constant(&self.topAnchor(), top);
        *self.ivars().nav_top.borrow_mut() = Some(constraint.clone());
        constraint
    }

    /// Line the buttons across the top up with the window's traffic lights,
    /// whose centre is `center` below the window's top.
    pub fn align_nav(&self, center: f64) {
        if let Some(top) = &*self.ivars().nav_top.borrow() {
            top.setConstant((center - NAV_HEIGHT / 2.0).max(4.0));
        }
    }

    /// Answer a secondary click on a row with the application's menu for
    /// its tab.
    pub fn set_menu(&self, menu_for: MenuFor) {
        *self.ivars().menu_for.borrow_mut() = Some(menu_for);
    }

    /// Peek over the page as a floating panel, or sit beside it.
    pub fn set_floating(&self, floating: bool) {
        self.ivars().floating.set(floating);
        // A shadow under the panel, while it floats over the page.
        let layer: *mut AnyObject = unsafe { msg_send![self, layer] };
        if !layer.is_null() {
            let opacity: f32 = if floating { 0.38 } else { 0.0 };
            unsafe {
                let _: () = msg_send![layer, setMasksToBounds: false];
                let _: () = msg_send![layer, setShadowOpacity: opacity];
                let _: () = msg_send![layer, setShadowRadius: 26.0f64];
                let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -10.0)];
            }
        }
        self.setNeedsDisplay(true);
    }

    /// Where the floating panel is drawn: the sidebar's bounds, less a
    /// margin all round, so that it floats clear of the window's edges and
    /// the page's card to its right.
    fn panel_rect(&self) -> NSRect {
        let bounds = self.bounds();
        NSRect::new(
            NSPoint::new(PANEL_MARGIN, PANEL_MARGIN),
            NSSize::new(
                (bounds.size.width - 2.0 * PANEL_MARGIN).max(0.0),
                (bounds.size.height - PANEL_MARGIN - PANEL_TOP).max(0.0),
            ),
        )
    }

    /// Show `favorites` as tiles, `layout`'s spaces at the foot, and the
    /// current space's `tabs` as rows: its folders and their tabs, the
    /// pinned tabs in none, a rule, the rest, New Tab.
    pub fn reload(&self, tabs: &[TabInfo], favorites: &[FavoriteInfo], layout: &Layout) {
        let mtm = self.mtm();
        let ivars = self.ivars();
        let clear = |stack: &NSStackView| {
            for view in stack.arrangedSubviews().iter() {
                stack.removeArrangedSubview(&view);
                view.removeFromSuperview();
            }
        };

        clear(&ivars.favorites);
        let inner = WIDTH - 2.0 * SIDE;
        let tile = (inner - TILE_GAP * (TILES_PER_ROW as f64 - 1.0)) / TILES_PER_ROW as f64;
        for row in favorites.chunks(TILES_PER_ROW) {
            let tiles: Vec<Retained<FavoriteTile>> = row
                .iter()
                .map(|f| FavoriteTile::new(mtm, tile, f, ivars.target.clone()))
                .collect();
            let views: Vec<&NSView> = tiles.iter().map(|t| -> &NSView { t }).collect();
            let line = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
            line.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            line.setSpacing(TILE_GAP);
            line.setTranslatesAutoresizingMaskIntoConstraints(false);
            ivars.favorites.addArrangedSubview(&line);
        }

        ivars.spaces.reload(layout);

        clear(&ivars.list);
        let menu_for = ivars.menu_for.borrow().clone();
        let width = WIDTH - 2.0 * (SIDE - 2.0);
        let me = || Some(Weak::from_retained(&self.retain()));
        let row_for = |tab: &TabInfo, indent: f64| {
            TabRow::new(mtm, width, Some(tab), menu_for.clone(), me(), indent)
        };

        // The current space's name, over its tabs.
        let space = layout.current_space();
        let heading = if space.icon.is_empty() {
            space.name.clone()
        } else {
            format!("{}  {}", space.icon, space.name)
        };
        let name = NSTextField::labelWithString(&NSString::from_str(&heading), mtm);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
        name.setTextColor(Some(&NSColor::secondaryLabelColor()));
        name.setTranslatesAutoresizingMaskIntoConstraints(false);
        let header = NSView::new(mtm);
        header.setTranslatesAutoresizingMaskIntoConstraints(false);
        header.addSubview(&name);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            header.heightAnchor().constraintEqualToConstant(22.0),
            name.leadingAnchor()
                .constraintEqualToAnchor_constant(&header.leadingAnchor(), ROW_PAD),
            name.centerYAnchor()
                .constraintEqualToAnchor(&header.centerYAnchor()),
        ]));
        ivars.list.addArrangedSubview(&header);

        // This space's tabs only: its folders and their tabs, the pinned
        // tabs in no folder, a rule, then the rest.
        let here: Vec<&TabInfo> = tabs.iter().filter(|t| t.space == space.id).collect();
        let folders = layout.folders_of(space.id);
        let in_folder = |tab: &TabInfo| {
            tab.pinned
                && tab
                    .folder
                    .is_some_and(|id| folders.iter().any(|f| f.id == id))
        };
        for folder in &folders {
            let row = FolderRow::new(mtm, width, folder);
            ivars.list.addArrangedSubview(&row);
            if folder.collapsed {
                continue;
            }
            for tab in here
                .iter()
                .filter(|t| t.pinned && t.folder == Some(folder.id))
            {
                ivars.list.addArrangedSubview(&row_for(tab, FOLDER_INDENT));
            }
        }
        let loose: Vec<&&TabInfo> = here.iter().filter(|t| t.pinned && !in_folder(t)).collect();
        for tab in &loose {
            ivars.list.addArrangedSubview(&row_for(tab, 0.0));
        }
        let unpinned: Vec<&&TabInfo> = here.iter().filter(|t| !t.pinned).collect();
        if (!folders.is_empty() || !loose.is_empty()) && !unpinned.is_empty() {
            let rule = NSBox::new(mtm);
            rule.setBoxType(NSBoxType::Separator);
            rule.setTranslatesAutoresizingMaskIntoConstraints(false);
            ivars.list.addArrangedSubview(&rule);
        }
        for tab in &unpinned {
            ivars.list.addArrangedSubview(&row_for(tab, 0.0));
        }
        let new_tab = TabRow::new(mtm, width, None, None, None, 0.0);
        ivars.list.addArrangedSubview(&new_tab);
    }
}
