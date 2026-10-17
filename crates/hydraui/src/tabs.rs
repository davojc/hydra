//! The tab list (spec §7.1): tabs in strip order, grouped by set with Loose tabs last, each
//! holding a split tree and its focused pane.
use crate::split::{Axis, Dir, PaneId, Tree};

pub type TabId = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub id: TabId,
    /// The set that opened it; `None` is the Loose group.
    pub set: Option<String>,
    /// The set file's title for the tab; Loose tabs have none and show their pane's label.
    pub title: Option<String>,
    pub tree: Tree,
    pub focused: PaneId,
}

/// What closing a pane closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    NotFound,
    Pane,
    /// It was the tab's last pane.
    Tab(TabId),
}

#[derive(Debug, Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
    next_id: TabId,
}

impl Tabs {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Whether any open tab belongs to a set.
    pub fn has_sets(&self) -> bool {
        self.tabs.iter().any(|t| t.set.is_some())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active().map(|t| t.id)
    }

    /// Adds a one-pane Loose-style tab; see `add_tree`.
    pub fn add(&mut self, set: Option<String>, pane: PaneId) -> TabId {
        self.add_tree(set, None, Tree::Leaf(pane), pane)
    }

    /// Adds a tab after the last tab of its set (a new set goes before the Loose tabs; a
    /// Loose tab goes last) and makes it active.
    pub fn add_tree(
        &mut self,
        set: Option<String>,
        title: Option<String>,
        tree: Tree,
        focused: PaneId,
    ) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        let at = match &set {
            Some(name) => self
                .tabs
                .iter()
                .rposition(|t| t.set.as_ref() == Some(name))
                .map(|i| i + 1)
                .unwrap_or_else(|| {
                    self.tabs
                        .iter()
                        .position(|t| t.set.is_none())
                        .unwrap_or(self.tabs.len())
                }),
            None => self.tabs.len(),
        };
        self.tabs.insert(
            at,
            Tab {
                id,
                set,
                title,
                tree,
                focused,
            },
        );
        self.active = at;
        id
    }

    pub fn activate(&mut self, id: TabId) -> bool {
        match self.tabs.iter().position(|t| t.id == id) {
            Some(i) => {
                self.active = i;
                true
            }
            None => false,
        }
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    /// Splits the active tab's focused pane and focuses `new`. Returns the pane that was split.
    pub fn split_focused(&mut self, axis: Axis, new: PaneId) -> Option<PaneId> {
        let tab = self.active_mut()?;
        let old = tab.focused;
        if !tab.tree.split(old, axis, new) {
            return None;
        }
        tab.focused = new;
        Some(old)
    }

    /// Moves focus within the active tab; returns the newly focused pane.
    pub fn focus_dir(&mut self, dir: Dir) -> Option<PaneId> {
        let tab = self.active_mut()?;
        let next = tab.tree.neighbor(tab.focused, dir)?;
        tab.focused = next;
        Some(next)
    }

    /// Activates the pane's tab and focuses the pane.
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.tree.contains(pane)) else {
            return false;
        };
        self.active = i;
        self.tabs[i].focused = pane;
        true
    }

    pub fn tab_of(&self, pane: PaneId) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|t| t.tree.contains(pane))
            .map(|t| t.id)
    }

    /// Closes a pane; a tab's last pane closes the tab.
    pub fn close_pane(&mut self, pane: PaneId) -> Closed {
        let Some(i) = self.tabs.iter().position(|t| t.tree.contains(pane)) else {
            return Closed::NotFound;
        };
        let tab = &mut self.tabs[i];
        match tab.tree.remove(pane) {
            Some(next) => {
                if tab.focused == pane {
                    tab.focused = next;
                }
                Closed::Pane
            }
            None => {
                let id = tab.id;
                self.remove_at(i);
                Closed::Tab(id)
            }
        }
    }

    /// Closes a tab and returns its panes.
    pub fn close_tab(&mut self, id: TabId) -> Vec<PaneId> {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let panes = self.tabs[i].tree.panes();
        self.remove_at(i);
        panes
    }

    /// The tab to the right becomes active, or the one to the left if it was the last.
    fn remove_at(&mut self, i: usize) {
        self.tabs.remove(i);
        if i < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Tab ids by group, in strip order; `None` is the Loose group.
    pub fn groups(&self) -> Vec<(Option<String>, Vec<TabId>)> {
        let mut out: Vec<(Option<String>, Vec<TabId>)> = Vec::new();
        for t in &self.tabs {
            match out.last_mut() {
                Some((set, ids)) if *set == t.set => ids.push(t.id),
                _ => out.push((t.set.clone(), vec![t.id])),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(t: &Tabs) -> Vec<TabId> {
        t.iter().map(|tab| tab.id).collect()
    }

    #[test]
    fn has_sets_only_while_a_set_tab_is_open() {
        let mut t = Tabs::default();
        assert!(!t.has_sets());
        t.add(None, 1);
        assert!(!t.has_sets());
        let b = t.add(Some("client-a".into()), 2);
        assert!(t.has_sets());
        t.close_tab(b);
        assert!(!t.has_sets());
    }

    #[test]
    fn tabs_open_grouped_by_set_with_loose_last_and_become_active() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        let b = t.add(Some("client-a".into()), 2);
        let c = t.add(Some("client-a".into()), 3);
        let d = t.add(Some("personal".into()), 4);
        assert_eq!(ids(&t), vec![b, c, d, a]);
        assert_eq!(t.active_id(), Some(d));
        assert_eq!(
            t.groups(),
            vec![
                (Some("client-a".to_string()), vec![b, c]),
                (Some("personal".to_string()), vec![d]),
                (None, vec![a]),
            ]
        );
        assert_eq!(t.tab_of(3), Some(c));
    }

    #[test]
    fn next_and_previous_wrap_round() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.add(None, 2);
        let c = t.add(None, 3);
        t.next();
        assert_eq!(t.active_id(), Some(a));
        t.previous();
        assert_eq!(t.active_id(), Some(c));
        assert!(t.activate(a));
        assert_eq!(t.active_id(), Some(a));
        assert!(!t.activate(99));
    }

    #[test]
    fn split_and_move_focus() {
        let mut t = Tabs::default();
        t.add(None, 1);
        assert_eq!(t.split_focused(Axis::Row, 2), Some(1));
        assert_eq!(t.active().unwrap().focused, 2);
        assert_eq!(t.focus_dir(Dir::Left), Some(1));
        assert_eq!(t.focus_dir(Dir::Left), None);
        assert_eq!(t.active().unwrap().focused, 1);
    }

    #[test]
    fn focus_pane_activates_its_tab() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.split_focused(Axis::Row, 2);
        t.add(None, 3);
        assert!(t.focus_pane(1));
        assert_eq!(t.active_id(), Some(a));
        assert_eq!(t.active().unwrap().focused, 1);
        assert!(!t.focus_pane(9));
    }

    #[test]
    fn closing_a_pane_then_the_last_pane_of_a_tab() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.split_focused(Axis::Row, 2);
        let b = t.add(None, 3);
        assert_eq!(t.close_pane(2), Closed::Pane);
        assert_eq!(t.get(a).unwrap().focused, 1);
        assert_eq!(t.active_id(), Some(b));
        assert_eq!(t.close_pane(3), Closed::Tab(b));
        assert_eq!(t.active_id(), Some(a));
        assert_eq!(t.close_pane(9), Closed::NotFound);
    }

    #[test]
    fn closing_everything_leaves_a_usable_empty_list() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.split_focused(Axis::Column, 2);
        assert_eq!(t.close_tab(a), vec![1, 2]);
        assert!(t.is_empty());
        assert_eq!(t.active(), None);
        t.next();
        t.previous();
        assert_eq!(t.focus_dir(Dir::Up), None);
        assert_eq!(t.split_focused(Axis::Row, 5), None);
        assert_eq!(t.close_tab(a), Vec::<PaneId>::new());
        assert!(t.groups().is_empty());
        let b = t.add(None, 6);
        assert_eq!(t.active_id(), Some(b));
    }

    #[test]
    fn add_tree_keeps_the_layout_title_and_focus() {
        let mut t = Tabs::default();
        t.add(None, 1);
        let tree = Tree::Split {
            axis: Axis::Row,
            ratio: 0.6,
            first: Box::new(Tree::Leaf(2)),
            second: Box::new(Tree::Leaf(3)),
        };
        let id = t.add_tree(Some("client-a".into()), Some("api".into()), tree.clone(), 2);
        let tab = t.get(id).unwrap();
        assert_eq!(tab.tree, tree);
        assert_eq!(tab.focused, 2);
        assert_eq!(tab.title.as_deref(), Some("api"));
        assert_eq!(t.active_id(), Some(id));
        assert_eq!(
            t.iter().next().unwrap().id,
            id,
            "a set's tab goes before the Loose tabs"
        );
        assert_eq!(t.get(t.tab_of(1).unwrap()).unwrap().title, None);
    }
}
