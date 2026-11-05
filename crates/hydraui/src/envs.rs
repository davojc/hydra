//! The window model (spec: docs/superpowers/specs/2026-10-19-hydraui-environment-tabs-design.md
//! §2): one tab per environment, each with its terminals in creation order, the one shown, and
//! each terminal's file pane, shown beside it.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Identifies a terminal or file pane for the life of the window.
pub type PaneId = u64;

/// The file pane's share of the middle is dragged within these bounds.
pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;

/// The terminal's share of the middle while the file pane shows.
pub const DEFAULT_FILES_RATIO: f32 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct EnvTab {
    pub env: String,
    /// In creation order.
    pub terminals: Vec<PaneId>,
    pub shown: Option<PaneId>,
    /// Each terminal's file pane, shown beside it: terminal → file pane.
    pub files: HashMap<PaneId, PaneId>,
    /// Whether keys go to the shown terminal's file pane rather than the terminal.
    pub files_focused: bool,
    /// Each terminal's share of the middle while its file pane shows; unset is the default.
    ratios: HashMap<PaneId, f32>,
}

impl EnvTab {
    /// The shown terminal's file pane.
    pub fn shown_files(&self) -> Option<PaneId> {
        self.files.get(&self.shown?).copied()
    }

    /// The shown terminal's share of the middle while its file pane shows.
    pub fn files_ratio(&self) -> f32 {
        self.shown
            .and_then(|s| self.ratios.get(&s).copied())
            .unwrap_or(DEFAULT_FILES_RATIO)
    }

    /// Sets the shown terminal's share, kept within [`MIN_RATIO`] and [`MAX_RATIO`].
    pub fn set_files_ratio(&mut self, ratio: f32) {
        if let Some(s) = self.shown {
            self.ratios.insert(s, ratio.clamp(MIN_RATIO, MAX_RATIO));
        }
    }
}

#[derive(Debug, Default)]
pub struct EnvTabs {
    tabs: Vec<EnvTab>,
    active: usize,
}

impl EnvTabs {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &EnvTab> {
        self.tabs.iter()
    }

    pub fn get(&self, env: &str) -> Option<&EnvTab> {
        self.tabs.iter().find(|t| t.env == env)
    }

    pub fn get_mut(&mut self, env: &str) -> Option<&mut EnvTab> {
        self.tabs.iter_mut().find(|t| t.env == env)
    }

    pub fn active(&self) -> Option<&EnvTab> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut EnvTab> {
        self.tabs.get_mut(self.active)
    }

    /// Appends a tab for `env` (none yet) and activates it; true when new.
    pub fn add_env(&mut self, env: &str) -> bool {
        if self.activate(env) {
            return false;
        }
        self.tabs.push(EnvTab {
            env: env.to_string(),
            terminals: Vec::new(),
            shown: None,
            files: HashMap::new(),
            files_focused: false,
            ratios: HashMap::new(),
        });
        self.active = self.tabs.len() - 1;
        true
    }

    pub fn activate(&mut self, env: &str) -> bool {
        match self.tabs.iter().position(|t| t.env == env) {
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

    /// Appends `id` to `env`'s terminals, shows it, activates `env`; false without that env.
    pub fn add_terminal(&mut self, env: &str, id: PaneId) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.env == env) else {
            return false;
        };
        let tab = &mut self.tabs[i];
        tab.terminals.push(id);
        tab.shown = Some(id);
        tab.files_focused = false;
        self.active = i;
        true
    }

    /// Appends `id` to `env`'s terminals without changing what is shown or which env is
    /// active (a stage terminal opens beside the user, orchestrator spec §3.2); a tab showing
    /// nothing shows it. False without that env.
    pub fn add_terminal_quietly(&mut self, env: &str, id: PaneId) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.env == env) else {
            return false;
        };
        tab.terminals.push(id);
        tab.shown.get_or_insert(id);
        true
    }

    /// Activates `id`'s env and shows it (`files_focused = false`).
    pub fn show(&mut self, id: PaneId) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.terminals.contains(&id)) else {
            return false;
        };
        let tab = &mut self.tabs[i];
        tab.shown = Some(id);
        tab.files_focused = false;
        self.active = i;
        true
    }

    /// Shows the next terminal of the active env, wrapping.
    pub fn next_terminal(&mut self) -> Option<PaneId> {
        self.step_terminal(1)
    }

    /// Shows the previous terminal of the active env, wrapping.
    pub fn previous_terminal(&mut self) -> Option<PaneId> {
        self.step_terminal(-1)
    }

    fn step_terminal(&mut self, by: isize) -> Option<PaneId> {
        let tab = self.tabs.get_mut(self.active)?;
        let len = tab.terminals.len() as isize;
        let at = tab
            .shown
            .and_then(|s| tab.terminals.iter().position(|&t| t == s))
            .unwrap_or(0) as isize;
        let id = *tab
            .terminals
            .get((at + by).rem_euclid(len.max(1)) as usize)?;
        tab.shown = Some(id);
        tab.files_focused = false;
        Some(id)
    }

    /// The env holding `id` as a terminal or its file pane.
    pub fn env_of(&self, id: PaneId) -> Option<&str> {
        self.tabs
            .iter()
            .find(|t| t.terminals.contains(&id) || t.files.values().any(|&f| f == id))
            .map(|t| t.env.as_str())
    }

    /// Removes a terminal; a shown one gives way to the next in the list, else the previous.
    pub fn close_terminal(&mut self, id: PaneId) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.terminals.contains(&id)) else {
            return false;
        };
        let at = tab.terminals.iter().position(|&t| t == id).unwrap_or(0);
        tab.terminals.remove(at);
        tab.files.remove(&id);
        tab.ratios.remove(&id);
        if tab.shown == Some(id) {
            tab.shown = tab
                .terminals
                .get(at)
                .or_else(|| tab.terminals.last())
                .copied();
        }
        true
    }

    /// Removes the tab; returns its terminals then their file panes, in the terminals' order.
    /// The tab to the right becomes active, else the left.
    pub fn close_env(&mut self, env: &str) -> Vec<PaneId> {
        let Some(i) = self.tabs.iter().position(|t| t.env == env) else {
            return Vec::new();
        };
        let tab = self.tabs.remove(i);
        if i < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        let files = tab
            .terminals
            .iter()
            .filter_map(|t| tab.files.get(t).copied());
        let files: Vec<PaneId> = files.collect();
        tab.terminals.into_iter().chain(files).collect()
    }

    /// Sets `terminal`'s file pane; a shown terminal without one can't give its file pane focus.
    pub fn set_files(&mut self, terminal: PaneId, files: Option<PaneId>) {
        let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|t| t.terminals.contains(&terminal))
        else {
            return;
        };
        match files {
            Some(f) => {
                tab.files.insert(terminal, f);
            }
            None => {
                tab.files.remove(&terminal);
                if tab.shown == Some(terminal) {
                    tab.files_focused = false;
                }
            }
        }
    }

    /// `terminal`'s file pane.
    pub fn files_of(&self, terminal: PaneId) -> Option<PaneId> {
        self.tabs
            .iter()
            .find_map(|t| t.files.get(&terminal).copied())
    }

    /// Forgets the file pane `files`, whichever terminal it was beside.
    pub fn drop_files(&mut self, files: PaneId) {
        let terminal = self
            .tabs
            .iter()
            .find_map(|t| t.files.iter().find(|&(_, &f)| f == files).map(|(&t, _)| t));
        if let Some(terminal) = terminal {
            self.set_files(terminal, None);
        }
    }
}

/// Where a new terminal starts: the environment's root folder (`Hydra::env_home`), else the shown
/// terminal's folder, else the user's home (spec §2.2).
pub fn new_terminal_cwd(
    shown: Option<&Path>,
    env_home: Option<&Path>,
    user_home: &Path,
) -> PathBuf {
    env_home.or(shown).unwrap_or(user_home).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs_with(envs: &[&str]) -> EnvTabs {
        let mut t = EnvTabs::default();
        for e in envs {
            t.add_env(e);
        }
        t
    }

    fn envs(t: &EnvTabs) -> Vec<&str> {
        t.iter().map(|e| e.env.as_str()).collect()
    }

    #[test]
    fn adding_an_env_twice_only_activates_it() {
        let mut t = tabs_with(&["a", "b"]);
        assert!(!t.add_env("a"));
        assert_eq!(envs(&t), vec!["a", "b"]);
        assert_eq!(t.active().unwrap().env, "a");
        assert!(t.add_env("c"));
        assert_eq!(t.active().unwrap().env, "c");
        assert_eq!(t.active().unwrap().files_ratio(), DEFAULT_FILES_RATIO);
    }

    #[test]
    fn terminals_append_in_creation_order_and_the_new_one_shows() {
        let mut t = tabs_with(&["a"]);
        assert!(t.add_terminal("a", 1));
        assert!(t.add_terminal("a", 2));
        let tab = t.get("a").unwrap();
        assert_eq!(tab.terminals, vec![1, 2]);
        assert_eq!(tab.shown, Some(2));
        assert!(!t.add_terminal("nope", 3));
    }

    #[test]
    fn adding_quietly_keeps_what_is_shown() {
        let mut t = tabs_with(&["a", "b"]);
        t.add_terminal("a", 1);
        t.add_terminal("b", 2);
        assert!(t.add_terminal_quietly("a", 3));
        assert_eq!(t.active().unwrap().env, "b");
        let tab = t.get("a").unwrap();
        assert_eq!((tab.terminals.clone(), tab.shown), (vec![1, 3], Some(1)));
        assert!(!t.add_terminal_quietly("nope", 4));
    }

    #[test]
    fn switching_envs_keeps_each_ones_shown_terminal() {
        let mut t = tabs_with(&["a", "b"]);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.add_terminal("b", 3);
        assert!(t.show(1));
        assert_eq!(t.active().unwrap().env, "a");
        t.activate("b");
        assert_eq!(t.active().unwrap().shown, Some(3));
        t.activate("a");
        assert_eq!(t.active().unwrap().shown, Some(1));
        assert!(t.show(3));
        assert_eq!(t.active().unwrap().env, "b");
        assert!(!t.show(99));
    }

    #[test]
    fn closing_the_shown_terminal_shows_the_next_else_the_previous() {
        let mut t = tabs_with(&["a"]);
        for id in [1, 2, 3] {
            t.add_terminal("a", id);
        }
        t.show(2);
        assert!(t.close_terminal(2));
        assert_eq!(t.get("a").unwrap().shown, Some(3));
        assert!(t.close_terminal(3));
        assert_eq!(t.get("a").unwrap().shown, Some(1));
        assert!(t.close_terminal(1));
        assert_eq!(t.get("a").unwrap().shown, None);
        assert!(t.get("a").is_some());
        assert!(!t.close_terminal(1));
    }

    #[test]
    fn closing_a_hidden_terminal_keeps_the_shown_one() {
        let mut t = tabs_with(&["a"]);
        for id in [1, 2, 3] {
            t.add_terminal("a", id);
        }
        t.show(2);
        t.close_terminal(1);
        t.close_terminal(3);
        let tab = t.get("a").unwrap();
        assert_eq!(tab.shown, Some(2));
        assert_eq!(tab.terminals, vec![2]);
    }

    #[test]
    fn files_focus_is_cleared_when_a_terminal_is_shown() {
        let mut t = tabs_with(&["a"]);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.set_files(1, Some(10));
        t.get_mut("a").unwrap().files_focused = true;
        t.show(1);
        assert!(!t.get("a").unwrap().files_focused);
        t.get_mut("a").unwrap().files_focused = true;
        t.next_terminal();
        assert!(!t.get("a").unwrap().files_focused);
        t.get_mut("a").unwrap().files_focused = true;
        t.add_terminal("a", 3);
        assert!(!t.get("a").unwrap().files_focused);
        t.show(1);
        t.get_mut("a").unwrap().files_focused = true;
        t.set_files(1, None);
        assert!(!t.get("a").unwrap().files_focused);
    }

    #[test]
    fn each_terminal_shows_its_own_files() {
        let mut t = tabs_with(&["a"]);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.set_files(1, Some(10));
        t.show(1);
        assert_eq!(t.active().unwrap().shown_files(), Some(10));
        t.show(2);
        assert_eq!(t.active().unwrap().shown_files(), None);
        t.set_files(2, Some(20));
        assert_eq!(t.active().unwrap().shown_files(), Some(20));
        t.show(1);
        assert_eq!(t.active().unwrap().shown_files(), Some(10));
        assert_eq!(t.files_of(2), Some(20));
        // Unknown terminals get none.
        t.set_files(99, Some(30));
        assert_eq!(t.files_of(99), None);
    }

    #[test]
    fn each_terminal_keeps_its_own_files_ratio() {
        let mut t = tabs_with(&["a"]);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.show(1);
        t.active_mut().unwrap().set_files_ratio(0.3);
        t.show(2);
        assert_eq!(t.active().unwrap().files_ratio(), DEFAULT_FILES_RATIO);
        t.active_mut().unwrap().set_files_ratio(0.95);
        assert_eq!(t.active().unwrap().files_ratio(), MAX_RATIO);
        t.show(1);
        assert_eq!(t.active().unwrap().files_ratio(), 0.3);
    }

    #[test]
    fn closing_a_terminal_forgets_its_files() {
        let mut t = tabs_with(&["a"]);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.set_files(1, Some(10));
        t.set_files(2, Some(20));
        assert!(t.close_terminal(1));
        assert_eq!(t.files_of(1), None);
        assert_eq!(t.env_of(10), None);
        assert_eq!(t.files_of(2), Some(20));
    }

    #[test]
    fn dropping_a_file_pane_finds_its_terminal() {
        let mut t = tabs_with(&["a"]);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.set_files(1, Some(10));
        t.set_files(2, Some(20));
        t.get_mut("a").unwrap().files_focused = true;
        t.drop_files(20);
        assert_eq!(t.files_of(2), None);
        assert_eq!(t.files_of(1), Some(10));
        assert!(!t.get("a").unwrap().files_focused);
    }

    #[test]
    fn close_env_returns_its_panes_and_activates_the_neighbour() {
        let mut t = tabs_with(&["a", "b", "c"]);
        t.add_terminal("b", 1);
        t.add_terminal("b", 2);
        t.set_files(1, Some(9));
        t.set_files(2, Some(8));
        t.activate("b");
        assert_eq!(t.close_env("b"), vec![1, 2, 9, 8]);
        assert_eq!(t.active().unwrap().env, "c");
        assert_eq!(t.close_env("b"), Vec::<PaneId>::new());
        assert_eq!(t.close_env("c"), Vec::<PaneId>::new());
        assert_eq!(t.active().unwrap().env, "a");
        t.add_env("d");
        t.activate("a");
        t.close_env("d");
        assert_eq!(t.active().unwrap().env, "a");
        t.close_env("a");
        assert!(t.is_empty());
        assert_eq!(t.active(), None);
        t.next();
        t.previous();
        assert!(t.add_env("e"));
        assert_eq!(t.active().unwrap().env, "e");
    }

    #[test]
    fn next_and_previous_terminal_wrap_within_the_active_env() {
        let mut t = tabs_with(&["a", "b"]);
        assert_eq!(t.next_terminal(), None);
        t.add_terminal("b", 7);
        t.add_terminal("a", 1);
        t.add_terminal("a", 2);
        t.add_terminal("a", 3);
        assert_eq!(t.next_terminal(), Some(1));
        assert_eq!(t.previous_terminal(), Some(3));
        assert_eq!(t.get("a").unwrap().shown, Some(3));
        assert_eq!(t.get("b").unwrap().shown, Some(7));
    }

    #[test]
    fn env_of_finds_terminals_and_file_panes() {
        let mut t = tabs_with(&["a", "b"]);
        t.add_terminal("a", 1);
        t.add_terminal("b", 2);
        t.set_files(2, Some(5));
        assert_eq!(t.env_of(1), Some("a"));
        assert_eq!(t.env_of(2), Some("b"));
        assert_eq!(t.env_of(5), Some("b"));
        assert_eq!(t.env_of(6), None);
    }

    #[test]
    fn new_terminal_cwd_prefers_env_home_then_shown_then_user_home() {
        let (s, e, u) = (Path::new("/s"), Path::new("/e"), Path::new("/u"));
        assert_eq!(new_terminal_cwd(Some(s), Some(e), u), PathBuf::from("/e"));
        assert_eq!(new_terminal_cwd(Some(s), None, u), PathBuf::from("/s"));
        assert_eq!(new_terminal_cwd(None, Some(e), u), PathBuf::from("/e"));
        assert_eq!(new_terminal_cwd(None, None, u), PathBuf::from("/u"));
    }
}
