//! What the sidebars show, and when they show it, in plain Rust: the rest
//! of the sidebar (`sidebar_macos`, `browser_macos`) is AppKit doing what
//! this decides.
//!
//! - **Rows only where they are seen.** Every tab is a window of its own,
//!   tabbed with the others by AppKit, and each window has a sidebar; only
//!   the window in front of its tab group shows one, so only that window's
//!   sidebar holds rows (`plan_group`). The others hold none. When another
//!   tab comes to the front, the sidebar that held the rows hands them to
//!   its sidebar, so a group of N tabs has N rows, made once, not N in each
//!   of N windows.
//! - **Updated in place.** A row is kept for its tab for as long as the tab
//!   is in the list: a refresh makes rows only for tabs that have none yet,
//!   and lets go of those whose tabs have gone (`diff_rows`). What a row
//!   shows is changed on it, not by making it again.
//! - **At most once a turn.** Whatever asks for a refresh only marks the
//!   group (`Pending`); the refresh runs once, at the end of the run loop's
//!   turn, for each group marked, however many times it was.

use crate::spaces::Layout;

/// A tab, as the sidebar's list needs it: `key` stands for its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tab<K> {
    pub key: K,
    pub pinned: bool,
    /// The space it is in, and its folder, if any.
    pub space: u64,
    pub folder: Option<u64>,
}

/// One line of the sidebar's list, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<K> {
    /// The current space's name, over its tabs.
    Header,
    /// A folder of the current space, by its id.
    Folder(u64),
    /// A tab's row; `in_folder` when it is shown under its folder, set in.
    Tab { key: K, in_folder: bool },
    /// The rule between the pinned tabs and the rest.
    Rule,
    /// New Tab, always last.
    NewTab,
}

/// The current space's tabs as the list shows them: its name; its folders,
/// each followed by its tabs unless it is folded; the pinned tabs in no
/// folder; a rule, when there are both pinned and other tabs; the rest, in
/// the order their windows are in; and New Tab.
pub fn items<K: Copy>(tabs: &[Tab<K>], layout: &Layout) -> Vec<Item<K>> {
    let space = layout.current_space().id;
    let here: Vec<&Tab<K>> = tabs.iter().filter(|t| t.space == space).collect();
    let folders = layout.folders_of(space);
    let in_folder = |tab: &Tab<K>| {
        tab.pinned
            && tab
                .folder
                .is_some_and(|id| folders.iter().any(|f| f.id == id))
    };
    let mut items = vec![Item::Header];
    for folder in &folders {
        items.push(Item::Folder(folder.id));
        if folder.collapsed {
            continue;
        }
        for tab in here
            .iter()
            .filter(|t| t.pinned && t.folder == Some(folder.id))
        {
            items.push(Item::Tab {
                key: tab.key,
                in_folder: true,
            });
        }
    }
    let loose: Vec<&&Tab<K>> = here.iter().filter(|t| t.pinned && !in_folder(t)).collect();
    for tab in &loose {
        items.push(Item::Tab {
            key: tab.key,
            in_folder: false,
        });
    }
    let unpinned: Vec<&&Tab<K>> = here.iter().filter(|t| !t.pinned).collect();
    if (!folders.is_empty() || !loose.is_empty()) && !unpinned.is_empty() {
        items.push(Item::Rule);
    }
    for tab in &unpinned {
        items.push(Item::Tab {
            key: tab.key,
            in_folder: false,
        });
    }
    items.push(Item::NewTab);
    items
}

/// The tabs `items` has a row for, in order.
pub fn row_keys<K: Copy>(items: &[Item<K>]) -> Vec<K> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Tab { key, .. } => Some(*key),
            _ => None,
        })
        .collect()
}

/// What a refresh of one tab group does with its windows' sidebars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPlan<K> {
    /// The window in front, whose sidebar shows the group's tabs; none when
    /// the group has no window in front that can show them.
    pub front: Option<K>,
    /// Another of the group's windows whose sidebar holds rows already,
    /// from when it was in front: they are handed to `front`'s sidebar
    /// rather than made again.
    pub from: Option<K>,
}

impl<K: Copy + PartialEq> GroupPlan<K> {
    /// Whether `window`'s sidebar shows the group's rows: only the front
    /// window's does. Every other window's holds none.
    pub fn shows(&self, window: K) -> bool {
        self.front == Some(window)
    }
}

/// The plan for a group of `windows`, of which `front` is in front, and
/// `holds` says whose sidebars hold rows now.
pub fn plan_group<K: Copy + PartialEq>(
    windows: &[K],
    front: Option<K>,
    holds: impl Fn(K) -> bool,
) -> GroupPlan<K> {
    let front = front.filter(|f| windows.contains(f));
    let from = match front {
        Some(f) if !holds(f) => windows.iter().copied().find(|w| *w != f && holds(*w)),
        _ => None,
    };
    GroupPlan { front, from }
}

/// The rows a sidebar makes and lets go of, to go from rows for `have` to
/// rows for `want`: rows are kept for the tabs in both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDiff<K> {
    pub make: Vec<K>,
    pub drop: Vec<K>,
}

pub fn diff_rows<K: Copy + PartialEq>(have: &[K], want: &[K]) -> RowDiff<K> {
    RowDiff {
        make: want.iter().copied().filter(|k| !have.contains(k)).collect(),
        drop: have.iter().copied().filter(|k| !want.contains(k)).collect(),
    }
}

/// The groups waiting for their refresh: each once, however often it was
/// asked for, in the order first asked; and whether a refresh is already
/// scheduled to take them.
#[derive(Debug)]
pub struct Pending<K, T> {
    waiting: Vec<(K, T)>,
    scheduled: bool,
}

impl<K: PartialEq, T> Default for Pending<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: PartialEq, T> Pending<K, T> {
    pub const fn new() -> Self {
        Self {
            waiting: Vec::new(),
            scheduled: false,
        }
    }

    /// Ask for `key`'s refresh, with `item` to do it from. Whether a refresh
    /// has to be scheduled now: only when none is yet.
    pub fn mark(&mut self, key: K, item: T) -> bool {
        if !self.waiting.iter().any(|(k, _)| *k == key) {
            self.waiting.push((key, item));
        }
        !std::mem::replace(&mut self.scheduled, true)
    }

    /// What is waiting, each once; anything asked for after this is
    /// scheduled anew.
    pub fn take(&mut self) -> Vec<T> {
        self.scheduled = false;
        self.waiting.drain(..).map(|(_, item)| item).collect()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }
}

/// `items`, each `key` once, the first of each kept.
pub fn unique_by<K: PartialEq, T>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut seen: Vec<K> = Vec::with_capacity(items.len());
    let mut kept = Vec::with_capacity(items.len());
    for item in items {
        let k = key(&item);
        if !seen.contains(&k) {
            seen.push(k);
            kept.push(item);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::{Folder, Layout, Space};
    use std::collections::HashMap;

    fn layout() -> Layout {
        let mut layout = Layout::default();
        layout.spaces = vec![
            Space {
                id: 1,
                name: "Home".into(),
                hue: 0,
                icon: "🏠".into(),
            },
            Space {
                id: 2,
                name: "Work".into(),
                hue: 1,
                icon: "💼".into(),
            },
        ];
        layout.folders = vec![];
        layout.current = 1;
        layout
    }

    fn tab(key: u32, pinned: bool) -> Tab<u32> {
        Tab {
            key,
            pinned,
            space: 1,
            folder: None,
        }
    }

    #[test]
    fn the_list_is_the_spaces_folders_pinned_tabs_a_rule_the_rest_and_new_tab() {
        let mut layout = layout();
        layout.folders = vec![
            Folder {
                id: 7,
                name: "Read".into(),
                space: 1,
                collapsed: false,
            },
            Folder {
                id: 8,
                name: "Shut".into(),
                space: 1,
                collapsed: true,
            },
            Folder {
                id: 9,
                name: "Elsewhere".into(),
                space: 2,
                collapsed: false,
            },
        ];
        let tabs = vec![
            tab(1, false),
            Tab {
                folder: Some(7),
                ..tab(2, true)
            },
            tab(3, true),
            Tab {
                folder: Some(8),
                ..tab(4, true)
            },
            Tab {
                space: 2,
                ..tab(5, false)
            },
            tab(6, false),
        ];
        assert_eq!(
            items(&tabs, &layout),
            vec![
                Item::Header,
                Item::Folder(7),
                Item::Tab {
                    key: 2,
                    in_folder: true
                },
                Item::Folder(8),
                Item::Tab {
                    key: 3,
                    in_folder: false
                },
                Item::Rule,
                Item::Tab {
                    key: 1,
                    in_folder: false
                },
                Item::Tab {
                    key: 6,
                    in_folder: false
                },
                Item::NewTab,
            ]
        );
    }

    #[test]
    fn no_rule_without_pinned_tabs_and_new_tab_alone_without_tabs() {
        let layout = layout();
        let tabs = vec![tab(1, false), tab(2, false)];
        assert_eq!(row_keys(&items(&tabs, &layout)), vec![1, 2]);
        assert!(!items(&tabs, &layout).contains(&Item::Rule));
        assert_eq!(items::<u32>(&[], &layout), vec![Item::Header, Item::NewTab]);
    }

    #[test]
    fn a_pinned_tab_in_a_folder_of_another_space_is_loose() {
        let mut layout = layout();
        layout.folders = vec![Folder {
            id: 9,
            name: "Elsewhere".into(),
            space: 2,
            collapsed: false,
        }];
        let tabs = vec![Tab {
            folder: Some(9),
            ..tab(1, true)
        }];
        assert_eq!(
            items(&tabs, &layout),
            vec![
                Item::Header,
                Item::Tab {
                    key: 1,
                    in_folder: false
                },
                Item::NewTab
            ]
        );
    }

    #[test]
    fn only_the_front_window_shows_rows() {
        let windows = [1, 2, 3, 4];
        let plan = plan_group(&windows, Some(3), |_| false);
        assert_eq!(
            windows.iter().filter(|w| plan.shows(**w)).count(),
            1,
            "one window of a group shows its rows"
        );
        assert!(plan.shows(3));
        assert_eq!(plan.from, None);
        // A front that is not one of the group's windows shows nothing.
        let plan = plan_group(&windows, Some(9), |_| true);
        assert!(windows.iter().all(|w| !plan.shows(*w)));
        assert_eq!(plan.from, None);
    }

    #[test]
    fn the_rows_are_handed_to_the_new_front_window() {
        let windows = [1, 2, 3];
        assert_eq!(
            plan_group(&windows, Some(2), |w| w == 1),
            GroupPlan {
                front: Some(2),
                from: Some(1)
            }
        );
        // The front window's own rows are kept where they are.
        assert_eq!(plan_group(&windows, Some(2), |w| w != 3).from, None);
    }

    #[test]
    fn rows_are_made_only_for_new_tabs_and_let_go_only_for_gone_ones() {
        assert_eq!(
            diff_rows(&[1, 2, 3], &[3, 1, 4]),
            RowDiff {
                make: vec![4],
                drop: vec![2]
            }
        );
        assert_eq!(
            diff_rows(&[1, 2], &[2, 1]),
            RowDiff {
                make: vec![],
                drop: vec![]
            }
        );
    }

    /// The sidebars of every window, as the application keeps them: the
    /// rows each holds, by tab. A refresh of a group does what
    /// `browser_macos::refresh_group` does with the plan; it says how many
    /// rows it made.
    #[derive(Default)]
    struct Sidebars {
        rows: HashMap<u32, Vec<u32>>,
    }

    impl Sidebars {
        fn refresh(&mut self, windows: &[u32], front: u32, layout: &Layout) -> usize {
            let tabs: Vec<Tab<u32>> = windows.iter().map(|w| tab(*w, false)).collect();
            let want = row_keys(&items(&tabs, layout));
            let plan = plan_group(windows, Some(front), |w| {
                self.rows.get(&w).is_some_and(|r| !r.is_empty())
            });
            if let (Some(to), Some(from)) = (plan.front, plan.from) {
                let rows = self.rows.remove(&from).unwrap_or_default();
                self.rows.insert(to, rows);
            }
            let mut made = 0;
            for w in windows {
                let held = self.rows.entry(*w).or_default();
                if plan.shows(*w) {
                    let diff = diff_rows(held, &want);
                    made += diff.make.len();
                    *held = want.clone();
                } else {
                    held.clear();
                }
            }
            made
        }

        fn held(&self) -> usize {
            self.rows.values().map(Vec::len).sum()
        }
    }

    /// The regression this module exists for: a group of N tabs holds N
    /// rows, not N in each of N windows (N², 900 for 30 tabs), and bringing
    /// each tab to the front in turn makes none.
    #[test]
    fn a_group_of_n_tabs_holds_n_rows_not_n_squared() {
        let layout = layout();
        let n = 30;
        let mut sidebars = Sidebars::default();
        let mut windows = Vec::new();
        let mut made = Vec::new();
        for w in 1..=n {
            windows.push(w);
            // A new tab opens in front.
            made.push(sidebars.refresh(&windows, w, &layout));
            assert_eq!(sidebars.held(), windows.len(), "with {w} tabs");
        }
        assert_eq!(
            made,
            vec![1; n as usize],
            "one row made for each tab opened"
        );
        // Each tab brought to the front in turn, and refreshed again and
        // again as its page loads: no row made, and still N.
        for front in &windows {
            for _ in 0..4 {
                assert_eq!(sidebars.refresh(&windows, *front, &layout), 0);
            }
            assert_eq!(sidebars.held(), n as usize);
        }
        // A tab closed: one row fewer, none made.
        windows.retain(|w| *w != 7);
        assert_eq!(sidebars.refresh(&windows, 8, &layout), 0);
        assert_eq!(sidebars.held(), n as usize - 1);
    }

    #[test]
    fn a_group_refresh_is_asked_for_many_times_and_runs_once() {
        let mut pending: Pending<usize, &str> = Pending::new();
        assert!(pending.mark(1, "a"), "the first ask schedules a refresh");
        for _ in 0..1000 {
            assert!(!pending.mark(1, "a"), "later asks do not");
        }
        assert!(!pending.mark(2, "b"));
        assert_eq!(pending.take(), vec!["a", "b"]);
        assert!(pending.is_empty());
        assert!(
            pending.mark(1, "a"),
            "after the refresh, the next ask schedules another"
        );
    }

    #[test]
    fn windows_are_refreshed_once_per_group() {
        // Ten windows closing at once each ask for their group: two groups.
        let asked: Vec<(u32, &str)> = (0..10)
            .map(|i| (i, if i % 3 == 0 { "group A" } else { "group B" }))
            .collect();
        let groups = unique_by(asked, |(_, group)| *group);
        assert_eq!(groups, vec![(0, "group A"), (1, "group B")]);
    }
}
