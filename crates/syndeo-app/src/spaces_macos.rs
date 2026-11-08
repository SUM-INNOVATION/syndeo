//! Spaces, kept and switched: the layout (`spaces::Layout`) in the
//! application's defaults, and the switcher at the foot of the sidebar —
//! a dot for each space in its colour, the current one ringed, with New
//! Space and New Folder beside them.
//!
//! The dots and buttons send to the application, through the responder
//! chain: `syndeoSwitchSpace:` with the dot as the sender, `syndeoNewSpace:`
//! and `syndeoNewFolder:`; a dot's menu sends `syndeoRenameSpace:`,
//! `syndeoSetSpaceHue:` and `syndeoDeleteSpace:`, with the space (and the
//! colour) in the item's tag. What they do to the tabs is the application's
//! (`app_macos`).

use crate::spaces::{self, Layout};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::Message;
use objc2::{define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSBezierPath, NSButton, NSColor, NSEvent, NSFontWeightMedium,
    NSImage, NSImageSymbolConfiguration, NSLayoutConstraint, NSMenu, NSMenuItem, NSStackView,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{ns_string, NSArray, NSPoint, NSRect, NSSize, NSString, NSUserDefaults};
use std::cell::{Cell, RefCell};

/// The defaults key the layout is kept under, as `Layout::render`.
const LAYOUT_KEY: &str = "SyndeoSpaces";

/// A space's tile in the switcher: its size, and the gap between tiles.
pub const DOT: f64 = 26.0;
const DOT_GAP: f64 = 8.0;

/// The switcher's height.
pub const BAR_HEIGHT: f64 = 28.0;

/// A tag that says which space, and which colour: `space * HUE_BASE + hue`.
pub const HUE_BASE: isize = 1000;

thread_local! {
    /// The layout, once read.
    static LAYOUT: RefCell<Option<Layout>> = const { RefCell::new(None) };
}

fn load() -> Layout {
    NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str(LAYOUT_KEY))
        .map(|json| Layout::parse(&json.to_string()))
        .unwrap_or_default()
}

fn save(layout: &Layout) {
    let json = NSString::from_str(&layout.render());
    let value: &AnyObject = json.as_ref();
    unsafe {
        NSUserDefaults::standardUserDefaults()
            .setObject_forKey(Some(value), &NSString::from_str(LAYOUT_KEY))
    };
}

/// Read the layout.
pub fn read<R>(f: impl FnOnce(&Layout) -> R) -> R {
    LAYOUT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let layout = slot.get_or_insert_with(load);
        f(layout)
    })
}

/// Change the layout, and keep it.
pub fn change<R>(f: impl FnOnce(&mut Layout) -> R) -> R {
    LAYOUT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let layout = slot.get_or_insert_with(load);
        let result = f(layout);
        save(layout);
        result
    })
}

/// The current space's id.
pub fn current() -> u64 {
    read(|layout| layout.current)
}

/// Whether `space` is one of the spaces; the current one stands in for one
/// that is not.
pub fn space_or_current(space: u64) -> u64 {
    read(|layout| {
        if layout.has_space(space) {
            space
        } else {
            layout.current
        }
    })
}

fn color(rgb: [f64; 3]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(rgb[0], rgb[1], rgb[2], 1.0)
}

/// The current space's gradient, light or dark.
pub fn current_colors(dark: bool) -> (Retained<NSColor>, Retained<NSColor>) {
    let hue = read(|layout| layout.current_space().hue);
    let (from, to) = spaces::hue_colors(hue, dark);
    (color(from), color(to))
}

/// Send `action` along the responder chain, from `sender`.
fn send(mtm: MainThreadMarker, action: objc2::runtime::Sel, sender: Option<&AnyObject>) {
    let app = NSApplication::sharedApplication(mtm);
    unsafe { app.sendAction_to_from(action, None, sender) };
}

/// A menu item sending `action` along the responder chain, with `tag`.
fn tagged_item(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    tag: isize,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::new(),
        )
    };
    item.setTag(tag);
    item
}

// ---------------------------------------------------------------- a dot

pub struct DotIvars {
    id: u64,
    hue: usize,
    name: String,
    current: Cell<bool>,
}

define_class!(
    /// One space in the switcher: a dot in its colour, ringed when current.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoSpaceDot"]
    #[ivars = DotIvars]
    pub struct SpaceDot;

    impl SpaceDot {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            // The current space's emoji sits on a lit, rounded square; the
            // others on nothing.
            if !self.ivars().current.get() {
                return;
            }
            let dark = crate::sidebar_macos::is_dark(self);
            let alpha = if dark { 0.18 } else { 0.62 };
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, alpha).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 7.0, 7.0).fill();
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            let me: &AnyObject = self.as_ref();
            send(self.mtm(), sel!(syndeoSwitchSpace:), Some(me));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let mtm = self.mtm();
            let id = self.ivars().id as isize;
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Space"));
            menu.addItem(&tagged_item(
                mtm,
                "Rename or Change Emoji…",
                sel!(syndeoRenameSpace:),
                id,
            ));
            let colours = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Colour"));
            for (hue, name) in COLOUR_NAMES.iter().enumerate() {
                let item = tagged_item(
                    mtm,
                    name,
                    sel!(syndeoSetSpaceHue:),
                    id * HUE_BASE + hue as isize,
                );
                if hue == self.ivars().hue {
                    item.setState(objc2_app_kit::NSControlStateValueOn);
                }
                colours.addItem(&item);
            }
            let holder = NSMenuItem::new(mtm);
            holder.setTitle(ns_string!("Colour"));
            holder.setSubmenu(Some(&colours));
            menu.addItem(&holder);
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            menu.addItem(&tagged_item(mtm, "Delete Space", sel!(syndeoDeleteSpace:), id));
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str("AXRadioButton"))
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn accessibility_label(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().name))
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            let me: &AnyObject = self.as_ref();
            send(self.mtm(), sel!(syndeoSwitchSpace:), Some(me));
            true
        }
    }
);

pub struct PickerIvars {
    field: Retained<objc2_app_kit::NSTextField>,
}

define_class!(
    /// Pick…, beside the emoji field in the space dialog: the system's
    /// emoji palette, typing into that field.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoEmojiPicker"]
    #[ivars = PickerIvars]
    pub struct EmojiPicker;

    impl EmojiPicker {
        #[unsafe(method(syndeoPickEmoji:))]
        fn pick(&self, _sender: Option<&AnyObject>) {
            let field = &self.ivars().field;
            if let Some(window) = field.window() {
                let view: &NSView = field;
                window.makeFirstResponder(Some(view));
            }
            NSApplication::sharedApplication(self.mtm()).orderFrontCharacterPalette(None);
        }
    }
);

impl EmojiPicker {
    pub fn new(mtm: MainThreadMarker, field: &objc2_app_kit::NSTextField) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PickerIvars {
            field: field.retain(),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// The colours' names, in `spaces::hue_colors`' order.
const COLOUR_NAMES: [&str; spaces::HUES] = [
    "Lavender", "Sky", "Mint", "Peach", "Rose", "Sand", "Slate", "Lilac",
];

impl SpaceDot {
    fn new(mtm: MainThreadMarker, space: &spaces::Space, current: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DotIvars {
            id: space.id,
            hue: space.hue,
            name: space.name.clone(),
            current: Cell::new(current),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(DOT, DOT));
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            this.widthAnchor().constraintEqualToConstant(DOT),
            this.heightAnchor().constraintEqualToConstant(DOT),
        ]));
        this.setToolTip(Some(&NSString::from_str(&space.name)));
        this.setAccessibilityIdentifier(Some(ns_string!("syndeo.space")));
        // The emoji, dimmed a little when the space is not the current one.
        let label =
            objc2_app_kit::NSTextField::labelWithString(&NSString::from_str(&space.icon), mtm);
        label.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(15.0)));
        label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        label.setFrame(NSRect::new(
            NSPoint::new(0.0, (DOT - 20.0) / 2.0),
            NSSize::new(DOT, 20.0),
        ));
        label.setAlphaValue(if current { 1.0 } else { 0.62 });
        this.addSubview(&label);
        this
    }

    /// The space's id.
    pub fn id(&self) -> u64 {
        self.ivars().id
    }
}

// ------------------------------------------------------------ the switcher

pub struct BarIvars {
    dots: Retained<NSStackView>,
    /// Downloads, at the left: tinted while anything downloads.
    downloads: Retained<NSButton>,
}

define_class!(
    /// The switcher: the spaces' dots, then New Space and New Folder.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoSpaceBar"]
    #[ivars = BarIvars]
    pub struct SpaceBar;
);

impl SpaceBar {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let dots = NSStackView::new(mtm);
        dots.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        dots.setSpacing(DOT_GAP);
        dots.setTranslatesAutoresizingMaskIntoConstraints(false);
        let downloads = small_button(
            mtm,
            "arrow.down.circle",
            "Downloads",
            sel!(syndeoShowDownloads:),
        );
        let this = Self::alloc(mtm).set_ivars(BarIvars {
            dots: dots.clone(),
            downloads: downloads.clone(),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.addSubview(&downloads);
        this.addSubview(&dots);

        let new_space = small_button(mtm, "plus.circle", "New Space", sel!(syndeoNewSpace:));
        let new_folder = small_button(
            mtm,
            "folder.badge.plus",
            "New Folder",
            sel!(syndeoNewFolder:),
        );
        let buttons: [&NSView; 2] = [&new_space, &new_folder];
        let right = NSStackView::stackViewWithViews(&NSArray::from_slice(&buttons), mtm);
        right.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        right.setSpacing(2.0);
        right.setTranslatesAutoresizingMaskIntoConstraints(false);
        this.addSubview(&right);

        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            this.heightAnchor().constraintEqualToConstant(BAR_HEIGHT),
            downloads
                .leadingAnchor()
                .constraintEqualToAnchor(&this.leadingAnchor()),
            downloads
                .centerYAnchor()
                .constraintEqualToAnchor(&this.centerYAnchor()),
            dots.centerXAnchor()
                .constraintEqualToAnchor(&this.centerXAnchor()),
            dots.centerYAnchor()
                .constraintEqualToAnchor(&this.centerYAnchor()),
            right
                .trailingAnchor()
                .constraintEqualToAnchor(&this.trailingAnchor()),
            right
                .centerYAnchor()
                .constraintEqualToAnchor(&this.centerYAnchor()),
        ]));
        this
    }

    /// Show `layout`'s spaces, the current one ringed; and whether anything
    /// is downloading, in the downloads button's tint.
    pub fn reload(&self, layout: &Layout) {
        let mtm = self.mtm();
        let busy = crate::downloads_macos::receiving() > 0;
        let downloads = &self.ivars().downloads;
        let (symbol, tint) = if busy {
            ("arrow.down.circle.fill", NSColor::controlAccentColor())
        } else {
            ("arrow.down.circle", NSColor::secondaryLabelColor())
        };
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(ns_string!("Downloads")),
        ) {
            downloads.setImage(Some(&image));
        }
        downloads.setContentTintColor(Some(&tint));
        let dots = &self.ivars().dots;
        for view in dots.arrangedSubviews().iter() {
            dots.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        for space in &layout.spaces {
            let dot = SpaceDot::new(mtm, space, space.id == layout.current);
            dots.addArrangedSubview(&dot);
        }
    }
}

/// A small borderless symbol button sending `action` along the responder
/// chain.
fn small_button(
    mtm: MainThreadMarker,
    symbol: &str,
    label: &str,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(label)),
    )
    .unwrap_or_default();
    let button =
        unsafe { NSButton::buttonWithImage_target_action(&image, None, Some(action), mtm) };
    button.setBordered(false);
    button.setSymbolConfiguration(Some(
        &NSImageSymbolConfiguration::configurationWithPointSize_weight(13.0, unsafe {
            NSFontWeightMedium
        }),
    ));
    button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    button.setToolTip(Some(&NSString::from_str(label)));
    button.setTranslatesAutoresizingMaskIntoConstraints(false);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        button.widthAnchor().constraintEqualToConstant(24.0),
        button.heightAnchor().constraintEqualToConstant(24.0),
    ]));
    button
}
