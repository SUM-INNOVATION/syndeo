//! The decisions about tabs, in plain Rust, so they are tested everywhere:
//! where a click on a link opens it, which tab ⌘1 to ⌘9 choose, the tabs
//! that were closed and can be opened again, where a pinned tab goes, and
//! the session kept from one launch to the next.
//!
//! The session holds the address and title of each open tab, and whether it
//! is pinned, in the order they were in, window by window; nothing else of a
//! page. It is read back only as addresses that typing would open
//! (`address::interpret`), so a damaged or altered file opens nothing a
//! window could not.

use crate::address::{self, Search, Typed};
use std::collections::VecDeque;

/// Where a navigation a page started opens: here, as the page asked, or in
/// a new tab or window because of the keys held or the button used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    Here,
    /// A tab beside this one: in front, or behind it.
    Tab {
        front: bool,
    },
    Window,
}

/// What WebKit says about a navigation: whether it came from a link or a
/// form, the keys held, and the mouse button, as `WKNavigationAction` gives
/// them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Click {
    /// A link followed or a form sent, by someone: not a script or a reload.
    pub followed: bool,
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    /// `WKNavigationAction.buttonNumber`, a mask of buttons: 1 the left, 2
    /// the right, 4 the middle (WebKit's `WebEventFactory::toNSButtonNumber`).
    pub buttons: isize,
}

/// The middle button, in `Click::buttons`.
const MIDDLE: isize = 1 << 2;

/// Where a click opens, as Safari has it: ⌘ or the middle button, a tab
/// behind this one, or in front with ⇧; ⌘⌥, a new window; anything else,
/// here. Only for a link or a form someone used: a script that navigates is
/// never sent elsewhere by a key that happens to be down.
pub fn opening(click: Click) -> Opening {
    if !click.followed {
        return Opening::Here;
    }
    let middle = click.buttons & MIDDLE != 0;
    if click.command && click.option {
        Opening::Window
    } else if click.command || middle {
        Opening::Tab { front: click.shift }
    } else {
        Opening::Here
    }
}

/// The tab ⌘`digit` chooses among `count`, by index: ⌘1 to ⌘8 that tab, if
/// there is one; ⌘9 the last, however many there are.
pub fn chosen_tab(digit: usize, count: usize) -> Option<usize> {
    match digit {
        9 => count.checked_sub(1),
        1..=8 if digit <= count => Some(digit - 1),
        _ => None,
    }
}

/// Where a tab that is being pinned, or unpinned, goes, by index, when
/// `pinned` other tabs of its window are pinned: pinned tabs come first, the
/// one pinned last after the others; a tab unpinned goes first among the
/// rest, after them all.
pub fn pinned_place(pinned: usize) -> usize {
    pinned
}

/// The least index a new tab may take among tabs of which the first
/// `pinned` are pinned: never among them.
pub fn after_pinned(wanted: usize, pinned: usize) -> usize {
    wanted.max(pinned)
}

/// How many closed tabs are kept to open again: 25, as Chrome and Firefox
/// keep. The oldest goes first.
pub const KEPT_CLOSED: usize = 25;

/// Tabs that were closed, the last closed first, at most [`KEPT_CLOSED`].
pub struct Closed<T> {
    tabs: VecDeque<T>,
}

impl<T> Default for Closed<T> {
    fn default() -> Self {
        Self {
            tabs: VecDeque::new(),
        }
    }
}

impl<T> Closed<T> {
    /// A tab was closed.
    pub fn push(&mut self, tab: T) {
        self.tabs.push_front(tab);
        self.tabs.truncate(KEPT_CLOSED);
    }

    /// The last tab closed, taken to open again.
    pub fn take_last(&mut self) -> Option<T> {
        self.tabs.pop_front()
    }

    /// The tab at `index`, as [`Closed::iter`] lists them, taken.
    pub fn take(&mut self, index: usize) -> Option<T> {
        self.tabs.remove(index)
    }

    /// The tabs, the last closed first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.tabs.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Forget them all, as Clear Browsing Data does.
    pub fn clear(&mut self) {
        self.tabs.clear();
    }
}

/// How long a title is in a menu, at most, in characters.
const MENU_TITLE: usize = 60;

/// A tab's title as a menu shows it: one line, at most [`MENU_TITLE`]
/// characters, and something even for a page with no title.
pub fn menu_title(title: &str, address: &str) -> String {
    let one_line: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = match one_line.trim() {
        "" if address.is_empty() => "Start Page",
        "" => address,
        trimmed => trimmed,
    };
    let mut shown: String = text.chars().take(MENU_TITLE).collect();
    if text.chars().count() > MENU_TITLE {
        shown.pop();
        shown.push('…');
    }
    shown
}

/// A tab, as the session keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTab {
    /// Its page's address: an `http` or `https` address, and nothing else.
    pub address: String,
    pub title: String,
    pub pinned: bool,
}

/// A window's tabs, in order, and the one that was in front.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedWindow {
    pub tabs: Vec<SavedTab>,
    pub selected: usize,
}

/// Every window's tabs, as they were when Syndeo quit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub windows: Vec<SavedWindow>,
}

/// The session file's first line, which says what it is and its version.
const SESSION_HEADER: &str = "syndeo-session 1";

/// The most windows, and tabs in each, read back from a session file, so a
/// damaged one cannot open without end.
const SESSION_WINDOWS: usize = 50;
const SESSION_TABS: usize = 500;

/// Whether `address` is one a tab may be saved, and opened again, with: an
/// `http` or `https` address, written with its scheme, that typing it would
/// open.
pub fn savable(address: &str) -> bool {
    let written = ["http://", "https://"].iter().any(|scheme| {
        address
            .get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    });
    written
        && matches!(address::interpret(address, Search::Off), Typed::Load(url)
            if url.scheme() == "http" || url.scheme() == "https")
}

/// One line of text: no tab, line break or other control character.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Session {
    /// Only what can be opened again: each window's tabs with an address
    /// [`savable`] allows, its windows with none left out, and the tab in
    /// front kept in front where it is still there.
    pub fn kept(mut self) -> Self {
        for window in &mut self.windows {
            let selected = window.tabs.get(window.selected).cloned();
            window.tabs.retain(|tab| savable(&tab.address));
            window.selected = selected
                .and_then(|tab| window.tabs.iter().position(|kept| *kept == tab))
                .unwrap_or(0);
        }
        self.windows.retain(|window| !window.tabs.is_empty());
        self
    }

    /// Its pinned tabs alone, as one window.
    pub fn pinned(&self) -> Vec<SavedTab> {
        self.windows
            .iter()
            .flat_map(|window| window.tabs.iter())
            .filter(|tab| tab.pinned)
            .cloned()
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// As the file has it: a header, then for each window a `window` line
    /// with the index of its tab in front, then a `tab` line for each tab,
    /// fields apart by tabs: `pinned` or `-`, the address, the title.
    pub fn to_text(&self) -> String {
        let mut text = format!("{SESSION_HEADER}\n");
        for window in &self.windows {
            text.push_str(&format!("window\t{}\n", window.selected));
            for tab in &window.tabs {
                text.push_str(&format!(
                    "tab\t{}\t{}\t{}\n",
                    if tab.pinned { "pinned" } else { "-" },
                    one_line(&tab.address),
                    one_line(&tab.title)
                ));
            }
        }
        text
    }

    /// Read back from the file's text: nothing from a file that is not a
    /// session, and of one, only lines as [`Session::to_text`] writes them,
    /// tabs whose address is [`savable`], and no more than a bounded number
    /// of windows and tabs.
    pub fn from_text(text: &str) -> Self {
        let mut lines = text.lines();
        if lines.next() != Some(SESSION_HEADER) {
            return Self::default();
        }
        let mut session = Self::default();
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["window", selected] => {
                    if session.windows.len() == SESSION_WINDOWS {
                        break;
                    }
                    session.windows.push(SavedWindow {
                        tabs: Vec::new(),
                        selected: selected.parse().unwrap_or(0),
                    });
                }
                ["tab", pin, address, title] if *pin == "pinned" || *pin == "-" => {
                    if let Some(window) = session.windows.last_mut() {
                        if window.tabs.len() < SESSION_TABS {
                            window.tabs.push(SavedTab {
                                address: (*address).to_string(),
                                title: (*title).to_string(),
                                pinned: *pin == "pinned",
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        for window in &mut session.windows {
            if window.selected >= window.tabs.len() {
                window.selected = 0;
            }
        }
        session.kept()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(command: bool, shift: bool, option: bool, buttons: isize) -> Click {
        Click {
            followed: true,
            command,
            shift,
            option,
            buttons,
        }
    }

    #[test]
    fn a_click_opens_where_safari_opens_it() {
        let left = 1;
        assert_eq!(opening(click(false, false, false, left)), Opening::Here);
        assert_eq!(
            opening(click(true, false, false, left)),
            Opening::Tab { front: false }
        );
        assert_eq!(
            opening(click(true, true, false, left)),
            Opening::Tab { front: true }
        );
        assert_eq!(
            opening(click(false, false, false, MIDDLE)),
            Opening::Tab { front: false }
        );
        assert_eq!(
            opening(click(false, true, false, MIDDLE)),
            Opening::Tab { front: true }
        );
        assert_eq!(opening(click(true, false, true, left)), Opening::Window);
        // ⇧ or ⌥ alone change nothing here, and neither does the right
        // button: WebKit never follows a link for it.
        assert_eq!(opening(click(false, true, false, left)), Opening::Here);
        assert_eq!(opening(click(false, false, true, left)), Opening::Here);
        assert_eq!(opening(click(false, false, false, 2)), Opening::Here);
        // A keyboard's ⌘-Return on a link has no button, and opens a tab.
        assert_eq!(
            opening(click(true, false, false, 0)),
            Opening::Tab { front: false }
        );
    }

    #[test]
    fn a_script_navigating_while_a_key_is_down_stays_here() {
        let script = Click {
            followed: false,
            command: true,
            buttons: MIDDLE,
            ..Click::default()
        };
        assert_eq!(opening(script), Opening::Here);
    }

    #[test]
    fn command_digits_choose_a_tab_and_nine_the_last() {
        assert_eq!(chosen_tab(1, 3), Some(0));
        assert_eq!(chosen_tab(3, 3), Some(2));
        assert_eq!(chosen_tab(4, 3), None);
        assert_eq!(chosen_tab(8, 12), Some(7));
        assert_eq!(chosen_tab(9, 12), Some(11));
        assert_eq!(chosen_tab(9, 1), Some(0));
        assert_eq!(chosen_tab(9, 0), None);
        assert_eq!(chosen_tab(1, 0), None);
        assert_eq!(chosen_tab(0, 5), None);
        assert_eq!(chosen_tab(10, 20), None);
    }

    #[test]
    fn pinned_tabs_come_first_and_new_tabs_never_go_among_them() {
        assert_eq!(pinned_place(0), 0);
        assert_eq!(pinned_place(2), 2);
        assert_eq!(after_pinned(1, 2), 2);
        assert_eq!(after_pinned(3, 2), 3);
        assert_eq!(after_pinned(0, 0), 0);
    }

    #[test]
    fn closed_tabs_reopen_last_first_and_only_so_many_are_kept() {
        let mut closed = Closed::default();
        assert!(closed.take_last().is_none());
        for n in 0..30 {
            closed.push(n);
        }
        assert_eq!(closed.len(), KEPT_CLOSED);
        assert_eq!(
            closed.iter().copied().take(3).collect::<Vec<_>>(),
            [29, 28, 27]
        );
        assert_eq!(closed.take_last(), Some(29));
        assert_eq!(closed.take(1), Some(27));
        assert_eq!(closed.take_last(), Some(28));
        assert_eq!(
            closed.iter().last(),
            Some(&5),
            "the oldest five were let go"
        );
        closed.clear();
        assert!(closed.is_empty());
    }

    #[test]
    fn a_menu_title_is_one_short_line() {
        assert_eq!(
            menu_title("Example Domain", "https://example.com/"),
            "Example Domain"
        );
        assert_eq!(
            menu_title("  ", "https://example.com/"),
            "https://example.com/"
        );
        assert_eq!(menu_title("", ""), "Start Page");
        assert_eq!(menu_title("a\tb\nc", ""), "a b c");
        let long = "x".repeat(100);
        let shown = menu_title(&long, "");
        assert_eq!(shown.chars().count(), MENU_TITLE);
        assert!(shown.ends_with('…'));
    }

    fn tab(address: &str, title: &str, pinned: bool) -> SavedTab {
        SavedTab {
            address: address.into(),
            title: title.into(),
            pinned,
        }
    }

    #[test]
    fn a_session_reads_back_as_it_was_written() {
        let session = Session {
            windows: vec![
                SavedWindow {
                    tabs: vec![
                        tab("https://example.com/", "Example Domain", true),
                        tab("http://127.0.0.1:8080/a?b=c", "A\tpage\nwith breaks", false),
                    ],
                    selected: 1,
                },
                SavedWindow {
                    tabs: vec![tab("https://www.rust-lang.org/", "Rust", false)],
                    selected: 0,
                },
            ],
        };
        let read = Session::from_text(&session.to_text());
        assert_eq!(read.windows.len(), 2);
        assert_eq!(read.windows[0].selected, 1);
        assert_eq!(read.windows[0].tabs[1].title, "A page with breaks");
        assert_eq!(read.windows[0].tabs[0], session.windows[0].tabs[0]);
        assert_eq!(read.windows[1], session.windows[1]);
        assert_eq!(
            read.pinned(),
            vec![tab("https://example.com/", "Example Domain", true)]
        );
    }

    /// Whatever the file says, only web addresses that typing would open
    /// come back: no other scheme, no address with a zone, nothing that is
    /// not an address at all.
    #[test]
    fn a_session_opens_nothing_a_window_could_not() {
        let text = format!(
            "{SESSION_HEADER}\nwindow\t3\n\
             tab\t-\tfile:///etc/passwd\tpasswd\n\
             tab\t-\tjavascript:alert(1)\tscript\n\
             tab\tpinned\tdata:text/html,hi\tdata\n\
             tab\t-\thttp://[fe80::1%25en0]/\tzoned\n\
             tab\t-\tbread\twords\n\
             tab\t-\thttps://example.com/\tExample\n\
             tab\tmaybe\thttps://example.org/\tunknown pin\n\
             tab\t-\thttps://example.net/\n\
             nonsense\n\
             window\tx\n\
             tab\t-\tsyndeo://x\tother\n"
        );
        let read = Session::from_text(&text);
        assert_eq!(
            read,
            Session {
                windows: vec![SavedWindow {
                    tabs: vec![tab("https://example.com/", "Example", false)],
                    selected: 0
                }]
            }
        );
    }

    #[test]
    fn anything_but_a_session_file_is_no_session() {
        for text in [
            "",
            "window\t0\ntab\t-\thttps://example.com/\tx\n",
            "syndeo-session 2\nwindow\t0\n",
        ] {
            assert!(Session::from_text(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn a_session_file_cannot_open_without_end() {
        let mut text = format!("{SESSION_HEADER}\n");
        for _ in 0..(SESSION_WINDOWS + 5) {
            text.push_str("window\t0\n");
            for _ in 0..(SESSION_TABS + 5) {
                text.push_str("tab\t-\thttps://example.com/\tx\n");
            }
        }
        let read = Session::from_text(&text);
        assert_eq!(read.windows.len(), SESSION_WINDOWS);
        assert!(read.windows.iter().all(|w| w.tabs.len() == SESSION_TABS));
    }

    #[test]
    fn only_web_addresses_with_their_scheme_are_savable() {
        assert!(savable("https://example.com/"));
        assert!(savable("http://127.0.0.1:8080/x?y=z"));
        assert!(
            !savable("example.com"),
            "typing completes it; the session never has to"
        );
        assert!(!savable(" https://example.com/"), "as typing would trim it");
        assert!(!savable(""));
        assert!(!savable("about:blank"));
        assert!(!savable("file:///etc/passwd"));
        assert!(!savable("http://[fe80::1%25en0]/"));
    }

    #[test]
    fn keeping_a_session_keeps_the_tab_in_front_where_it_still_is() {
        let session = Session {
            windows: vec![
                SavedWindow {
                    tabs: vec![
                        tab("about:blank", "", false),
                        tab("https://example.com/", "a", false),
                        tab("https://example.org/", "b", false),
                    ],
                    selected: 2,
                },
                SavedWindow {
                    tabs: vec![tab("about:blank", "", false)],
                    selected: 0,
                },
            ],
        }
        .kept();
        assert_eq!(
            session.windows.len(),
            1,
            "a window of start pages alone is not kept"
        );
        assert_eq!(session.windows[0].tabs.len(), 2);
        assert_eq!(session.windows[0].selected, 1);
    }
}
