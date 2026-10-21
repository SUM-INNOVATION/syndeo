//! Tabs, as macOS draws them, with what a browser adds to them.
//!
//! Each tab is a window of its own, tabbed with the others by AppKit; this
//! adds, beside each tab's title, its badge: a pin for a pinned tab, a
//! spinner while its page loads, and its page's icon. The icon is the one
//! the page names (`<link rel="icon">`), fetched by the page's own web view,
//! so through Syndeo's proxy and pinned as everything else the page loads,
//! and drawn there at 32 by 32; nothing outside the web view fetches
//! anything. A page that names no icon has none: Syndeo does not guess at
//! `/favicon.ico`, so it asks no server for anything the page did not.
//!
//! A tab's context menu is Syndeo's own: AppKit gives an application no way
//! to add to the one it draws, so a secondary click on a tab, and no other
//! click anywhere, is answered here instead, with Close Tab, Close Other
//! Tabs and Move Tab to New Window as AppKit has them, and Duplicate, Pin
//! and Show All Tabs.
//!
//! The session (`tabs::Session`) is kept in the application's own folder,
//! readable by this user alone, written whole or not at all.

use crate::tabs::Session;
use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, AnyThread, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAccessibility, NSControlSize, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSImage, NSImageView, NSMenu, NSMenuItem, NSProgressIndicator, NSProgressIndicatorStyle,
    NSStackView, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
};
use objc2_foundation::{
    NSArray, NSData, NSDataBase64DecodingOptions, NSError, NSPoint, NSSize, NSString,
    NSUserDefaults,
};
use objc2_web_kit::{WKContentWorld, WKWebView};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

/// A tab's badge, beside its title in the tab bar.
pub struct Badge {
    view: Retained<NSStackView>,
    pin: Retained<NSImageView>,
    spinner: Retained<NSProgressIndicator>,
    icon: Retained<NSImageView>,
}

/// The size an icon is shown at, in points.
const ICON: f64 = 16.0;

impl Badge {
    /// A badge with nothing showing yet, given to `window`'s tab.
    pub fn new(mtm: MainThreadMarker, window: &NSWindow) -> Self {
        let pin = NSImageView::new(mtm);
        if let Some(image) = symbol("pin.fill", "Pinned") {
            pin.setImage(Some(&image));
        }
        let spinner = NSProgressIndicator::new(mtm);
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        spinner.setDisplayedWhenStopped(false);
        let icon = NSImageView::new(mtm);
        icon.setAccessibilityLabel(Some(&NSString::from_str("Page icon")));
        let views: [&NSView; 3] = [&pin, &spinner, &icon];
        let view = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
        view.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        view.setSpacing(2.0);
        // What Accessibility, and the UI test, find them by.
        for (part, name) in [
            (&*pin as &NSView, "syndeo.tab-pinned"),
            (&*spinner, "syndeo.tab-loading"),
            (&*icon, "syndeo.tab-icon"),
        ] {
            part.setAccessibilityIdentifier(Some(&NSString::from_str(name)));
            part.setHidden(true);
        }
        window.tab().setAccessoryView(Some(&view));
        Self {
            view,
            pin,
            spinner,
            icon,
        }
    }

    pub fn set_pinned(&self, pinned: bool) {
        self.pin.setHidden(!pinned);
    }

    /// The spinner while the page loads, and its icon, if it has one, once
    /// it has.
    pub fn set_loading(&self, loading: bool) {
        self.spinner.setHidden(!loading);
        unsafe {
            if loading {
                self.spinner.startAnimation(None);
            } else {
                self.spinner.stopAnimation(None);
            }
        }
        self.icon.setHidden(loading || self.icon.image().is_none());
    }

    /// The page's icon, or none.
    pub fn set_icon(&self, image: Option<&NSImage>) {
        self.icon.setImage(image);
        self.icon
            .setHidden(image.is_none() || !self.spinner.isHidden());
    }

    /// Let the tab go of it.
    pub fn detach(&self, window: &NSWindow) {
        window.tab().setAccessoryView(None);
        self.view.removeFromSuperview();
    }
}

/// An SF Symbol, described for VoiceOver and Accessibility by `label`.
fn symbol(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

/// The script that finds a page's icon, in a world of its own so the page's
/// scripts cannot see or change it: the icon the page names, fetched by the
/// page's web view, drawn at 32 by 32 and handed back as a PNG data address,
/// or null. Nothing is fetched for a page that names no icon.
const ICON_SCRIPT: &str = r#"
const link = Array.from(document.querySelectorAll("link[rel][href]")).find(
  (l) => l.rel.toLowerCase().split(/\s+/).includes("icon"));
if (!link || !/^(https?|data):/i.test(link.href)) return null;
let blob;
try {
  const answer = await fetch(link.href);
  if (!answer.ok) return null;
  blob = await answer.blob();
} catch (e) {
  return null;
}
if (blob.size > 262144) return null;
const url = URL.createObjectURL(blob);
try {
  const image = new Image();
  image.src = url;
  await image.decode();
  const canvas = document.createElement("canvas");
  canvas.width = 32;
  canvas.height = 32;
  canvas.getContext("2d").drawImage(image, 0, 0, 32, 32);
  return canvas.toDataURL("image/png");
} catch (e) {
  return null;
} finally {
  URL.revokeObjectURL(url);
}
"#;

/// The most a page's icon may be, as a data address: a 32 by 32 PNG is a
/// few kilobytes.
const ICON_TEXT: usize = 16 * 1024;

/// What a PNG starts with.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Find `view`'s page's icon, and hand it to `done`: an image, or `None`
/// for a page with none, or one that could not be had.
pub fn fetch_icon(
    mtm: MainThreadMarker,
    view: &WKWebView,
    done: impl Fn(Option<Retained<NSImage>>) + 'static,
) {
    let world = unsafe { WKContentWorld::worldWithName(&NSString::from_str("syndeo-icons"), mtm) };
    let handler = block2::RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
        let text = unsafe { result.as_ref() }
            .and_then(|object| object.downcast_ref::<NSString>())
            .map(|text| text.to_string());
        done(text.as_deref().and_then(icon_from));
    });
    unsafe {
        view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
            &NSString::from_str(ICON_SCRIPT),
            None,
            None,
            &world,
            Some(&handler),
        )
    };
}

/// An image from a PNG data address the icon script made, if it is one and
/// no larger than an icon is.
fn icon_from(text: &str) -> Option<Retained<NSImage>> {
    let encoded = text.strip_prefix("data:image/png;base64,")?;
    if text.len() > ICON_TEXT {
        return None;
    }
    let data = NSData::initWithBase64EncodedString_options(
        NSData::alloc(),
        &NSString::from_str(encoded),
        NSDataBase64DecodingOptions::empty(),
    )?;
    if !data.to_vec().starts_with(PNG) {
        return None;
    }
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    image.setSize(NSSize::new(ICON, ICON));
    Some(image)
}

/// The windows tabbed with `window`, itself included, in the order their
/// tabs are in; just `window` if it has no tabs.
pub fn tabs_of(window: &NSWindow) -> Vec<Retained<NSWindow>> {
    let tabbed: Vec<Retained<NSWindow>> = window
        .tabGroup()
        .map(|group| group.windows().to_vec())
        .unwrap_or_default();
    if tabbed.iter().any(|w| std::ptr::eq(&**w, window)) {
        tabbed
    } else {
        vec![window.retain()]
    }
}

/// Where `window`'s tab is among its window's tabs.
pub fn index_of(window: &NSWindow) -> usize {
    tabs_of(window)
        .iter()
        .position(|w| std::ptr::eq(&**w, window))
        .unwrap_or(0)
}

/// Put `window`, a new tab or one of `beside`'s, at `index` among
/// `beside`'s window's tabs, counted without it.
pub fn place(beside: &NSWindow, window: &NSWindow, index: usize) {
    if let Some(group) = window.tabGroup() {
        if group.windows().iter().any(|w| std::ptr::eq(&*w, beside))
            && !std::ptr::eq(beside, window)
        {
            group.removeWindow(window);
        }
    }
    let others: Vec<Retained<NSWindow>> = tabs_of(beside)
        .into_iter()
        .filter(|w| !std::ptr::eq(&**w, window))
        .collect();
    match index.checked_sub(1).and_then(|before| others.get(before)) {
        Some(before) => {
            before.addTabbedWindow_ordered(window, objc2_app_kit::NSWindowOrderingMode::Above)
        }
        None => {
            match others.first() {
                Some(first) => first
                    .addTabbedWindow_ordered(window, objc2_app_kit::NSWindowOrderingMode::Below),
                None => beside
                    .addTabbedWindow_ordered(window, objc2_app_kit::NSWindowOrderingMode::Above),
            }
        }
    }
}

/// The accessibility role of an element AppKit hands back, if it has one.
fn role_of(element: &AnyObject) -> Option<String> {
    let responds: bool =
        unsafe { msg_send![element, respondsToSelector: objc2::sel!(accessibilityRole)] };
    if !responds {
        return None;
    }
    let role: Option<Retained<NSString>> = unsafe { msg_send![element, accessibilityRole] };
    role.map(|r| r.to_string())
}

fn parent_of(element: &AnyObject) -> Option<Retained<AnyObject>> {
    let responds: bool =
        unsafe { msg_send![element, respondsToSelector: objc2::sel!(accessibilityParent)] };
    if !responds {
        return None;
    }
    unsafe { msg_send![element, accessibilityParent] }
}

fn children_of(element: &AnyObject) -> Vec<Retained<AnyObject>> {
    let responds: bool =
        unsafe { msg_send![element, respondsToSelector: objc2::sel!(accessibilityChildren)] };
    if !responds {
        return Vec::new();
    }
    let children: Option<Retained<NSArray<AnyObject>>> =
        unsafe { msg_send![element, accessibilityChildren] };
    children.map(|c| c.to_vec()).unwrap_or_default()
}

/// Which of `window`'s tabs is at `point`, in the window's coordinates: the
/// tab bar's button there, found as Accessibility finds it, by its place
/// among the bar's buttons. Nothing in the window's content is ever a tab,
/// whatever a page calls its own parts.
pub fn tab_at(window: &NSWindow, point: NSPoint) -> Option<usize> {
    let content = window.contentLayoutRect();
    if point.y < content.origin.y + content.size.height {
        return None;
    }
    let screen = window.convertPointToScreen(point);
    let found: Option<Retained<AnyObject>> =
        unsafe { msg_send![window, accessibilityHitTest: screen] };
    let mut element = found?;
    for _ in 0..8 {
        if role_of(&element).as_deref() == Some("AXRadioButton") {
            let bar = parent_of(&element)?;
            if role_of(&bar).as_deref() != Some("AXTabGroup") {
                return None;
            }
            return children_of(&bar)
                .iter()
                .filter(|child| role_of(child).as_deref() == Some("AXRadioButton"))
                .position(|child| std::ptr::eq(&**child, &*element));
        }
        element = parent_of(&element)?;
    }
    None
}

/// Answer a secondary click on a tab, and nothing else, with `menu_for`'s
/// menu for that tab's window. Returns the monitor, which has to be kept.
pub fn watch_tab_clicks(
    mtm: MainThreadMarker,
    menu_for: impl Fn(&NSWindow) -> Option<Retained<NSMenu>> + 'static,
) -> Option<Retained<AnyObject>> {
    let handler = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let seen = unsafe { event.as_ref() };
        let secondary = seen.r#type() == NSEventType::RightMouseDown
            || (seen.r#type() == NSEventType::LeftMouseDown
                && seen.modifierFlags().contains(NSEventModifierFlags::Control));
        if !secondary {
            return event.as_ptr();
        }
        let Some(window) = seen.window(mtm) else {
            return event.as_ptr();
        };
        let Some(index) = tab_at(&window, seen.locationInWindow()) else {
            return event.as_ptr();
        };
        let Some(tab) = tabs_of(&window).get(index).cloned() else {
            return event.as_ptr();
        };
        let (Some(menu), Some(view)) = (menu_for(&tab), window.contentView()) else {
            return event.as_ptr();
        };
        NSMenu::popUpContextMenu_withEvent_forView(&menu, seen, &view);
        std::ptr::null_mut()
    });
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::RightMouseDown | NSEventMask::LeftMouseDown,
            &handler,
        )
    }
}

/// A menu item sending `action` to `target`, or along the responder chain.
pub fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    target: Option<&AnyObject>,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::new(),
        )
    };
    unsafe { item.setTarget(target) };
    item
}

/// Whether macOS is set to keep an application's windows when it quits:
/// System Settings > Desktop & Dock > "Close windows when quitting an
/// application", off. Syndeo then opens its tabs again as they were.
pub fn keeps_windows() -> bool {
    NSUserDefaults::standardUserDefaults()
        .boolForKey(&NSString::from_str("NSQuitAlwaysKeepsWindows"))
}

/// Where the session is kept, under the application's folder.
pub fn session_file(home: &Path) -> PathBuf {
    home.join("session.txt")
}

/// The session kept when Syndeo last quit, or none.
pub fn read_session(home: &Path) -> Session {
    std::fs::read_to_string(session_file(home))
        .map(|text| Session::from_text(&text))
        .unwrap_or_default()
}

/// Keep `session`, readable by this user alone, written to a file of its
/// own and then put in place, so the one there is whole, old or new; or,
/// for a session with nothing in it, keep none.
pub fn write_session(home: &Path, session: &Session) -> Result<()> {
    let path = session_file(home);
    if session.is_empty() {
        return forget_session(home);
    }
    let partial = home.join("session.txt.partial");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(&partial);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .with_context(|| format!("creating {}", partial.display()))?;
        file.write_all(session.to_text().as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("writing {}", partial.display()))?;
    }
    std::fs::rename(&partial, &path).with_context(|| format!("replacing {}", path.display()))
}

/// Keep no session.
pub fn forget_session(home: &Path) -> Result<()> {
    for path in [session_file(home), home.join("session.txt.partial")] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("removing {}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::{SavedTab, SavedWindow};
    use std::os::unix::fs::PermissionsExt;

    fn session() -> Session {
        Session {
            windows: vec![SavedWindow {
                tabs: vec![SavedTab {
                    address: "https://example.com/".into(),
                    title: "Example Domain".into(),
                    pinned: true,
                }],
                selected: 0,
            }],
        }
    }

    #[test]
    fn the_session_is_kept_for_this_user_alone_and_read_back() {
        let home = tempfile::tempdir().unwrap();
        assert!(read_session(home.path()).is_empty());
        write_session(home.path(), &session()).unwrap();
        let mode = std::fs::metadata(session_file(home.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_session(home.path()), session());
        assert!(!home.path().join("session.txt.partial").exists());
    }

    #[test]
    fn an_empty_session_or_forgetting_it_leaves_no_file() {
        let home = tempfile::tempdir().unwrap();
        write_session(home.path(), &session()).unwrap();
        write_session(home.path(), &Session::default()).unwrap();
        assert!(!session_file(home.path()).exists());
        write_session(home.path(), &session()).unwrap();
        forget_session(home.path()).unwrap();
        assert!(!session_file(home.path()).exists());
        forget_session(home.path()).unwrap();
    }

    #[test]
    fn a_partial_file_left_by_a_crash_does_not_stop_the_next_write() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("session.txt.partial"), "half").unwrap();
        write_session(home.path(), &session()).unwrap();
        assert_eq!(read_session(home.path()), session());
    }

    #[test]
    fn a_png_from_the_icon_script_is_an_icon_shown_at_sixteen_points() {
        let one_pixel = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let image = icon_from(one_pixel).expect("a PNG is an icon");
        let size = image.size();
        assert_eq!((size.width, size.height), (ICON, ICON));
    }

    #[test]
    fn only_a_small_png_is_an_icon() {
        assert!(icon_from("data:image/gif;base64,R0lGODlhAQABAAAAACw=").is_none());
        assert!(
            icon_from("data:image/png;base64,bm90IGEgcG5n").is_none(),
            "not a PNG"
        );
        let big = format!("data:image/png;base64,{}", "A".repeat(ICON_TEXT));
        assert!(icon_from(&big).is_none());
        assert!(icon_from("https://example.com/icon.png").is_none());
    }
}
