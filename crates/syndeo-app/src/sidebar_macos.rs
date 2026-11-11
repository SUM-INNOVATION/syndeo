//! The sidebar: a browser window's tabs down its left side,
//! with Hide Sidebar, Back, Forward and Reload across its top, the address
//! field as a pill below them, the favorites as a grid of tiles under that,
//! and the tabs below.
//!
//! Every tab is still a window of its own, tabbed with the others by AppKit
//! (`tabs_macos`): the sidebar is a view of that tab group, and only the
//! window in front shows it, so only that window's sidebar holds the
//! group's rows; the others hold none (`sidebar_model`). When another tab
//! comes to the front, the rows, the favorites' tiles and the list's own
//! lines move to its sidebar (`Sidebar::adopt`), and what changed is
//! changed on them (`Sidebar::show`): a row is made once for its tab and
//! kept while the tab is in the list. AppKit's own tab bar is kept hidden
//! (`tabs_macos::hide_tab_bar`).
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
//!   goes again when the pointer leaves it. The panel is drawn
//!   here; what shows and hides it is the window's chrome (`chrome_macos`),
//!   which the sensor and the sidebar tell through `syndeoPeekSidebar:` and
//!   `syndeoUnpeekSidebar:`, sent to the window's controller.
//!
//! An agent's window (`control_macos`) shows none of the person's
//! favorites, and offers no way to add one, nor any of their spaces: no
//! switcher, folder or space's name, and every one of the agent's tabs is
//! listed, whatever space is current (`sidebar_model::shows`).
//!
//! Nothing here fetches anything: icons are the ones each tab's badge has.
//! To Accessibility each row is a button named for its tab, selected while
//! its tab is in front, and the UI test finds the rows by
//! `syndeo.sidebar-tab` and the tiles by `syndeo.favorite`.

use crate::favorites::{self, Favorite};
use crate::sidebar_model::{self, Item, Owner, Shows};
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
use std::collections::HashMap;
use std::rc::Rc;

/// The sidebar's width, open.
pub const WIDTH: f64 = 248.0;

/// Inside the sidebar: the margin at either side, where the top row of
/// buttons sits below the window's top (clear of the traffic lights, which
/// take the first 80 points of its width), and the gaps below the address
/// field and the favorites.
pub const SIDE: f64 = 12.0;
pub const NAV_TOP: f64 = 8.0;
pub const NAV_HEIGHT: f64 = 28.0;
const ADDRESS_TOP: f64 = NAV_TOP + NAV_HEIGHT + 12.0;
const LIST_GAP: f64 = 18.0;

/// The space's name over its tabs: its row's height; and the room left
/// after it, and around the rule between the pinned tabs and the rest.
const HEADER_HEIGHT: f64 = 30.0;
const SECTION_GAP: f64 = 10.0;

/// The floating panel, when the sidebar peeks over the page: its margin
/// from the window's left and bottom, and its corner radius.
pub const PANEL_MARGIN: f64 = 8.0;
const PANEL_TOP: f64 = 0.0;
const PANEL_RADIUS: f64 = 12.0;
/// How far the sidebar's content moves in from its edges while it floats,
/// so that it sits inside the panel as it sits inside the window otherwise.
pub const FLOATING_INSET: f64 = PANEL_MARGIN + 4.0;
/// And how far it moves down, so the traffic lights and buttons are not
/// in the panel's very corner.
pub const FLOATING_TOP: f64 = 8.0;

/// The edge of the window that brings a hidden sidebar back.
pub const EDGE: f64 = 6.0;

/// A row: its height, its corner radius, the icon's size, and the gaps.
const ROW_HEIGHT: f64 = 36.0;
const ROW_RADIUS: f64 = 9.0;
const ROW_PAD: f64 = 10.0;
const ICON: f64 = 18.0;
const CLOSE: f64 = 20.0;
const ROW_GAP: f64 = 3.0;

/// How far a press has to move to be a drag, not a click.
const DRAG_SLOP: f64 = 4.0;

/// A favorite's tile: four to a row, with this much between them.
const TILES_PER_ROW: usize = 4;
const TILE_GAP: f64 = 8.0;
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

/// An SF Symbol, if macOS has it.
type Symbol = Option<Retained<NSImage>>;

/// A folder's row, with the name and fold it was made with.
type FolderLine = (Retained<FolderRow>, String, bool);

thread_local! {
    /// The SF Symbols the sidebar shows, each made once and shared by every
    /// row that shows it.
    static SYMBOLS: RefCell<HashMap<(&'static str, &'static str), Symbol>> =
        RefCell::new(HashMap::new());
    /// Rows, New Tab's included, that exist; and tabs' rows held by the
    /// sidebars, which is one for each tab in the windows in front.
    static ROWS_ALIVE: Cell<usize> = const { Cell::new(0) };
    static ROWS_HELD: Cell<usize> = const { Cell::new(0) };
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
fn symbol(name: &'static str, label: &'static str) -> Option<Retained<NSImage>> {
    SYMBOLS.with(|symbols| {
        symbols
            .borrow_mut()
            .entry((name, label))
            .or_insert_with(|| {
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(name),
                    Some(&NSString::from_str(label)),
                )
            })
            .clone()
    })
}

/// How many rows exist, New Tab's included: measured, for the log.
pub fn rows_alive() -> usize {
    ROWS_ALIVE.with(Cell::get)
}

/// How many tabs' rows the sidebars hold between them.
pub fn rows_held() -> usize {
    ROWS_HELD.with(Cell::get)
}

fn held_more(by: usize) {
    ROWS_HELD.with(|held| held.set(held.get() + by));
}

fn held_fewer(by: usize) {
    ROWS_HELD.with(|held| held.set(held.get().saturating_sub(by)));
}

/// `view`, out of `stack` and out of its superview, but kept by whoever
/// holds it.
fn detach(stack: &NSStackView, view: &NSView) {
    stack.removeArrangedSubview(view);
    view.removeFromSuperview();
}

/// Everything in `stack`, out of it: the last first, so that nothing
/// after each has to move up.
fn detach_all(stack: &NSStackView) {
    for view in stack.arrangedSubviews().to_vec().iter().rev() {
        detach(stack, view);
    }
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
    /// The New Tab row, which never has Close.
    new_tab: bool,
    title: RefCell<String>,
    active: Cell<bool>,
    hover: Cell<bool>,
    pinned: Cell<bool>,
    /// Shown under its folder, set in.
    in_folder: Cell<bool>,
    /// What the icon shows: whether the page is loading, and the page's
    /// icon, by its address.
    shows: Cell<Option<(bool, usize)>>,
    icon: Retained<NSImageView>,
    label: Retained<NSTextField>,
    /// Close, at the row's end: made the first time it is shown, for the
    /// tab in front or a row pointed at.
    close: RefCell<Option<Retained<NSButton>>>,
    /// The pin at the row's end, for a pinned tab.
    pin: RefCell<Option<Retained<NSImageView>>>,
    menu_for: RefCell<Option<MenuFor>>,
    /// Where the mouse went down, in the window, while it is down; and
    /// whether it has moved far enough since to be a drag.
    press: Cell<Option<NSPoint>>,
    dragging: Cell<bool>,
}

impl Drop for RowIvars {
    fn drop(&mut self) {
        ROWS_ALIVE.with(|alive| alive.set(alive.get().saturating_sub(1)));
    }
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
            let Some(press) = ivars.press.get() else {
                return;
            };
            let Some(sidebar) = self.sidebar() else {
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

        /// What a release does can change the sidebar, and so take this row
        /// out of it: the row is kept until it returns, and nothing of it is
        /// borrowed meanwhile.
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let _keep = self.retain();
            let ivars = self.ivars();
            let was_drag = ivars.dragging.replace(false);
            ivars.press.set(None);
            if !was_drag {
                self.choose();
                return;
            }
            let tab = ivars.tab.borrow().as_ref().and_then(Weak::load);
            let sidebar = self.sidebar();
            if let (Some(sidebar), Some(tab)) = (sidebar, tab) {
                sidebar.drag_ended(event.locationInWindow(), &tab);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let _keep = self.retain();
            let ivars = self.ivars();
            let tab = ivars.tab.borrow().as_ref().and_then(Weak::load);
            let menu_for = ivars.menu_for.borrow().clone();
            let (Some(tab), Some(menu_for)) = (tab, menu_for) else {
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

        /// The row of the tab in front is the one selected, as it is lit.
        #[unsafe(method(isAccessibilitySelected))]
        fn is_accessibility_selected(&self) -> bool {
            self.ivars().active.get()
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            let _keep = self.retain();
            self.choose();
            true
        }

        /// Close, at the row's end. Closing the tab takes its row out of
        /// the sidebar: the row is kept until this returns, and its window
        /// is not borrowed from it meanwhile.
        #[unsafe(method(syndeoCloseRow:))]
        fn close_row(&self, _sender: Option<&AnyObject>) {
            let _keep = self.retain();
            let tab = self.ivars().tab.borrow().as_ref().and_then(Weak::load);
            if let Some(tab) = tab {
                tab.performClose(None);
            }
        }
    }
);

impl TabRow {
    /// A row for `tab`, set in when it is `in_folder`; or for New Tab when
    /// there is none.
    fn new(
        mtm: MainThreadMarker,
        width: f64,
        tab: Option<(&TabInfo, bool)>,
        menu_for: Option<MenuFor>,
    ) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, ROW_HEIGHT));
        // The icon: the page's, a globe without one, an hourglass while
        // the page loads, and a plus for New Tab.
        let icon = NSImageView::new(mtm);
        icon.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        // The title, clipped at its end.
        let label = NSTextField::labelWithString(&NSString::new(), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        let colour = if tab.is_none() {
            NSColor::secondaryLabelColor()
        } else {
            NSColor::labelColor()
        };
        label.setTextColor(Some(&colour));
        label.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
        let this = Self::alloc(mtm).set_ivars(RowIvars {
            tab: RefCell::new(None),
            new_tab: tab.is_none(),
            title: RefCell::new(String::new()),
            active: Cell::new(false),
            hover: Cell::new(false),
            pinned: Cell::new(false),
            in_folder: Cell::new(false),
            shows: Cell::new(None),
            icon: icon.clone(),
            label: label.clone(),
            close: RefCell::new(None),
            pin: RefCell::new(None),
            menu_for: RefCell::new(menu_for),
            press: Cell::new(None),
            dragging: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        ROWS_ALIVE.with(|alive| alive.set(alive.get() + 1));
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.heightAnchor()
            .constraintEqualToConstant(ROW_HEIGHT)
            .setActive(true);
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.sidebar-tab")));
        this.addSubview(&icon);
        this.addSubview(&label);
        this.place(false);
        match tab {
            None => {
                *this.ivars().title.borrow_mut() = "New Tab".to_string();
                label.setStringValue(ns_string!("New Tab"));
                icon.setImage(symbol("plus", "New Tab").as_deref());
            }
            Some((tab, in_folder)) => this.update(tab, in_folder),
        }
        this
    }

    /// Show `tab` as it is now, set in when `in_folder`: changing only what
    /// changed.
    fn update(&self, tab: &TabInfo, in_folder: bool) {
        let ivars = self.ivars();
        *ivars.tab.borrow_mut() = Some(Weak::from_retained(&tab.window));
        if *ivars.title.borrow() != tab.title {
            *ivars.title.borrow_mut() = tab.title.clone();
            ivars.label.setStringValue(&NSString::from_str(&tab.title));
        }
        let shows = (
            tab.loading,
            tab.icon
                .as_ref()
                .map_or(0, |i| Retained::as_ptr(i) as usize),
        );
        if ivars.shows.replace(Some(shows)) != Some(shows) {
            let image = if tab.loading {
                symbol("hourglass", "Loading")
            } else {
                tab.icon.clone().or_else(|| symbol("globe", "Page"))
            };
            ivars.icon.setImage(image.as_deref());
            // What the UI test finds a tab's state by, as the tab bar's
            // badge once showed it: loading, or the page's own icon.
            let state = if tab.loading {
                Some(ns_string!("syndeo.tab-loading"))
            } else if tab.icon.is_some() {
                Some(ns_string!("syndeo.tab-icon"))
            } else {
                None
            };
            ivars.icon.setAccessibilityIdentifier(state);
        }
        if ivars.in_folder.replace(in_folder) != in_folder {
            self.place(in_folder);
        }
        let pinned = ivars.pinned.replace(tab.pinned) != tab.pinned;
        if pinned {
            self.show_pin(tab.pinned);
        }
        let active = ivars.active.replace(tab.active) != tab.active;
        if pinned || active {
            self.show_close();
            self.setNeedsDisplay(true);
        }
    }

    /// The icon and the title where they go, set in under a folder.
    fn place(&self, in_folder: bool) {
        let ivars = self.ivars();
        let indent = if in_folder { FOLDER_INDENT } else { 0.0 };
        ivars.icon.setFrame(NSRect::new(
            NSPoint::new(ROW_PAD + indent, (ROW_HEIGHT - ICON) / 2.0),
            NSSize::new(ICON, ICON),
        ));
        let text_x = ROW_PAD + indent + ICON + 8.0;
        let text_w = self.bounds().size.width - text_x - CLOSE - ROW_PAD;
        ivars.label.setFrame(NSRect::new(
            NSPoint::new(text_x, (ROW_HEIGHT - 18.0) / 2.0),
            NSSize::new(text_w.max(10.0), 18.0),
        ));
    }

    /// Where Close, or the pin, goes: the row's end.
    fn end(&self) -> NSRect {
        NSRect::new(
            NSPoint::new(
                self.bounds().size.width - ROW_PAD - CLOSE,
                (ROW_HEIGHT - CLOSE) / 2.0,
            ),
            NSSize::new(CLOSE, CLOSE),
        )
    }

    /// The pin at the row's end, for a pinned tab, or none.
    fn show_pin(&self, pinned: bool) {
        let ivars = self.ivars();
        if !pinned {
            if let Some(pin) = &*ivars.pin.borrow() {
                pin.setHidden(true);
            }
            return;
        }
        if let Some(pin) = &*ivars.pin.borrow() {
            pin.setHidden(false);
            return;
        }
        let pin = NSImageView::new(self.mtm());
        pin.setImage(symbol("pin.fill", "Pinned").as_deref());
        pin.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
        pin.setAccessibilityIdentifier(Some(ns_string!("syndeo.tab-pinned")));
        pin.setFrame(self.end());
        pin.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinXMargin);
        self.addSubview(&pin);
        *ivars.pin.borrow_mut() = Some(pin);
    }

    /// Close, shown for the tab in front and a row pointed at, and never
    /// for a pinned tab or New Tab; made the first time it is shown, as a
    /// plain button, which costs a row half what one made with
    /// `+buttonWithImage:target:action:` does.
    fn show_close(&self) {
        let ivars = self.ivars();
        let wanted =
            !ivars.new_tab && !ivars.pinned.get() && (ivars.hover.get() || ivars.active.get());
        if let Some(close) = &*ivars.close.borrow() {
            close.setHidden(!wanted);
            return;
        }
        if !wanted {
            return;
        }
        let close = NSButton::initWithFrame(NSButton::alloc(self.mtm()), self.end());
        close.setImage(symbol("xmark", "Close Tab").as_deref());
        close.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
        close.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        close.setBordered(false);
        let target: &AnyObject = self.as_ref();
        unsafe {
            close.setTarget(Some(target));
            close.setAction(Some(sel!(syndeoCloseRow:)));
        }
        close.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        close.setToolTip(Some(ns_string!("Close Tab")));
        close.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinXMargin);
        self.addSubview(&close);
        *ivars.close.borrow_mut() = Some(close);
    }

    /// The sidebar the row is in, if it is in one: the window in front's,
    /// which can change.
    fn sidebar(&self) -> Option<Retained<Sidebar>> {
        // SAFETY: on the main thread, each superview is retained as it is
        // read, and nothing changes the views in between.
        let mut view = unsafe { self.superview() };
        while let Some(v) = view {
            match v.downcast::<Sidebar>() {
                Ok(sidebar) => return Some(sidebar),
                Err(v) => view = unsafe { v.superview() },
            }
        }
        None
    }

    /// Bring the tab in front, or open a new one.
    fn choose(&self) {
        let tab = self.ivars().tab.borrow().as_ref().and_then(Weak::load);
        match tab {
            Some(window) => {
                window.makeKeyAndOrderFront(None);
                // Its sidebar shows the rows before it is drawn, whether or
                // not the application is active and the window became key.
                crate::browser_macos::refresh_sidebars(&window);
            }
            None => send(self.mtm(), sel!(newWindowForTab:), None, None),
        }
    }

    fn set_hover(&self, hover: bool) {
        self.ivars().hover.set(hover);
        self.show_close();
        self.setNeedsDisplay(true);
    }
}

// --------------------------------------------------------------- a tile

pub struct TileIvars {
    address: String,
    title: String,
    hover: Cell<bool>,
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
            let _keep = self.retain();
            let mtm = self.mtm();
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Favorite"));
            let target = self.target();
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
    fn new(mtm: MainThreadMarker, size: f64, favorite: &FavoriteInfo) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size, size));
        let this = Self::alloc(mtm).set_ivars(TileIvars {
            address: favorite.favorite.address.clone(),
            title: favorite.favorite.title.clone(),
            hover: Cell::new(false),
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

    /// The controller of the window the tile is in, which opens the page
    /// and keeps the favorites: the window in front's, which can change.
    fn target(&self) -> Option<Retained<AnyObject>> {
        let delegate = self.window()?.delegate()?;
        let object: &AnyObject = (*delegate).as_ref();
        Some(object.retain())
    }

    /// Open the page, in this tab.
    fn choose(&self) {
        let _keep = self.retain();
        let target = self.target();
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
            let _keep = self.retain();
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
        title.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        title.setTextColor(Some(&NSColor::labelColor()));
        let text_x = ROW_PAD + 8.0 + ICON + 8.0;
        title.setFrame(NSRect::new(
            NSPoint::new(text_x, (ROW_HEIGHT - 18.0) / 2.0),
            NSSize::new((width - text_x - ROW_PAD).max(10.0), 18.0),
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
    /// The content's distance from the sidebar's left, right, bottom and
    /// top: nothing beside the page, the panel's margins while floating.
    insets: RefCell<Vec<Retained<NSLayoutConstraint>>>,
    /// It shows the tab group's tabs: its window is in front. Only then
    /// does it hold anything below.
    showing: Cell<bool>,
    /// A row for each tab, by the address of the tab's window.
    rows: RefCell<HashMap<usize, Retained<TabRow>>>,
    /// The folders' rows, by id, with the name and fold each was made with.
    folder_rows: RefCell<HashMap<u64, FolderLine>>,
    /// The list's own lines: the space's name over the tabs (and its
    /// words), the rule, and New Tab.
    header: RefCell<Option<(Retained<NSView>, Retained<NSTextField>)>>,
    rule: RefCell<Option<Retained<NSView>>>,
    new_tab: RefCell<Option<Retained<TabRow>>>,
    /// The favorites the tiles were made for: address, title and icon.
    tiles_for: RefCell<Option<Vec<(String, String, usize)>>>,
    /// What it shows besides the tabs, as its tab group's owner allows:
    /// nothing of the person's until it is first shown.
    shows: Cell<Shows>,
}

impl Drop for SidebarIvars {
    fn drop(&mut self) {
        held_fewer(self.rows.get_mut().len());
    }
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
            insets: RefCell::new(Vec::new()),
            showing: Cell::new(false),
            rows: RefCell::new(HashMap::new()),
            folder_rows: RefCell::new(HashMap::new()),
            header: RefCell::new(None),
            rule: RefCell::new(None),
            new_tab: RefCell::new(None),
            tiles_for: RefCell::new(None),
            shows: Cell::new(sidebar_model::shows(Owner::Agent)),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        // Layer-backed, for the shadow it casts while it peeks.
        this.setWantsLayer(true);

        // Everything sits in one content view, which moves in from the
        // sidebar's edges while the sidebar floats as a panel.
        let content = NSView::new(mtm);
        content.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.addSubview(&content);

        // The buttons across the top, right-aligned, clear of the traffic
        // lights at the left.
        content.addSubview(&row);
        content.addSubview(field);
        content.addSubview(&favorites);
        content.addSubview(&hint);
        content.addSubview(&spaces);

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
        content.addSubview(&scroll);

        let leading = content
            .leadingAnchor()
            .constraintEqualToAnchor(&this.leadingAnchor());
        let trailing = content
            .trailingAnchor()
            .constraintEqualToAnchor(&this.trailingAnchor());
        let bottom = content
            .bottomAnchor()
            .constraintEqualToAnchor(&this.bottomAnchor());
        let top = content
            .topAnchor()
            .constraintEqualToAnchor(&this.topAnchor());
        *this.ivars().insets.borrow_mut() = vec![
            leading.clone(),
            trailing.clone(),
            bottom.clone(),
            top.clone(),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            leading,
            trailing,
            bottom,
            top,
            this.nav_top_constraint(&row, NAV_TOP),
            row.trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -SIDE),
            row.heightAnchor().constraintEqualToConstant(NAV_HEIGHT),
            field
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), ADDRESS_TOP),
            field
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), SIDE),
            field
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -SIDE),
            favorites
                .topAnchor()
                .constraintEqualToAnchor_constant(&field.bottomAnchor(), LIST_GAP),
            favorites
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), SIDE),
            favorites
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -SIDE),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&favorites.bottomAnchor(), LIST_GAP),
            scroll
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), SIDE - 2.0),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -(SIDE - 2.0)),
            scroll
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&spaces.topAnchor(), -6.0),
            spaces
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), SIDE),
            spaces
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -SIDE),
            spaces
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -(PANEL_MARGIN + 4.0)),
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
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), SIDE - 4.0),
            hint.trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -(SIDE - 4.0)),
            hint.bottomAnchor()
                .constraintGreaterThanOrEqualToAnchor_constant(&favorites.bottomAnchor(), 6.0),
            hint.heightAnchor()
                .constraintGreaterThanOrEqualToConstant(HINT_HEIGHT),
        ]));
        this
    }

    /// A tab's row is being dragged: show where it can be dropped. Not to
    /// the favorites in an agent's window, which has none.
    fn drag_began(&self) {
        let hint = &self.ivars().hint;
        hint.set_active(false);
        hint.setHidden(!self.ivars().shows.get().favorites);
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

    /// Whether `at`, in the window, is over the drop hint, which shows only
    /// where the favorites do.
    fn over_hint(&self, at: NSPoint) -> bool {
        if !self.ivars().shows.get().favorites {
            return false;
        }
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
        let inset = if floating { FLOATING_INSET } else { 0.0 };
        let bottom = if floating { PANEL_MARGIN } else { 0.0 };
        if let [leading, trailing, below, above] = &self.ivars().insets.borrow()[..] {
            leading.setConstant(inset);
            trailing.setConstant(-inset);
            below.setConstant(-bottom);
            above.setConstant(if floating { FLOATING_TOP } else { 0.0 });
        }
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

    /// Whether it shows the tab group's tabs, and holds what it shows.
    pub fn is_showing(&self) -> bool {
        self.ivars().showing.get()
    }

    /// Show the tab group's tabs, its window being in front: `items` in
    /// order, each tab's row as `tabs` has it, keyed by the address of the
    /// tab's window; `favorites` as tiles, and `layout`'s spaces at the foot,
    /// as far as `shows` allows: in an agent's window, neither.
    /// A row is kept for its tab and changed in place; rows are made only
    /// for tabs that have none, and let go only for tabs that are gone.
    /// What was made and let go, in rows.
    pub fn show(
        &self,
        items: &[Item<usize>],
        tabs: &HashMap<usize, &TabInfo>,
        favorites: &[FavoriteInfo],
        layout: &Layout,
        shows: Shows,
    ) -> (usize, usize) {
        let mtm = self.mtm();
        let ivars = self.ivars();
        ivars.showing.set(true);
        ivars.shows.set(shows);
        self.show_favorites(if shows.favorites { favorites } else { &[] });
        ivars.spaces.reload(layout, shows.spaces);

        // The tabs' rows: those kept, those made, those let go.
        let width = WIDTH - 2.0 * (SIDE - 2.0);
        let want = sidebar_model::row_keys(items);
        let have: Vec<usize> = ivars.rows.borrow().keys().copied().collect();
        let diff = sidebar_model::diff_rows(&have, &want);
        {
            let mut rows = ivars.rows.borrow_mut();
            for key in &diff.drop {
                if let Some(row) = rows.remove(key) {
                    detach(&ivars.list, &row);
                }
            }
            let menu_for = ivars.menu_for.borrow().clone();
            for key in &diff.make {
                if let Some(tab) = tabs.get(key) {
                    rows.insert(
                        *key,
                        TabRow::new(mtm, width, Some((tab, false)), menu_for.clone()),
                    );
                }
            }
        }
        held_fewer(diff.drop.len());
        held_more(diff.make.len());

        // Every line, in order, each changed only where it changed.
        let mut lines: Vec<Retained<NSView>> = Vec::with_capacity(items.len());
        let mut folders: Vec<u64> = Vec::new();
        for item in items {
            match *item {
                Item::Header => lines.push(self.header(layout)),
                Item::Folder(id) => {
                    if let Some(folder) = layout.folder(id) {
                        folders.push(id);
                        lines.push(Retained::into_super(self.folder_row(folder)));
                    }
                }
                Item::Tab { key, in_folder } => {
                    let row = ivars.rows.borrow().get(&key).cloned();
                    if let (Some(row), Some(tab)) = (row, tabs.get(&key)) {
                        row.update(tab, in_folder);
                        lines.push(Retained::into_super(row));
                    }
                }
                Item::Rule => lines.push(self.rule()),
                Item::NewTab => lines.push(Retained::into_super(self.new_tab_row())),
            }
        }
        ivars.folder_rows.borrow_mut().retain(|id, (row, _, _)| {
            let kept = folders.contains(id);
            if !kept {
                detach(&ivars.list, row);
            }
            kept
        });

        // In the order wanted: arranged again only when that is not the
        // order they are in, so that the list's subviews, which
        // Accessibility reads in order, are in the order shown.
        let now = ivars.list.arrangedSubviews();
        let same = now.len() == lines.len()
            && now.iter().zip(&lines).all(|(a, b)| std::ptr::eq(&*a, &**b));
        if !same {
            detach_all(&ivars.list);
            for line in &lines {
                ivars.list.addArrangedSubview(line);
            }
            if let Some((header, _)) = &*ivars.header.borrow() {
                ivars
                    .list
                    .setCustomSpacing_afterView(SECTION_GAP - ROW_GAP, header);
            }
        }
        (diff.make.len(), diff.drop.len())
    }

    /// Hold nothing: the window is behind another tab of its group, and
    /// its sidebar is not seen. How many tabs' rows were let go.
    pub fn empty(&self) -> usize {
        let ivars = self.ivars();
        ivars.showing.set(false);
        detach_all(&ivars.list);
        let dropped = ivars.rows.borrow_mut().drain().count();
        held_fewer(dropped);
        ivars.folder_rows.borrow_mut().clear();
        ivars.header.borrow_mut().take();
        ivars.rule.borrow_mut().take();
        ivars.new_tab.borrow_mut().take();
        self.clear_favorites();
        ivars.spaces.clear();
        dropped
    }

    /// Take what `from` holds, made while its window was in front: its
    /// rows, folders, lines and tiles, for this one's window, which is in
    /// front now; `from` holds nothing after. The rows are then shown as
    /// they are (`show`), not made again.
    pub fn adopt(&self, from: &Sidebar) {
        if std::ptr::eq(self, from) {
            return;
        }
        let (mine, theirs) = (self.ivars(), from.ivars());
        self.empty();
        detach_all(&theirs.list);
        let lines = theirs.favorites.arrangedSubviews();
        detach_all(&theirs.favorites);
        for line in lines.iter() {
            mine.favorites.addArrangedSubview(&line);
        }
        *mine.tiles_for.borrow_mut() = theirs.tiles_for.borrow_mut().take();
        // The rows move from one sidebar to the other: held, still, once.
        *mine.rows.borrow_mut() = theirs.rows.take();
        *mine.folder_rows.borrow_mut() = theirs.folder_rows.take();
        *mine.header.borrow_mut() = theirs.header.borrow_mut().take();
        *mine.rule.borrow_mut() = theirs.rule.borrow_mut().take();
        *mine.new_tab.borrow_mut() = theirs.new_tab.borrow_mut().take();
        mine.shows.set(theirs.shows.get());
        theirs.showing.set(false);
        mine.showing.set(true);
        // Nothing is pointed at in a window just brought to the front.
        for row in mine.rows.borrow().values() {
            if row.ivars().hover.get() {
                row.set_hover(false);
            }
        }
    }

    /// The favorites as tiles, four to a line: made again only when they,
    /// or an icon of theirs, changed.
    fn show_favorites(&self, favorites: &[FavoriteInfo]) {
        let mtm = self.mtm();
        let ivars = self.ivars();
        let want: Vec<(String, String, usize)> = favorites
            .iter()
            .map(|f| {
                (
                    f.favorite.address.clone(),
                    f.favorite.title.clone(),
                    f.icon.as_ref().map_or(0, |i| Retained::as_ptr(i) as usize),
                )
            })
            .collect();
        if ivars.tiles_for.borrow().as_ref() == Some(&want) {
            return;
        }
        self.clear_favorites();
        let inner = WIDTH - 2.0 * SIDE;
        let tile = (inner - TILE_GAP * (TILES_PER_ROW as f64 - 1.0)) / TILES_PER_ROW as f64;
        for row in favorites.chunks(TILES_PER_ROW) {
            let tiles: Vec<Retained<FavoriteTile>> = row
                .iter()
                .map(|f| FavoriteTile::new(mtm, tile, f))
                .collect();
            let views: Vec<&NSView> = tiles.iter().map(|t| -> &NSView { t }).collect();
            let line = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
            line.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            line.setSpacing(TILE_GAP);
            line.setTranslatesAutoresizingMaskIntoConstraints(false);
            ivars.favorites.addArrangedSubview(&line);
        }
        *ivars.tiles_for.borrow_mut() = Some(want);
    }

    fn clear_favorites(&self) {
        let ivars = self.ivars();
        detach_all(&ivars.favorites);
        ivars.tiles_for.borrow_mut().take();
    }

    /// The current space's name, over its tabs: made once, its words
    /// changed when the space's are.
    fn header(&self, layout: &Layout) -> Retained<NSView> {
        let mtm = self.mtm();
        let space = layout.current_space();
        let heading = if space.icon.is_empty() {
            space.name.clone()
        } else {
            format!("{}  {}", space.icon, space.name)
        };
        let mut slot = self.ivars().header.borrow_mut();
        if let Some((header, name)) = &*slot {
            if name.stringValue().to_string() != heading {
                name.setStringValue(&NSString::from_str(&heading));
            }
            return header.clone();
        }
        let name = NSTextField::labelWithString(&NSString::from_str(&heading), mtm);
        name.setFont(Some(&NSFont::systemFontOfSize_weight(12.0, unsafe {
            objc2_app_kit::NSFontWeightSemibold
        })));
        name.setTextColor(Some(&NSColor::secondaryLabelColor()));
        name.setTranslatesAutoresizingMaskIntoConstraints(false);
        let header = NSView::new(mtm);
        header.setTranslatesAutoresizingMaskIntoConstraints(false);
        header.addSubview(&name);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            header
                .heightAnchor()
                .constraintEqualToConstant(HEADER_HEIGHT),
            name.leadingAnchor()
                .constraintEqualToAnchor_constant(&header.leadingAnchor(), ROW_PAD),
            name.centerYAnchor()
                .constraintEqualToAnchor(&header.centerYAnchor()),
        ]));
        *slot = Some((header.clone(), name));
        header
    }

    /// The rule between the pinned tabs and the rest, with room above and
    /// below it: made once.
    fn rule(&self) -> Retained<NSView> {
        let mtm = self.mtm();
        let mut slot = self.ivars().rule.borrow_mut();
        if let Some(around) = &*slot {
            return around.clone();
        }
        let rule = NSBox::new(mtm);
        rule.setBoxType(NSBoxType::Separator);
        rule.setTranslatesAutoresizingMaskIntoConstraints(false);
        let around = NSView::new(mtm);
        around.setTranslatesAutoresizingMaskIntoConstraints(false);
        around.addSubview(&rule);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            around
                .heightAnchor()
                .constraintEqualToConstant(2.0 * SECTION_GAP),
            rule.leadingAnchor()
                .constraintEqualToAnchor_constant(&around.leadingAnchor(), ROW_PAD),
            rule.trailingAnchor()
                .constraintEqualToAnchor_constant(&around.trailingAnchor(), -ROW_PAD),
            rule.centerYAnchor()
                .constraintEqualToAnchor(&around.centerYAnchor()),
        ]));
        *slot = Some(around.clone());
        around
    }

    /// New Tab, the last row: made once.
    fn new_tab_row(&self) -> Retained<TabRow> {
        let mut slot = self.ivars().new_tab.borrow_mut();
        if let Some(row) = &*slot {
            return row.clone();
        }
        let width = WIDTH - 2.0 * (SIDE - 2.0);
        let row = TabRow::new(self.mtm(), width, None, None);
        *slot = Some(row.clone());
        row
    }

    /// `folder`'s row: made again only when its name or its fold changed.
    fn folder_row(&self, folder: &Folder) -> Retained<FolderRow> {
        let ivars = self.ivars();
        let mut rows = ivars.folder_rows.borrow_mut();
        if let Some((row, name, collapsed)) = rows.get(&folder.id) {
            if *name == folder.name && *collapsed == folder.collapsed {
                return row.clone();
            }
            detach(&ivars.list, row);
        }
        let width = WIDTH - 2.0 * (SIDE - 2.0);
        let row = FolderRow::new(self.mtm(), width, folder);
        rows.insert(
            folder.id,
            (row.clone(), folder.name.clone(), folder.collapsed),
        );
        row
    }
}
