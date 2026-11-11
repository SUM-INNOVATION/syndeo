//! Downloads, as a list: what has been downloaded this session, what is
//! still coming, and what failed or was blocked, newest first; and each
//! window's rule for which downloads it lets begin ([`Gate`]). Plain Rust:
//! the names, paths, counts and decisions, and nothing of WebKit, which
//! does the downloading (`downloads_macos`) and says how each one began
//! (`browser_macos`).

use crate::address;
use std::path::{Path, PathBuf};

/// How far a download has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Still coming in.
    Receiving,
    Done,
    Failed(String),
    Cancelled,
    /// Never begun: the page had had its one automatic download
    /// ([`BLOCKED`]).
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub id: u64,
    /// The file's name, as saved.
    pub name: String,
    /// Where it is saved, once that is decided.
    pub path: Option<PathBuf>,
    /// How many bytes there are, when the server said.
    pub total: Option<u64>,
    pub received: u64,
    pub state: State,
}

/// The downloads, newest first.
#[derive(Debug, Default)]
pub struct List {
    items: Vec<Download>,
    next_id: u64,
}

/// The most kept: older finished ones fall off the end.
const MOST: usize = 50;

/// Why a download failed that a server redirected somewhere a window may not
/// go (`address::download_may_follow`).
pub const REDIRECT_REFUSED: &str =
    "it was redirected to an address Syndeo does not open, and was stopped";

/// Why a download never began that a page started by itself, after its one
/// automatic download ([`Gate`]).
pub const BLOCKED: &str = "this page tried to download more files";

impl List {
    /// A download that has started, not yet with a name or a place. Its id.
    pub fn begin(&mut self) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.items.insert(
            0,
            Download {
                id,
                name: "Download".to_string(),
                path: None,
                total: None,
                received: 0,
                state: State::Receiving,
            },
        );
        self.trim();
        id
    }

    /// A download a page started by itself and was refused, after its one
    /// automatic download: a row that says so, named `name`. Its id.
    pub fn block(&mut self, name: &str) -> u64 {
        let id = self.begin();
        if let Some(item) = self.get_mut(id) {
            item.name = name.to_string();
            item.state = State::Blocked;
        }
        id
    }

    /// Whether a download still coming in is to be saved at `path`: its
    /// name is reserved, so that another, begun meanwhile, picks another.
    pub fn reserved(&self, path: &Path) -> bool {
        self.items
            .iter()
            .any(|item| item.state == State::Receiving && item.path.as_deref() == Some(path))
    }

    /// Whether `id` is a blocked row still in the list.
    pub fn is_blocked(&self, id: u64) -> bool {
        self.get(id)
            .is_some_and(|item| item.state == State::Blocked)
    }

    /// Forget every download, finished, failed, blocked, refused or still
    /// coming in, as Clear Browsing Data does: what is coming in is
    /// cancelled first (`downloads_macos::forget_all`). The files already
    /// saved are not touched.
    pub fn clear_all(&mut self) {
        self.items.clear();
    }

    /// Where a download is going, and how big it is if the server said.
    pub fn place(&mut self, id: u64, path: PathBuf, total: Option<u64>) {
        if let Some(item) = self.get_mut(id) {
            item.name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Download".to_string());
            item.path = Some(path);
            item.total = total;
        }
    }

    pub fn progress(&mut self, id: u64, received: u64, total: Option<u64>) {
        if let Some(item) = self.get_mut(id) {
            if item.state == State::Receiving {
                item.received = received;
                if total.is_some() {
                    item.total = total;
                }
            }
        }
    }

    pub fn finish(&mut self, id: u64) {
        if let Some(item) = self.get_mut(id) {
            item.state = State::Done;
            if let Some(total) = item.total {
                item.received = total;
            }
        }
    }

    pub fn fail(&mut self, id: u64, why: &str) {
        if let Some(item) = self.get_mut(id) {
            item.state = State::Failed(why.to_string());
        }
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(item) = self.get_mut(id) {
            if item.state == State::Receiving {
                item.state = State::Cancelled;
            }
        }
    }

    /// Forget every download that is over.
    pub fn clear_finished(&mut self) {
        self.items.retain(|item| item.state == State::Receiving);
    }

    pub fn get(&self, id: u64) -> Option<&Download> {
        self.items.iter().find(|item| item.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Download> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    pub fn items(&self) -> &[Download] {
        &self.items
    }

    /// How many are still coming in.
    pub fn receiving(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.state == State::Receiving)
            .count()
    }

    fn trim(&mut self) {
        while self.items.len() > MOST {
            match self
                .items
                .iter()
                .rposition(|item| item.state != State::Receiving)
            {
                Some(at) => {
                    self.items.remove(at);
                }
                None => break,
            }
        }
    }
}

/// The most bytes of UTF-8 a file's name may have on APFS (and HFS+):
/// longer, and saving it fails.
pub const NAME_BYTES: usize = 255;

/// Unicode's format characters (general category Cf, as of Unicode 17):
/// invisible, and some of them reorder what is shown around them, such as
/// the bidirectional overrides U+202A–202E and isolates U+2066–2069, which
/// can make `invoice<U+202E>fdp.command` show as a PDF's name, and the
/// zero-width characters U+200B–200F.
const FORMAT: [(u32, u32); 21] = [
    (0x00AD, 0x00AD),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
];

/// Whether `c` has no place in a file's name: a control character (Cc), a
/// format character (Cf, [`FORMAT`]), or the line and paragraph separators
/// U+2028 and U+2029.
fn unfit(c: char) -> bool {
    let code = u32::from(c);
    c.is_control()
        || code == 0x2028
        || code == 0x2029
        || FORMAT.iter().any(|&(from, to)| (from..=to).contains(&code))
}

/// A file name a server suggested, made safe to save under: no path in it,
/// no control, format or separator characters ([`unfit`]), not empty, not
/// hidden, and no longer than [`NAME_BYTES`], cut short before its
/// extension, which it keeps.
pub fn safe_name(suggested: &str) -> String {
    let name: String = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|&c| !unfit(c))
        .collect();
    let name = name.trim().trim_start_matches('.').to_string();
    if name.is_empty() {
        "download".to_string()
    } else {
        let (stem, ext) = split_extension(&name);
        fit(stem, "", ext)
    }
}

/// A name's stem and its extension with its dot, or none: the extension is
/// what follows the last dot, if that is not the first character, and is
/// short enough to be one (at most 32 bytes).
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 32 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// `stem`, then `suffix`, then `ext`, in at most [`NAME_BYTES`] bytes: the
/// stem cut short, at a character's boundary, to make room.
fn fit(stem: &str, suffix: &str, ext: &str) -> String {
    let room = NAME_BYTES.saturating_sub(suffix.len() + ext.len());
    if stem.len() <= room {
        return format!("{stem}{suffix}{ext}");
    }
    let mut end = room;
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let stem = stem[..end].trim_end();
    let stem = if stem.is_empty() { "download" } else { stem };
    format!("{stem}{suffix}{ext}")
}

/// Whether something is at `path` already: a file, a folder, or a link,
/// even one to nothing, which a download must not be saved through.
pub fn taken(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// A path in `dir` for `name` that `exists` says is free: the name, or the
/// name with " 2", " 3" and so on before its extension, each within
/// [`NAME_BYTES`].
pub fn unique_path(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = split_extension(name);
    (2..)
        .map(|n| dir.join(fit(stem, &format!(" {n}"), ext)))
        .find(|path| !exists(path))
        .expect("some number is free")
}

/// Bytes, as people read them.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A download's second line: how far it has got, or how it ended.
pub fn describe(download: &Download) -> String {
    match &download.state {
        State::Receiving => match download.total {
            Some(total) if total > 0 => {
                format!("{} of {}", human(download.received), human(total))
            }
            _ => format!("{} so far", human(download.received)),
        },
        State::Done => match download.total.or(Some(download.received)) {
            Some(size) if size > 0 => human(size),
            _ => "Done".to_string(),
        },
        State::Failed(why) => format!("Failed: {why}"),
        State::Cancelled => "Cancelled".to_string(),
        State::Blocked => format!("Blocked: {BLOCKED}"),
    }
}

/// How far along, from 0 to 1, when the size is known.
pub fn fraction(download: &Download) -> Option<f64> {
    match download.total {
        Some(total) if total > 0 => Some((download.received as f64 / total as f64).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Who began the main frame's navigation now under way, as its window saw
/// it when WebKit asked about it: kept until its page comes, or its
/// response becomes a download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Navigating {
    /// The person: by a click WebKit reports ([`clicked`]), or one the
    /// window saw in the page, or through the window itself (an address
    /// typed, Back, Forward, Reload, a tab opened).
    pub person: bool,
    /// The main frame began it, not a frame inside the page.
    pub by_main_frame: bool,
}

/// A download a window is asked to let begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked<'a> {
    /// The main frame began it, for the main frame: a link marked for
    /// downloading in the page itself, or the page's own response.
    pub main_frame: bool,
    /// The scheme of its address, and whether that names an IPv6 host with
    /// a zone (`address::has_zone`).
    pub scheme: &'a str,
    pub zoned: bool,
    /// WebKit reports the person's click for it ([`clicked`]), or, for a
    /// response, its navigation was the person's ([`Navigating`]).
    pub person: bool,
}

/// What a window does with a download it is asked to let begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It begins: the person started it, or it is the page's one automatic
    /// download.
    Allowed { by_person: bool },
    /// Refused: a frame inside the page started it.
    Subframe,
    /// Refused: its address's scheme is not one a download may come from
    /// (`address::may_download`).
    Scheme,
    /// Refused: the page started it by itself, and has had its one
    /// automatic download; the list shows it blocked ([`BLOCKED`]).
    Automatic,
}

/// Whether WebKit reports a navigation as a person's click:
/// `WKNavigationAction.buttonNumber`, the mask of mouse buttons pressed,
/// which WebKit leaves empty unless a trusted mouse event caused it.
///
/// Measured on macOS 15, CI 43100625418 (ci/test-app.sh downloads, which
/// logs every navigation action), for each way a download begins, as
/// navigationType, buttonNumber, the frame that began it and the frame it
/// is for, and shouldPerformDownload; modifierFlags was 0 in every one:
/// - a person's click with the mouse on a link marked for downloading:
///   link, 1, main, main, yes;
/// - on a plain link to a file (a response): link, 1, main, main, no;
/// - a link marked for downloading pressed through Accessibility (AXPress,
///   as VoiceOver presses it): link, 1, main, main, yes;
/// - a script's `click()` on such a link, on load: link, 0, main, main,
///   yes;
/// - a click the script made up as a `MouseEvent` and dispatched: link, 0,
///   main, main, yes;
/// - the page's address set to a file, on load: other, 0, main, main, no;
/// - a meta refresh to a file: other, 0, main, main, no;
/// - each of `click()`, its address and a meta refresh in a hidden frame
///   from another origin: link or other, 0, inner, inner.
///
/// So buttonNumber tells the person's click from a page's, and a page
/// cannot make one up; navigationType cannot (a script's `click()` is a
/// link too). What it does not see, a download a page starts from the
/// person's click on a button, or from Return or Space, the window's own
/// watch for the person's acts in the page covers ([`Gate::person_acted`]).
pub fn clicked(buttons: isize) -> bool {
    buttons != 0
}

/// A window's rule for downloads, and what it has seen to apply it.
///
/// - **Only the main frame downloads.** Anything a frame inside the page
///   starts is refused: a link marked for downloading there, a response
///   there that is not a page, or a navigation of the main frame that frame
///   began.
/// - **Only from the web's own addresses**, and `blob:` and `data:`, which
///   a page makes for a file it generates, such as an export
///   (`address::may_download`).
/// - **What the person asks for always begins**: a link they click, a
///   page's download their click (or Return or Space) in it started, an
///   address they typed. A person's act in the page is spent by the
///   download it allows, or by the next page; one through the window (an
///   address typed, Back, Forward, Reload) by the page it brings.
/// - **One automatic download a page**: a download the person did not
///   start (a script's `click()` on load, its address set to a file, a meta
///   refresh to one) begins only if the page has had none. Every further one
///   is refused, and shown blocked, until the person clicks in the page or
///   navigates. A page that navigates by itself (a script, a meta refresh, a
///   redirect) does not earn another: otherwise a page reloading itself
///   would download again and again.
#[derive(Debug, Default)]
pub struct Gate {
    /// The person acted in the window, and nothing has spent it yet.
    acted: bool,
    /// The page has had its one automatic download.
    automatic_spent: bool,
    /// The main frame's navigation under way.
    navigating: Option<Navigating>,
    /// The page's blocked row in the list, if it has one: further blocked
    /// downloads go in it rather than a row each.
    blocked_row: Option<u64>,
}

impl Gate {
    /// The person clicked in the page, or pressed Return or Space in it, or
    /// asked the window to go somewhere.
    pub fn person_acted(&mut self) {
        self.acted = true;
    }

    /// WebKit asks about a navigation of the main frame, begun by its main
    /// frame (`by_main_frame`) or by a frame inside the page, with the
    /// person's click reported or not (`clicked`). What is kept of it.
    pub fn navigation(&mut self, by_main_frame: bool, clicked: bool) -> Navigating {
        let navigating = Navigating {
            person: clicked || self.acted,
            by_main_frame,
        };
        self.navigating = Some(navigating);
        navigating
    }

    /// The main frame's navigation under way, given up: its response is
    /// not a page, so no page comes of it.
    pub fn take_navigation(&mut self) -> Option<Navigating> {
        self.navigating.take()
    }

    /// The main frame's navigation brought a page. The person's act is
    /// spent; a page the person brought may have an automatic download
    /// again, one the page brought by itself may not.
    pub fn page_came(&mut self) {
        let by_person = self.navigating.take().is_some_and(|n| n.person);
        self.acted = false;
        self.blocked_row = None;
        if by_person {
            self.automatic_spent = false;
        }
    }

    /// Whether a download may begin, by the rule above; one that does
    /// spends the person's act, or the page's automatic download.
    pub fn judge(&mut self, asked: &Asked) -> Verdict {
        if !asked.main_frame {
            return Verdict::Subframe;
        }
        if !address::may_download(asked.scheme, asked.zoned) {
            return Verdict::Scheme;
        }
        if asked.person || self.acted {
            self.acted = false;
            return Verdict::Allowed { by_person: true };
        }
        if self.automatic_spent {
            return Verdict::Automatic;
        }
        self.automatic_spent = true;
        Verdict::Allowed { by_person: false }
    }

    /// The page's blocked row, if it has one, and its new one.
    pub fn blocked_row(&self) -> Option<u64> {
        self.blocked_row
    }

    pub fn set_blocked_row(&mut self, id: u64) {
        self.blocked_row = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_goes_from_begun_to_done() {
        let mut list = List::default();
        let id = list.begin();
        assert_eq!(list.receiving(), 1);
        list.place(id, PathBuf::from("/tmp/Downloads/report.pdf"), Some(2000));
        assert_eq!(list.get(id).unwrap().name, "report.pdf");
        list.progress(id, 1000, None);
        assert_eq!(describe(list.get(id).unwrap()), "1.0 KB of 2.0 KB");
        assert_eq!(fraction(list.get(id).unwrap()), Some(0.5));
        list.finish(id);
        assert_eq!(list.get(id).unwrap().state, State::Done);
        assert_eq!(describe(list.get(id).unwrap()), "2.0 KB");
        assert_eq!(list.receiving(), 0);
        list.clear_finished();
        assert!(list.items().is_empty());
    }

    #[test]
    fn newest_first_and_the_old_fall_off() {
        let mut list = List::default();
        let first = list.begin();
        let second = list.begin();
        assert_eq!(list.items()[0].id, second);
        assert_eq!(list.items()[1].id, first);
        for _ in 0..MOST {
            let id = list.begin();
            list.finish(id);
        }
        assert_eq!(list.items().len(), MOST);
        // The two still receiving are kept, whatever their age.
        assert!(list.get(first).is_some());
        assert!(list.get(second).is_some());
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\Users\\x\\a.txt"), "a.txt");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("  "), "download");
        assert_eq!(safe_name("bad\nname.pdf"), "badname.pdf");
        assert_eq!(safe_name("report .pdf"), "report .pdf");
    }

    #[test]
    fn format_characters_and_separators_are_taken_out_of_names() {
        // The right-to-left override that shows `invoice<RLO>fdp.command`
        // as a PDF's name.
        assert_eq!(
            safe_name("invoice\u{202E}fdp.command"),
            "invoicefdp.command"
        );
        for c in [
            '\u{202A}',
            '\u{202B}',
            '\u{202C}',
            '\u{202D}',
            '\u{202E}',
            '\u{2066}',
            '\u{2067}',
            '\u{2068}',
            '\u{2069}',
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{200E}',
            '\u{200F}',
            '\u{2060}',
            '\u{FEFF}',
            '\u{00AD}',
            '\u{061C}',
            '\u{E0001}',
            '\u{E007F}',
            '\u{2028}',
            '\u{2029}',
            '\u{0085}',
            '\u{007F}',
        ] {
            assert_eq!(
                safe_name(&format!("a{c}b.txt")),
                "ab.txt",
                "U+{:04X}",
                u32::from(c)
            );
        }
        // Only what is invisible: letters of any script stay.
        assert_eq!(safe_name("отчёт 報告 تقرير.pdf"), "отчёт 報告 تقرير.pdf");
        assert_eq!(safe_name("\u{202E}\u{200B}"), "download");
        assert_eq!(safe_name("\u{FEFF}.hidden"), "hidden");
    }

    #[test]
    fn a_long_name_is_cut_short_before_its_extension() {
        let long = format!("{}.pdf", "a".repeat(400));
        let name = safe_name(&long);
        assert_eq!(name.len(), NAME_BYTES);
        assert!(name.ends_with("a.pdf"), "{name}");
        // Never through a character: 'é' is two bytes, '報' three.
        for filler in ["é", "報", "😀"] {
            let name = safe_name(&format!("{}.zip", filler.repeat(200)));
            assert!(name.len() <= NAME_BYTES, "{filler}: {}", name.len());
            assert!(
                name.len() > NAME_BYTES - 4 - filler.len(),
                "{filler}: {}",
                name.len()
            );
            assert!(name.ends_with(".zip"));
            assert!(name
                .trim_end_matches(".zip")
                .chars()
                .all(|c| c.to_string() == filler));
        }
        // A name with no extension, or one too long to be one, is cut short
        // as a whole.
        assert_eq!(safe_name(&"b".repeat(300)), "b".repeat(NAME_BYTES));
        let odd = format!("x.{}", "y".repeat(300));
        assert_eq!(safe_name(&odd).len(), NAME_BYTES);
        assert!(safe_name(&odd).starts_with("x.y"));
        // Short names are as they were.
        assert_eq!(safe_name("a.pdf"), "a.pdf");
    }

    #[test]
    fn a_numbered_name_stays_within_the_limit() {
        let dir = Path::new("/d");
        let name = safe_name(&format!("{}.tar.gz", "n".repeat(300)));
        assert_eq!(name.len(), NAME_BYTES);
        let first = dir.join(&name);
        let path = unique_path(dir, &name, |p| p == first);
        let file = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(file.len(), NAME_BYTES);
        assert!(file.ends_with("n 2.gz"), "{file}");
        let many = unique_path(dir, &name, |p| {
            !p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with(" 100.gz")
        });
        assert!(many.file_name().unwrap().len() <= NAME_BYTES);
    }

    #[test]
    fn a_link_to_nothing_is_taken() {
        let dir = std::env::temp_dir().join(format!("syndeo-taken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("report.pdf");
        std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();
        assert!(
            !link.exists(),
            "a dangling link: exists() says nothing is there"
        );
        assert!(taken(&link));
        assert_eq!(
            unique_path(&dir, "report.pdf", taken),
            dir.join("report 2.pdf")
        );
        let file = dir.join("data.bin");
        std::fs::write(&file, b"x").unwrap();
        assert!(taken(&file));
        assert!(taken(&dir));
        assert!(!taken(&dir.join("free.bin")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_coming_in_reserves_its_name() {
        let mut list = List::default();
        let dir = Path::new("/d");
        let first = list.begin();
        let path = unique_path(dir, "a.bin", |p| list.reserved(p));
        assert_eq!(path, PathBuf::from("/d/a.bin"));
        list.place(first, path.clone(), None);
        // Another, begun before the first has written anything there.
        let second = list.begin();
        let next = unique_path(dir, "a.bin", |p| list.reserved(p));
        assert_eq!(next, PathBuf::from("/d/a 2.bin"));
        list.place(second, next, None);
        // Once over, its name is no longer reserved: the file, if there is
        // one, is what takes it then.
        list.finish(first);
        assert!(!list.reserved(&path));
        list.cancel(second);
        assert!(!list.reserved(Path::new("/d/a 2.bin")));
    }

    #[test]
    fn a_taken_name_is_numbered_before_its_extension() {
        let dir = Path::new("/d");
        let taken = |p: &Path| p == Path::new("/d/a.tar.gz") || p == Path::new("/d/a.tar 2.gz");
        assert_eq!(
            unique_path(dir, "a.tar.gz", taken),
            PathBuf::from("/d/a.tar 3.gz")
        );
        assert_eq!(unique_path(dir, "b", |_| false), PathBuf::from("/d/b"));
        assert_eq!(
            unique_path(dir, "noext", |p| p == Path::new("/d/noext")),
            PathBuf::from("/d/noext 2")
        );
    }

    #[test]
    fn bytes_read_as_people_read_them() {
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1500), "1.5 KB");
        assert_eq!(human(123_456_789), "123 MB");
        assert_eq!(human(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn a_failure_and_a_cancel_say_so() {
        let mut list = List::default();
        let a = list.begin();
        let b = list.begin();
        list.fail(a, "the network went away");
        list.cancel(b);
        assert_eq!(
            describe(list.get(a).unwrap()),
            "Failed: the network went away"
        );
        assert_eq!(describe(list.get(b).unwrap()), "Cancelled");
        // Nothing moves a finished download on.
        list.progress(a, 10, None);
        assert_eq!(list.get(a).unwrap().received, 0);
    }

    #[test]
    fn a_refused_redirect_says_why() {
        let mut list = List::default();
        let id = list.begin();
        list.fail(id, REDIRECT_REFUSED);
        assert_eq!(
            describe(list.get(id).unwrap()),
            "Failed: it was redirected to an address Syndeo does not open, and was stopped"
        );
        assert_eq!(list.receiving(), 0);
    }

    fn asked(main_frame: bool, scheme: &str, person: bool) -> Asked<'_> {
        Asked {
            main_frame,
            scheme,
            zoned: false,
            person,
        }
    }

    /// A window on a page the person brought: an address typed, then the
    /// page.
    fn on_a_page() -> Gate {
        let mut gate = Gate::default();
        gate.person_acted();
        gate.navigation(true, false);
        gate.page_came();
        gate
    }

    #[test]
    fn only_the_main_frame_downloads() {
        let mut gate = on_a_page();
        // A frame inside the page: refused, the person's click or not, and
        // it spends nothing.
        assert_eq!(gate.judge(&asked(false, "https", false)), Verdict::Subframe);
        assert_eq!(gate.judge(&asked(false, "https", true)), Verdict::Subframe);
        gate.person_acted();
        assert_eq!(gate.judge(&asked(false, "blob", false)), Verdict::Subframe);
        // The main frame still has its act, and its automatic download.
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: true }
        );
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: false }
        );
    }

    #[test]
    fn a_navigation_a_frame_began_downloads_nothing() {
        let mut gate = on_a_page();
        let navigating = gate.navigation(false, true);
        assert!(!navigating.by_main_frame);
        // Its response, for the main frame, is judged as the frame's.
        let taken = gate.take_navigation().unwrap();
        assert_eq!(
            gate.judge(&asked(taken.by_main_frame, "https", taken.person)),
            Verdict::Subframe
        );
    }

    #[test]
    fn one_automatic_download_a_page() {
        let mut gate = on_a_page();
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: false }
        );
        for _ in 0..5 {
            assert_eq!(gate.judge(&asked(true, "https", false)), Verdict::Automatic);
        }
        // What the person clicks still begins, each time.
        for _ in 0..3 {
            assert_eq!(
                gate.judge(&asked(true, "https", true)),
                Verdict::Allowed { by_person: true }
            );
        }
        // A click in the page lets one more begin, its own; then not again.
        gate.person_acted();
        assert_eq!(
            gate.judge(&asked(true, "blob", false)),
            Verdict::Allowed { by_person: true }
        );
        assert_eq!(gate.judge(&asked(true, "blob", false)), Verdict::Automatic);
    }

    #[test]
    fn a_page_the_person_brings_has_its_own_automatic_download() {
        let mut gate = on_a_page();
        gate.judge(&asked(true, "https", false));
        assert_eq!(gate.judge(&asked(true, "https", false)), Verdict::Automatic);
        // A link the person clicks, to another page.
        gate.navigation(true, true);
        gate.page_came();
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: false }
        );
        assert_eq!(gate.judge(&asked(true, "https", false)), Verdict::Automatic);
        // An address typed: the window says the person acted.
        gate.person_acted();
        gate.navigation(true, false);
        gate.page_came();
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: false }
        );
    }

    #[test]
    fn a_page_navigating_by_itself_earns_no_more() {
        let mut gate = on_a_page();
        gate.judge(&asked(true, "https", false));
        // The page reloads itself, again and again.
        for _ in 0..3 {
            gate.navigation(true, false);
            gate.page_came();
            assert_eq!(gate.judge(&asked(true, "https", false)), Verdict::Automatic);
        }
    }

    #[test]
    fn a_person_s_act_is_spent_by_the_next_page() {
        let mut gate = on_a_page();
        // An address typed is not left over for the page it brings to
        // spend on a download of its own.
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: false }
        );
        // A click on a link to another page: the page it brings has its
        // automatic download, and no click left over besides.
        gate.person_acted();
        gate.navigation(true, false);
        gate.page_came();
        assert_eq!(
            gate.judge(&asked(true, "https", false)),
            Verdict::Allowed { by_person: false }
        );
        assert_eq!(gate.judge(&asked(true, "https", false)), Verdict::Automatic);
    }

    #[test]
    fn a_response_the_person_asked_for_downloads() {
        let mut gate = on_a_page();
        gate.judge(&asked(true, "https", false));
        // An address typed, to a file: its response becomes a download.
        gate.person_acted();
        let navigating = gate.navigation(true, false);
        assert!(navigating.person);
        let taken = gate.take_navigation().unwrap();
        assert_eq!(
            gate.judge(&asked(taken.by_main_frame, "https", taken.person)),
            Verdict::Allowed { by_person: true }
        );
        // The page meanwhile still has its own spent.
        assert_eq!(gate.judge(&asked(true, "https", false)), Verdict::Automatic);
        // And a response the page asked for by itself is automatic.
        let taken = gate.navigation(true, false);
        assert_eq!(
            gate.judge(&asked(taken.by_main_frame, "https", taken.person)),
            Verdict::Automatic
        );
    }

    #[test]
    fn downloads_come_only_from_the_web_and_a_page_s_own_files() {
        for scheme in ["http", "https", "HTTPS", "blob", "data"] {
            let mut gate = on_a_page();
            assert_eq!(
                gate.judge(&asked(true, scheme, true)),
                Verdict::Allowed { by_person: true },
                "{scheme}"
            );
        }
        for scheme in ["file", "about", "javascript", "ftp", "mailto", "x-app", ""] {
            let mut gate = on_a_page();
            assert_eq!(
                gate.judge(&asked(true, scheme, true)),
                Verdict::Scheme,
                "{scheme}"
            );
            // Refused for its scheme, it spends nothing: the page still has
            // its automatic download.
            assert_eq!(
                gate.judge(&asked(true, "https", false)),
                Verdict::Allowed { by_person: false }
            );
        }
        let mut gate = on_a_page();
        let zoned = Asked {
            zoned: true,
            ..asked(true, "http", true)
        };
        assert_eq!(gate.judge(&zoned), Verdict::Scheme);
    }

    #[test]
    fn clicks_are_what_webkit_reports_as_buttons() {
        assert!(!clicked(0));
        assert!(clicked(1));
        assert!(clicked(1 << 2));
    }

    #[test]
    fn a_blocked_download_says_so_and_is_kept_with_the_rest() {
        let mut list = List::default();
        let done = list.begin();
        list.finish(done);
        let blocked = list.block("file.bin");
        assert!(list.is_blocked(blocked));
        assert!(!list.is_blocked(done));
        let row = list.get(blocked).unwrap();
        assert_eq!(row.name, "file.bin");
        assert_eq!(
            describe(row),
            "Blocked: this page tried to download more files"
        );
        assert_eq!(list.receiving(), 0);
        // Nothing moves it on.
        list.progress(blocked, 10, None);
        list.cancel(blocked);
        assert_eq!(list.get(blocked).unwrap().state, State::Blocked);
        list.clear_finished();
        assert!(list.items().is_empty());
    }

    #[test]
    fn clearing_everything_leaves_nothing_listed() {
        let mut list = List::default();
        let coming = list.begin();
        let done = list.begin();
        list.finish(done);
        let failed = list.begin();
        list.fail(failed, REDIRECT_REFUSED);
        list.block("x.bin");
        list.clear_all();
        assert!(list.items().is_empty());
        assert_eq!(list.receiving(), 0);
        assert!(list.get(coming).is_none());
        // Ids are not given again.
        assert!(list.begin() > coming.max(done).max(failed));
    }
}
