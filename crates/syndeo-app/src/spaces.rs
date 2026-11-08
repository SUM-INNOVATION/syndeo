//! Spaces and folders, as Arc arranges a sidebar: a space is a named set of
//! tabs with a colour of its own, one of which is current, and a folder
//! is a named group of pinned tabs within a space. Plain Rust: what is
//! kept, as JSON, and nothing of how it is shown (`sidebar_macos`) or
//! stored (`spaces_macos`, in the application's defaults). Favorites are
//! not a space's: they are shared by all of them (`favorites`).

use serde_json::{json, Value};

/// The colours a space can have: see [`hue_colors`].
pub const HUES: usize = 8;

/// The most spaces, and folders, kept: a sidebar's worth.
pub const MOST_SPACES: usize = 12;
pub const MOST_FOLDERS: usize = 50;

/// The space every tab starts in, when nothing says otherwise.
pub const FIRST_SPACE: u64 = 1;

/// The emoji a space is given when none is chosen, by its place.
pub const DEFAULT_ICONS: [&str; 8] = ["🏠", "💼", "🎨", "📚", "🎧", "🧪", "✈️", "🛒"];

/// The default emoji for the space at `index`.
pub fn default_icon(index: usize) -> &'static str {
    DEFAULT_ICONS[index % DEFAULT_ICONS.len()]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Space {
    pub id: u64,
    pub name: String,
    /// Which of the [`HUES`].
    pub hue: usize,
    /// An emoji, or nothing: shown in the switcher in place of its dot.
    pub icon: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: u64,
    pub name: String,
    /// The space it is in.
    pub space: u64,
    /// Folded shut, its tabs out of sight.
    pub collapsed: bool,
}

/// The spaces and folders, which is current, and the next id to give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub spaces: Vec<Space>,
    pub folders: Vec<Folder>,
    pub current: u64,
    next_id: u64,
}

impl Default for Layout {
    /// One space, Personal, in the first colour.
    fn default() -> Self {
        Self {
            spaces: vec![Space {
                id: FIRST_SPACE,
                name: "Personal".to_string(),
                hue: 0,
                icon: default_icon(0).to_string(),
            }],
            folders: Vec::new(),
            current: FIRST_SPACE,
            next_id: FIRST_SPACE + 1,
        }
    }
}

impl Layout {
    /// The layout in `json`, as `render` wrote it; the default for anything
    /// else, or for one with no spaces.
    pub fn parse(json: &str) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(json) else {
            return Self::default();
        };
        let spaces: Vec<Space> = value
            .get("spaces")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        Some(Space {
                            id: item.get("id")?.as_u64()?,
                            name: item.get("name")?.as_str()?.to_string(),
                            hue: item.get("hue").and_then(Value::as_u64).unwrap_or(0) as usize
                                % HUES,
                            icon: icon_or_default(
                                item.get("icon").and_then(Value::as_str).unwrap_or(""),
                                index,
                            ),
                        })
                    })
                    .take(MOST_SPACES)
                    .collect()
            })
            .unwrap_or_default();
        if spaces.is_empty() {
            return Self::default();
        }
        let folders: Vec<Folder> = value
            .get("folders")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let space = item.get("space")?.as_u64()?;
                        spaces.iter().any(|s| s.id == space).then_some(Folder {
                            id: item.get("id")?.as_u64()?,
                            name: item.get("name")?.as_str()?.to_string(),
                            space,
                            collapsed: item
                                .get("collapsed")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .take(MOST_FOLDERS)
                    .collect()
            })
            .unwrap_or_default();
        let current = value
            .get("current")
            .and_then(Value::as_u64)
            .filter(|id| spaces.iter().any(|s| s.id == *id))
            .unwrap_or(spaces[0].id);
        let used = spaces
            .iter()
            .map(|s| s.id)
            .chain(folders.iter().map(|f| f.id))
            .max()
            .unwrap_or(FIRST_SPACE);
        let next_id = value
            .get("next_id")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .max(used + 1);
        Self {
            spaces,
            folders,
            current,
            next_id,
        }
    }

    /// The layout, as JSON for `parse`.
    pub fn render(&self) -> String {
        json!({
            "spaces": self.spaces.iter().map(|s| json!({"id": s.id, "name": s.name, "hue": s.hue, "icon": s.icon})).collect::<Vec<_>>(),
            "folders": self.folders.iter().map(|f| json!({"id": f.id, "name": f.name, "space": f.space, "collapsed": f.collapsed})).collect::<Vec<_>>(),
            "current": self.current,
            "next_id": self.next_id,
        })
        .to_string()
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn space(&self, id: u64) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == id)
    }

    /// The current space; there is always one.
    pub fn current_space(&self) -> &Space {
        self.space(self.current).unwrap_or(&self.spaces[0])
    }

    /// Whether `id` names a space.
    pub fn has_space(&self, id: u64) -> bool {
        self.space(id).is_some()
    }

    /// A new space, after the others, in the next colour along, made
    /// current. Its id, or none past [`MOST_SPACES`].
    pub fn add_space(&mut self, name: &str, icon: &str) -> Option<u64> {
        if self.spaces.len() >= MOST_SPACES {
            return None;
        }
        let hue = self.spaces.last().map_or(0, |last| (last.hue + 1) % HUES);
        let id = self.take_id();
        let name = if name.trim().is_empty() {
            format!("Space {}", self.spaces.len() + 1)
        } else {
            name.trim().to_string()
        };
        self.spaces.push(Space {
            id,
            name,
            hue,
            icon: icon_or_default(icon, self.spaces.len()),
        });
        self.current = id;
        Some(id)
    }

    /// Take a space away, with its folders; not the last one. The current
    /// space, taken away, gives way to the first. Whether it was taken.
    pub fn remove_space(&mut self, id: u64) -> bool {
        if self.spaces.len() <= 1 || !self.has_space(id) {
            return false;
        }
        self.spaces.retain(|s| s.id != id);
        self.folders.retain(|f| f.space != id);
        if self.current == id {
            self.current = self.spaces[0].id;
        }
        true
    }

    pub fn rename_space(&mut self, id: u64, name: &str) -> bool {
        let trimmed = name.trim();
        match self.spaces.iter_mut().find(|s| s.id == id) {
            Some(space) if !trimmed.is_empty() => {
                space.name = trimmed.to_string();
                true
            }
            _ => false,
        }
    }

    /// A space's emoji. Nothing typed changes nothing: every space has one.
    pub fn set_icon(&mut self, id: u64, icon: &str) -> bool {
        let icon = icon_of(icon);
        match self.spaces.iter_mut().find(|s| s.id == id) {
            Some(space) if !icon.is_empty() => {
                space.icon = icon;
                true
            }
            _ => false,
        }
    }

    pub fn set_hue(&mut self, id: u64, hue: usize) -> bool {
        match self.spaces.iter_mut().find(|s| s.id == id) {
            Some(space) if hue < HUES => {
                space.hue = hue;
                true
            }
            _ => false,
        }
    }

    /// Make `id` current. Whether it is a space.
    pub fn switch_to(&mut self, id: u64) -> bool {
        let is = self.has_space(id);
        if is {
            self.current = id;
        }
        is
    }

    /// The space after the current one, round again at the end.
    pub fn next_space(&self) -> u64 {
        let at = self
            .spaces
            .iter()
            .position(|s| s.id == self.current)
            .unwrap_or(0);
        self.spaces[(at + 1) % self.spaces.len()].id
    }

    /// The space before the current one, round again at the start.
    pub fn previous_space(&self) -> u64 {
        let at = self
            .spaces
            .iter()
            .position(|s| s.id == self.current)
            .unwrap_or(0);
        self.spaces[(at + self.spaces.len() - 1) % self.spaces.len()].id
    }

    /// The folders of `space`, in order.
    pub fn folders_of(&self, space: u64) -> Vec<&Folder> {
        self.folders.iter().filter(|f| f.space == space).collect()
    }

    pub fn folder(&self, id: u64) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// A new folder in `space`, after the others. Its id, or none for a
    /// space that is not one, or past [`MOST_FOLDERS`].
    pub fn add_folder(&mut self, space: u64, name: &str) -> Option<u64> {
        if !self.has_space(space) || self.folders.len() >= MOST_FOLDERS {
            return None;
        }
        let id = self.take_id();
        let name = if name.trim().is_empty() {
            format!("Folder {}", self.folders_of(space).len() + 1)
        } else {
            name.trim().to_string()
        };
        self.folders.push(Folder {
            id,
            name,
            space,
            collapsed: false,
        });
        Some(id)
    }

    pub fn remove_folder(&mut self, id: u64) -> bool {
        let before = self.folders.len();
        self.folders.retain(|f| f.id != id);
        self.folders.len() != before
    }

    pub fn rename_folder(&mut self, id: u64, name: &str) -> bool {
        let trimmed = name.trim();
        match self.folders.iter_mut().find(|f| f.id == id) {
            Some(folder) if !trimmed.is_empty() => {
                folder.name = trimmed.to_string();
                true
            }
            _ => false,
        }
    }

    /// Fold a folder shut, or open it again.
    pub fn toggle_folder(&mut self, id: u64) -> bool {
        match self.folders.iter_mut().find(|f| f.id == id) {
            Some(folder) => {
                folder.collapsed = !folder.collapsed;
                true
            }
            None => false,
        }
    }
}

/// A space's icon from what was typed: its first character, trimmed, so
/// that an emoji and nothing else is kept; or nothing.
pub fn icon_of(typed: &str) -> String {
    typed
        .trim()
        .chars()
        .next()
        .filter(|c| !c.is_ascii() && !c.is_control())
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// `icon_of`, or the default for the space at `index` when nothing was
/// typed.
fn icon_or_default(typed: &str, index: usize) -> String {
    let icon = icon_of(typed);
    if icon.is_empty() {
        default_icon(index).to_string()
    } else {
        icon
    }
}

/// A space's colours: the gradient's two ends, in light appearance and in
/// dark, as sRGB. The first is the lavender into blush the window had
/// before there were spaces.
pub fn hue_colors(hue: usize, dark: bool) -> ([f64; 3], [f64; 3]) {
    const LIGHT: [([f64; 3], [f64; 3]); HUES] = [
        ([0.851, 0.831, 0.969], [0.969, 0.851, 0.890]),
        ([0.808, 0.882, 0.980], [0.878, 0.949, 0.980]),
        ([0.800, 0.941, 0.882], [0.898, 0.969, 0.851]),
        ([0.992, 0.878, 0.800], [0.992, 0.929, 0.851]),
        ([0.980, 0.831, 0.871], [0.969, 0.890, 0.941]),
        ([0.961, 0.925, 0.812], [0.980, 0.961, 0.890]),
        ([0.851, 0.871, 0.910], [0.910, 0.922, 0.941]),
        ([0.890, 0.851, 0.969], [0.949, 0.890, 0.980]),
    ];
    let (from, to) = LIGHT[hue % HUES];
    if !dark {
        return (from, to);
    }
    let deepen = |c: [f64; 3]| -> [f64; 3] {
        let mut out = [0.0; 3];
        for (i, channel) in c.iter().enumerate() {
            out[i] = (channel - 0.6).max(0.0) * 0.4 + 0.12;
        }
        out
    };
    (deepen(from), deepen(to))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_one_space_called_personal() {
        let layout = Layout::default();
        assert_eq!(layout.spaces.len(), 1);
        assert_eq!(layout.current_space().name, "Personal");
        assert_eq!(layout.current, FIRST_SPACE);
    }

    #[test]
    fn a_round_trip_keeps_everything() {
        let mut layout = Layout::default();
        let work = layout.add_space("Work", "").unwrap();
        let folder = layout.add_folder(work, "Reading").unwrap();
        assert!(layout.toggle_folder(folder));
        assert!(layout.set_hue(work, 3));
        let read = Layout::parse(&layout.render());
        assert_eq!(read, layout);
        assert_eq!(read.current, work);
        assert!(read.folder(folder).unwrap().collapsed);
    }

    #[test]
    fn ids_are_never_given_twice() {
        let mut layout = Layout::default();
        let a = layout.add_space("A", "").unwrap();
        assert!(layout.remove_space(a));
        let b = layout.add_space("B", "").unwrap();
        assert_ne!(a, b);
        let read = Layout::parse(&layout.render());
        let mut again = read.clone();
        let c = again.add_space("C", "").unwrap();
        assert!(c > b);
    }

    #[test]
    fn the_last_space_stays_and_its_folders_go_with_it() {
        let mut layout = Layout::default();
        assert!(!layout.remove_space(FIRST_SPACE));
        let work = layout.add_space("Work", "").unwrap();
        layout.add_folder(work, "Reading");
        assert_eq!(layout.folders_of(work).len(), 1);
        assert!(layout.remove_space(work));
        assert!(layout.folders.is_empty());
        assert_eq!(layout.current, FIRST_SPACE);
    }

    #[test]
    fn next_and_previous_go_round() {
        let mut layout = Layout::default();
        let work = layout.add_space("Work", "").unwrap();
        let play = layout.add_space("Play", "").unwrap();
        assert_eq!(layout.current, play);
        assert_eq!(layout.next_space(), FIRST_SPACE);
        assert_eq!(layout.previous_space(), work);
        assert!(layout.switch_to(FIRST_SPACE));
        assert_eq!(layout.previous_space(), play);
        assert!(!layout.switch_to(999));
    }

    #[test]
    fn names_are_trimmed_and_never_empty() {
        let mut layout = Layout::default();
        let id = layout.add_space("   ", "").unwrap();
        assert_eq!(layout.space(id).unwrap().name, "Space 2");
        assert!(!layout.rename_space(id, " "));
        assert!(layout.rename_space(id, " Work "));
        assert_eq!(layout.space(id).unwrap().name, "Work");
        let folder = layout.add_folder(id, "").unwrap();
        assert_eq!(layout.folder(folder).unwrap().name, "Folder 1");
    }

    #[test]
    fn anything_else_parses_to_the_default() {
        assert_eq!(Layout::parse(""), Layout::default());
        assert_eq!(Layout::parse("{\"spaces\":[]}"), Layout::default());
        let orphan = "{\"spaces\":[{\"id\":1,\"name\":\"P\"}],\"folders\":[{\"id\":2,\"name\":\"F\",\"space\":9}],\"current\":9}";
        let read = Layout::parse(orphan);
        assert!(read.folders.is_empty());
        assert_eq!(read.current, 1);
    }

    #[test]
    fn every_space_has_one_emoji() {
        assert_eq!(icon_of(" 🚀 "), "🚀");
        assert_eq!(icon_of("abc"), "");
        assert_eq!(icon_of(""), "");
        let mut layout = Layout::default();
        assert_eq!(layout.current_space().icon, default_icon(0));
        let id = layout.add_space("Rockets", "🚀🚀").unwrap();
        assert_eq!(layout.space(id).unwrap().icon, "🚀");
        let plain = layout.add_space("Plain", "").unwrap();
        assert_eq!(layout.space(plain).unwrap().icon, default_icon(2));
        assert!(!layout.set_icon(id, ""));
        assert!(layout.set_icon(id, "🛰"));
        assert_eq!(Layout::parse(&layout.render()).space(id).unwrap().icon, "🛰");
        // A layout kept before every space had an emoji gets the defaults.
        let old =
            "{\"spaces\":[{\"id\":1,\"name\":\"P\"},{\"id\":2,\"name\":\"W\"}],\"current\":2}";
        let read = Layout::parse(old);
        assert_eq!(read.spaces[0].icon, default_icon(0));
        assert_eq!(read.spaces[1].icon, default_icon(1));
    }

    #[test]
    fn every_hue_has_colours_in_range() {
        for hue in 0..HUES {
            for dark in [false, true] {
                let (from, to) = hue_colors(hue, dark);
                for c in from.iter().chain(to.iter()) {
                    assert!((0.0..=1.0).contains(c));
                }
            }
        }
    }
}
