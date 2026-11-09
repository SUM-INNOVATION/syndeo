//! Downloads, as a list: what has been downloaded this session, what is
//! still coming, and what failed, newest first. Plain Rust: the names,
//! paths and counts, and nothing of WebKit, which does the downloading
//! (`downloads_macos`).

use std::path::{Path, PathBuf};

/// How far a download has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Still coming in.
    Receiving,
    Done,
    Failed(String),
    Cancelled,
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

/// A file name a server suggested, made safe to save under: no path in
/// it, no control characters, not empty, and not hidden.
pub fn safe_name(suggested: &str) -> String {
    let name: String = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name = name.trim().trim_start_matches('.').to_string();
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// A path in `dir` for `name` that `exists` says is free: the name, or the
/// name with " 2", " 3" and so on before its extension.
pub fn unique_path(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
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
    }
}

/// How far along, from 0 to 1, when the size is known.
pub fn fraction(download: &Download) -> Option<f64> {
    match download.total {
        Some(total) if total > 0 => Some((download.received as f64 / total as f64).clamp(0.0, 1.0)),
        _ => None,
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
}
