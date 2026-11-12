//! Downloads: WebKit's, kept in a list (`downloads`) and shown from the
//! sidebar's foot.
//!
//! A response a window cannot show as a page, or a link marked for
//! download, becomes a `WKDownload` (`browser_macos` decides that), which
//! is handed here (`adopt`): it is given this module's delegate, a place in
//! `~/Downloads` under the name the server suggested, made safe and made
//! unique, and a row in the list. While any download is still coming in,
//! a timer follows its progress.
//!
//! The downloads button in the sidebar's switcher shows a popover with the
//! list: each download's name, how far it has got, Show in Finder once it
//! is done or Cancel while it is not; and Open Downloads Folder and Clear
//! for the list as a whole. The button is tinted while anything downloads,
//! and hidden in an agent's window, which never shows the person's
//! downloads. A download a page started by itself, after its one automatic download,
//! never begins, and the list shows it blocked (`blocked`), one row for the
//! page however often it tries.
//!
//! A download's name is the server's suggestion made safe
//! (`downloads::safe_name`: no path, no control, format or separator
//! characters, at most 255 bytes) and unique: nothing already at that path,
//! not even a link to nothing, and no download still coming in that is to
//! be saved there.
//!
//! Clear Browsing Data cancels every download still coming in, removes
//! what it had written, and empties the list (`forget_all`). Files already
//! saved in ~/Downloads stay: they are the person's, not browsing data.
//! Cancel does the same for one download.
//!
//! Downloaded files are quarantined, as a browser's are, through the
//! bundle's `LSFileQuarantineEnabled`.
//!
//! A download is the window's load, as its page is, and held to the same
//! rules:
//! - **TLS challenges** a download meets, on a connection of its own to the
//!   proxy or to a host sent directly, are answered exactly as its window
//!   answers its page's (`browser_macos::Trust`, kept with the download when
//!   it is adopted): a host sent directly gets macOS's own trust, every other
//!   is pinned to the proxy's authority. A download nothing is known of is
//!   refused.
//! - **Redirects** go only where the window's main frame may
//!   (`address::download_may_follow`): the web's own addresses, never an
//!   IPv6 address with a zone. Any other is refused, and the download with
//!   it, which the list says. (WebKit asks only of a redirect within the
//!   download's own origin; one elsewhere it makes a navigation of the
//!   window, which the window's navigation policy decides.)
//! - **An agent's tab downloads nothing.** Its window refuses both ways a
//!   download begins (`browser_macos`), and one handed here all the same is
//!   cancelled before it has a place to go.

use crate::address;
use crate::browser_macos::{self, BrowserController, Trust};
use crate::downloads::{self, List, State};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAccessibility, NSBezelStyle, NSButton, NSColor, NSControlSize, NSFont, NSLayoutAttribute,
    NSLayoutConstraint, NSLineBreakMode, NSPopover, NSPopoverBehavior, NSProgressIndicator,
    NSProgressIndicatorStyle, NSStackView, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
    NSViewController, NSWorkspace,
};
use objc2_foundation::{
    ns_string, NSArray, NSEdgeInsets, NSError, NSPoint, NSProgress, NSRect, NSRectEdge, NSSize,
    NSString, NSTimer, NSURLAuthenticationChallenge, NSURLRequest, NSURLResponse, NSURL,
};
use objc2_web_kit::{WKDownload, WKDownloadDelegate, WKDownloadRedirectPolicy};
use std::cell::RefCell;
use std::path::PathBuf;

/// The popover: its width, and its inset.
const WIDTH: f64 = 340.0;
const INSET: f64 = 12.0;

/// How often a download's progress is looked at while it comes in.
const TICK: f64 = 0.3;

/// `NSURLErrorCancelled`: a download stopped on purpose, not a failure.
const CANCELLED: isize = -999;

/// A download still coming in: its row's id, and how the window it began
/// in answers TLS challenges.
#[derive(Clone)]
struct Active {
    id: u64,
    download: Retained<WKDownload>,
    trust: Trust,
}

thread_local! {
    static LIST: RefCell<List> = RefCell::new(List::default());
    /// The downloads still coming in.
    static ACTIVE: RefCell<Vec<Active>> = const { RefCell::new(Vec::new()) };
    static KEEPER: RefCell<Option<Retained<Keeper>>> = const { RefCell::new(None) };
    static POPOVER: RefCell<Option<Retained<NSPopover>>> = const { RefCell::new(None) };
    static TICKING: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

/// Take a download WebKit has begun in `from`'s window: give it its
/// delegate, its row, and the window's way of answering TLS challenges. In
/// an agent's tab, which neither of its window's policies lets download,
/// cancel it instead, before it has anywhere to go.
pub fn adopt(mtm: MainThreadMarker, download: &WKDownload, from: &BrowserController) {
    let keeper = KEEPER.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| Keeper::new(mtm))
            .clone()
    });
    // This module's delegate either way: one it keeps no record of is given
    // no place to go, and its challenges and redirects are refused.
    unsafe { download.setDelegate(Some(ProtocolObject::from_ref(&*keeper))) };
    if from.refuses_downloads() {
        unsafe { download.cancel(None) };
        tracing::warn!("a download began in an agent's tab: cancelled");
        return;
    }
    let id = LIST.with(|list| list.borrow_mut().begin());
    ACTIVE.with(|active| {
        active.borrow_mut().push(Active {
            id,
            download: download.retain(),
            trust: from.trust(),
        })
    });
    tracing::info!(download = id, "a download begun");
    tick_while_receiving(mtm);
    changed(mtm);
}

/// A download a page started by itself was refused, after its one
/// automatic download: the page's blocked row, `row`, if it is still in the
/// list, or a new one named `name`. The row's id.
pub fn blocked(mtm: MainThreadMarker, name: &str, row: Option<u64>) -> u64 {
    let (id, new) = LIST.with(|list| {
        let mut list = list.borrow_mut();
        match row.filter(|&id| list.is_blocked(id)) {
            Some(id) => (id, false),
            None => (list.block(name), true),
        }
    });
    if new {
        changed(mtm);
    }
    id
}

/// Clear Browsing Data: every download still coming in is cancelled, as
/// every window's loading is stopped, and what it had written removed; and
/// the list is emptied, of what finished, failed, was blocked or refused,
/// as well. Files already saved in ~/Downloads stay: they are the person's
/// now, not browsing data, and nothing here deletes one.
pub fn forget_all(mtm: MainThreadMarker) {
    let active = ACTIVE.with(|active| std::mem::take(&mut *active.borrow_mut()));
    for download in &active {
        cancel(&download.download, path_of(download.id));
    }
    let listed = LIST.with(|list| {
        let mut list = list.borrow_mut();
        let listed = list.items().len();
        list.clear_all();
        listed
    });
    tracing::info!(
        cancelled = active.len(),
        listed,
        "downloads cleared: those coming in cancelled, and the list emptied"
    );
    changed(mtm);
}

/// Where download `id` is being saved, once that is decided.
fn path_of(id: u64) -> Option<PathBuf> {
    LIST.with(|list| list.borrow().get(id).and_then(|d| d.path.clone()))
}

/// Cancel a download still coming in, and once WebKit has stopped it,
/// remove what it had written at `path`, where this module placed it: a
/// file of its own, never one that was there before (`unique_path`). Only a
/// plain file: never a folder or a link.
fn cancel(download: &WKDownload, path: Option<PathBuf>) {
    let stopped = block2::RcBlock::new(move |_resume: *mut objc2_foundation::NSData| {
        let Some(path) = &path else { return };
        let partial = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file());
        if partial {
            match std::fs::remove_file(path) {
                Ok(()) => tracing::info!("a cancelled download's partial file removed"),
                Err(err) => {
                    tracing::warn!("a cancelled download's partial file stayed: {}", err.kind())
                }
            }
        }
    });
    unsafe { download.cancel(Some(&stopped)) };
}

/// How many downloads are still coming in.
pub fn receiving() -> usize {
    LIST.with(|list| list.borrow().receiving())
}

/// Where downloads go: `~/Downloads`, made if it is not there.
fn folder() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = home.join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// What this module keeps of `download`, if it adopted it and it is still
/// coming in.
fn active(download: &WKDownload) -> Option<Active> {
    ACTIVE.with(|active| {
        active
            .borrow()
            .iter()
            .find(|a| std::ptr::eq(&*a.download, download))
            .cloned()
    })
}

fn id_of(download: &WKDownload) -> Option<u64> {
    active(download).map(|a| a.id)
}

fn forget_active(id: u64) {
    ACTIVE.with(|active| active.borrow_mut().retain(|a| a.id != id));
}

/// Something about the downloads changed: the popover, if it shows, and
/// the sidebars' buttons.
fn changed(mtm: MainThreadMarker) {
    rebuild_popover(mtm);
    if let Some(window) = objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow() {
        crate::browser_macos::refresh_sidebars(&window);
    }
}

/// Follow every active download's progress, while there is one.
fn tick_while_receiving(mtm: MainThreadMarker) {
    let running = TICKING.with(|slot| slot.borrow().is_some());
    if running {
        return;
    }
    let tick = block2::RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| {
        let active: Vec<(u64, Retained<WKDownload>)> = ACTIVE.with(|active| {
            active
                .borrow()
                .iter()
                .map(|a| (a.id, a.download.clone()))
                .collect()
        });
        if active.is_empty() {
            unsafe { timer.as_ref() }.invalidate();
            TICKING.with(|slot| *slot.borrow_mut() = None);
            return;
        }
        for (id, download) in &active {
            let progress: Option<Retained<NSProgress>> =
                unsafe { msg_send![&**download, progress] };
            if let Some(progress) = progress {
                let done = u64::try_from(progress.completedUnitCount()).unwrap_or(0);
                let total = u64::try_from(progress.totalUnitCount())
                    .ok()
                    .filter(|t| *t > 0);
                LIST.with(|list| list.borrow_mut().progress(*id, done, total));
            }
        }
        rebuild_popover(mtm);
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(TICK, true, &tick) };
    TICKING.with(|slot| *slot.borrow_mut() = Some(timer));
}

// ------------------------------------------------------------- the keeper

define_class!(
    /// Every download's delegate, and what the popover's buttons send to.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoDownloads"]
    pub struct Keeper;

    unsafe impl NSObjectProtocol for Keeper {}

    unsafe impl WKDownloadDelegate for Keeper {
        /// Where the file goes: `~/Downloads`, under the suggested name made
        /// safe, numbered if the name is taken.
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        unsafe fn decide_destination(
            &self,
            download: &WKDownload,
            response: &NSURLResponse,
            suggested: &NSString,
            completion: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            // Only a download adopted, and still coming in, has a place:
            // one cancelled in an agent's tab, or one never handed here,
            // goes nowhere.
            let Some(id) = id_of(download) else {
                tracing::warn!("a download nothing is known of was given no place");
                completion.call((std::ptr::null_mut(),));
                return;
            };
            let dir = folder();
            let name = downloads::safe_name(&suggested.to_string());
            let path = LIST.with(|list| {
                let list = list.borrow();
                downloads::unique_path(&dir, &name, |p| downloads::taken(p) || list.reserved(p))
            });
            let total = u64::try_from(response.expectedContentLength())
                .ok()
                .filter(|t| *t > 0);
            LIST.with(|list| list.borrow_mut().place(id, path.clone(), total));
            tracing::info!(download = id, "a download placed");
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            completion.call((Retained::as_ptr(&url).cast_mut(),));
            changed(self.mtm());
        }

        /// A TLS challenge the download meets on a connection of its own,
        /// answered exactly as its window answers its page's: a host sent
        /// directly gets macOS's own trust, every other is pinned to the
        /// proxy's authority. Nothing known of the download: refused.
        #[unsafe(method(download:didReceiveAuthenticationChallenge:completionHandler:))]
        unsafe fn did_receive_challenge(
            &self,
            download: &WKDownload,
            challenge: &NSURLAuthenticationChallenge,
            completion: &block2::DynBlock<dyn Fn(isize, *mut AnyObject)>,
        ) {
            let known = active(download);
            let id = known.as_ref().map(|a| a.id);
            let trust = known.map_or_else(Trust::none, |a| a.trust);
            let judged = trust.judge(challenge, browser_macos::host_of(challenge).as_deref());
            let answer = (judged.disposition, judged.credential);
            // How it was answered, and nothing of the host.
            tracing::info!(download = ?id, answer = answered(answer.0), "a download's TLS challenge answered");
            completion.call(answer);
        }

        /// A redirect goes only where the window's main frame may go; any
        /// other is refused, and the download with it. WebKit asks this
        /// only of a redirect to the download's own origin, measured on
        /// macOS 15 (CI 42541397760): one to another origin, from a link
        /// marked for downloading, it turns into a navigation of the
        /// window, which its navigation policy decides. This is the rule
        /// should WebKit ever ask of another.
        #[unsafe(method(download:willPerformHTTPRedirection:newRequest:decisionHandler:))]
        unsafe fn will_redirect(
            &self,
            download: &WKDownload,
            _response: &AnyObject,
            request: &NSURLRequest,
            decision: &block2::DynBlock<dyn Fn(WKDownloadRedirectPolicy)>,
        ) {
            let id = id_of(download);
            let scheme = browser_macos::scheme_of(request);
            let target = browser_macos::address_of(request);
            let allowed = id.is_some() && address::download_may_follow(&scheme, &target);
            if allowed {
                tracing::info!(download = ?id, scheme = %scheme, "a download's redirect allowed");
            } else {
                if let Some(id) = id {
                    LIST.with(|list| list.borrow_mut().fail(id, downloads::REDIRECT_REFUSED));
                    forget_active(id);
                }
                // Its scheme, and nothing else of where it was sent.
                tracing::info!(download = ?id, scheme = %scheme, "refused a download's redirect");
            }
            decision.call((if allowed {
                WKDownloadRedirectPolicy::Allow
            } else {
                WKDownloadRedirectPolicy::Cancel
            },));
            if !allowed {
                changed(self.mtm());
            }
        }

        #[unsafe(method(downloadDidFinish:))]
        unsafe fn did_finish(&self, download: &WKDownload) {
            if let Some(id) = id_of(download) {
                LIST.with(|list| list.borrow_mut().finish(id));
                forget_active(id);
                tracing::info!(download = id, "a download finished");
            }
            changed(self.mtm());
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        unsafe fn did_fail(
            &self,
            download: &WKDownload,
            error: &NSError,
            _resume_data: Option<&objc2_foundation::NSData>,
        ) {
            if let Some(id) = id_of(download) {
                if error.code() == CANCELLED {
                    // Stopped by WebKit, not by Cancel, which forgets it
                    // first: a TLS challenge refused, say.
                    LIST.with(|list| list.borrow_mut().cancel(id));
                    tracing::info!(download = id, "a download stopped before it finished");
                } else {
                    let why = error.localizedDescription().to_string();
                    LIST.with(|list| list.borrow_mut().fail(id, &why));
                    tracing::warn!(download = id, "a download failed: {why}");
                }
                forget_active(id);
            }
            changed(self.mtm());
        }
    }

    impl Keeper {
        /// Show in Finder: the button's tag is the download.
        #[unsafe(method(syndeoRevealDownload:))]
        fn reveal(&self, sender: Option<&AnyObject>) {
            let Some(id) = tag_of(sender) else { return };
            let path = LIST.with(|list| list.borrow().get(id).and_then(|d| d.path.clone()));
            if let Some(path) = path {
                let shown = NSWorkspace::sharedWorkspace()
                    .selectFile_inFileViewerRootedAtPath(
                        Some(&NSString::from_str(&path.to_string_lossy())),
                        ns_string!(""),
                    );
                tracing::info!(shown, "a download shown in Finder");
            }
        }

        /// Cancel: the button's tag is the download.
        #[unsafe(method(syndeoCancelDownload:))]
        fn cancel(&self, sender: Option<&AnyObject>) {
            let Some(id) = tag_of(sender) else { return };
            let download = ACTIVE.with(|active| {
                active
                    .borrow()
                    .iter()
                    .find(|a| a.id == id)
                    .map(|a| a.download.clone())
            });
            if let Some(download) = download {
                cancel(&download, path_of(id));
            }
            LIST.with(|list| list.borrow_mut().cancel(id));
            forget_active(id);
            tracing::info!(download = id, "a download cancelled");
            changed(self.mtm());
        }

        #[unsafe(method(syndeoOpenDownloadsFolder:))]
        fn open_folder(&self, _sender: Option<&AnyObject>) {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&folder().to_string_lossy()));
            NSWorkspace::sharedWorkspace().openURL(&url);
        }

        #[unsafe(method(syndeoClearDownloads:))]
        fn clear(&self, _sender: Option<&AnyObject>) {
            LIST.with(|list| list.borrow_mut().clear_finished());
            changed(self.mtm());
        }
    }
);

impl Keeper {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// A TLS challenge's answer, `NSURLSessionAuthChallengeDisposition`, in
/// words for the log.
fn answered(disposition: isize) -> &'static str {
    match disposition {
        0 => "vouched for, by Syndeo's authority",
        1 => "left to macOS",
        _ => "refused",
    }
}

/// A control's tag, as a download's id.
fn tag_of(sender: Option<&AnyObject>) -> Option<u64> {
    let control = sender?.downcast_ref::<objc2_app_kit::NSControl>()?;
    u64::try_from(control.tag()).ok()
}

// ------------------------------------------------------------ the popover

/// Show the downloads, in a popover from `from`; but never in an agent's
/// window, whose sidebar has no downloads button: they are the person's
/// (`sidebar_model::shows`).
pub fn show(mtm: MainThreadMarker, from: &NSView) {
    let owner = from
        .window()
        .map(|window| crate::control_macos::owner_of(&window));
    if owner.is_some_and(|owner| !crate::sidebar_model::shows(owner).downloads) {
        return;
    }
    let popover = POPOVER.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                let popover = NSPopover::new(mtm);
                popover.setBehavior(NSPopoverBehavior::Transient);
                let controller = NSViewController::new(mtm);
                popover.setContentViewController(Some(&controller));
                popover
            })
            .clone()
    });
    if popover.isShown() {
        popover.close();
        return;
    }
    rebuild_popover(mtm);
    popover.showRelativeToRect_ofView_preferredEdge(from.bounds(), from, NSRectEdge::MaxY);
}

/// The popover's content, as the list is now, if the popover exists.
fn rebuild_popover(mtm: MainThreadMarker) {
    let popover = POPOVER.with(|slot| slot.borrow().clone());
    let Some(popover) = popover else { return };
    let Some(controller) = popover.contentViewController() else {
        return;
    };
    let view = build(mtm);
    let size = view.frame().size;
    controller.setView(&view);
    popover.setContentSize(size);
}

/// A small bordered button sending `action` to the keeper, tagged `tag`.
fn button(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    tag: Option<u64>,
) -> Retained<NSButton> {
    let keeper = KEEPER.with(|slot| slot.borrow().clone());
    let target: Option<&AnyObject> = keeper.as_deref().map(|k| k.as_ref());
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            target,
            Some(action),
            mtm,
        )
    };
    button.setBezelStyle(NSBezelStyle::Push);
    button.setControlSize(NSControlSize::Small);
    button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    if let Some(tag) = tag {
        button.setTag(tag as isize);
    }
    button
}

fn label(mtm: MainThreadMarker, text: &str, size: f64, secondary: bool) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(size)));
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    if secondary {
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    label
}

/// The list, as a view sized to fit: a head with the title and the two
/// buttons for the whole list, then a row for each download — its name,
/// then what has happened to it with its button at the end, then a bar
/// while it comes in — with a rule between rows. Every label is held to
/// the popover's width, so a long name is cut short, not the popover
/// widened.
fn build(mtm: MainThreadMarker) -> Retained<NSView> {
    // Make sure the keeper exists, so the buttons have a target.
    KEEPER.with(|slot| {
        slot.borrow_mut().get_or_insert_with(|| Keeper::new(mtm));
    });
    let inner = WIDTH - 2.0 * INSET;
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(8.0);
    stack.setEdgeInsets(NSEdgeInsets {
        top: INSET,
        left: INSET,
        bottom: INSET,
        right: INSET,
    });
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);

    // A row across the popover: `left` takes the room, `right` sits at the
    // end.
    let across = |left: &NSView, right: &[&NSView]| -> Retained<NSStackView> {
        let line = NSStackView::new(mtm);
        line.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        line.setSpacing(8.0);
        line.setTranslatesAutoresizingMaskIntoConstraints(false);
        line.addArrangedSubview(left);
        let spring = NSView::new(mtm);
        spring.setTranslatesAutoresizingMaskIntoConstraints(false);
        spring.setContentHuggingPriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        line.addArrangedSubview(&spring);
        for view in right {
            line.addArrangedSubview(view);
        }
        line.widthAnchor()
            .constraintEqualToConstant(inner)
            .setActive(true);
        left.widthAnchor()
            .constraintLessThanOrEqualToConstant(inner * 0.62)
            .setActive(true);
        line
    };

    let title = label(mtm, "Downloads", 13.0, false);
    title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    title.setAccessibilityIdentifier(Some(ns_string!("syndeo.downloads")));
    let clear = button(mtm, "Clear", sel!(syndeoClearDownloads:), None);
    let open = button(mtm, "Open Folder", sel!(syndeoOpenDownloadsFolder:), None);
    stack.addArrangedSubview(&across(&title, &[&clear, &open]));

    let items: Vec<downloads::Download> = LIST.with(|list| list.borrow().items().to_vec());
    if items.is_empty() {
        let none = label(mtm, "Nothing downloaded yet.", 12.0, true);
        none.setAccessibilityIdentifier(Some(ns_string!("syndeo.downloads-none")));
        stack.addArrangedSubview(&none);
    }
    for item in &items {
        let rule = objc2_app_kit::NSBox::new(mtm);
        rule.setBoxType(objc2_app_kit::NSBoxType::Separator);
        rule.setTranslatesAutoresizingMaskIntoConstraints(false);
        rule.widthAnchor()
            .constraintEqualToConstant(inner)
            .setActive(true);
        stack.addArrangedSubview(&rule);

        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        row.setAlignment(NSLayoutAttribute::Leading);
        row.setSpacing(4.0);
        row.setTranslatesAutoresizingMaskIntoConstraints(false);

        let name = label(mtm, &item.name, 12.0, false);
        name.setFont(Some(&NSFont::systemFontOfSize_weight(12.0, unsafe {
            objc2_app_kit::NSFontWeightMedium
        })));
        name.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        name.setAccessibilityIdentifier(Some(ns_string!("syndeo.download-name")));
        name.widthAnchor()
            .constraintLessThanOrEqualToConstant(inner)
            .setActive(true);
        row.addArrangedSubview(&name);

        let detail = label(mtm, &downloads::describe(item), 11.0, true);
        detail.setAccessibilityIdentifier(Some(ns_string!("syndeo.download-detail")));
        let action: Option<Retained<NSButton>> = match item.state {
            State::Receiving => Some(button(
                mtm,
                "Cancel",
                sel!(syndeoCancelDownload:),
                Some(item.id),
            )),
            State::Done if item.path.is_some() => Some(button(
                mtm,
                "Show in Finder",
                sel!(syndeoRevealDownload:),
                Some(item.id),
            )),
            _ => None,
        };
        let right: Vec<&NSView> = action.iter().map(|b| -> &NSView { b }).collect();
        row.addArrangedSubview(&across(&detail, &right));

        if item.state == State::Receiving {
            let bar = NSProgressIndicator::new(mtm);
            bar.setStyle(NSProgressIndicatorStyle::Bar);
            bar.setControlSize(NSControlSize::Small);
            bar.setTranslatesAutoresizingMaskIntoConstraints(false);
            match downloads::fraction(item) {
                Some(fraction) => {
                    bar.setIndeterminate(false);
                    bar.setMinValue(0.0);
                    bar.setMaxValue(1.0);
                    bar.setDoubleValue(fraction);
                }
                None => {
                    bar.setIndeterminate(true);
                    unsafe { bar.startAnimation(None) };
                }
            }
            bar.widthAnchor()
                .constraintEqualToConstant(inner)
                .setActive(true);
            row.addArrangedSubview(&bar);
        }
        stack.addArrangedSubview(&row);
    }

    let container = NSView::new(mtm);
    container.addSubview(&stack);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        stack.widthAnchor().constraintEqualToConstant(WIDTH),
        stack
            .leadingAnchor()
            .constraintEqualToAnchor(&container.leadingAnchor()),
        stack
            .trailingAnchor()
            .constraintEqualToAnchor(&container.trailingAnchor()),
        stack
            .topAnchor()
            .constraintEqualToAnchor(&container.topAnchor()),
        stack
            .bottomAnchor()
            .constraintEqualToAnchor(&container.bottomAnchor()),
    ]));
    let height = stack.fittingSize().height.max(60.0);
    container.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(WIDTH, height),
    ));
    container
}
