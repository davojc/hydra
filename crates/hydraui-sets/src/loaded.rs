//! Which sets are open, and which tabs each one opened.
use std::path::Path;
use std::time::SystemTime;

use hydra_core::paths::HydraPaths;

use crate::set_file;

/// The app's id for a tab.
pub type TabId = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSet {
    pub name: String,
    pub tabs: Vec<TabId>,
    /// The set file's modification time when it was loaded.
    pub modified: Option<SystemTime>,
}

/// Loaded sets in load order (the tab strip's group order).
#[derive(Debug, Default)]
pub struct LoadedSets {
    sets: Vec<LoadedSet>,
}

pub fn file_modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl LoadedSets {
    pub fn get(&self, name: &str) -> Option<&LoadedSet> {
        self.sets.iter().find(|s| s.name == name)
    }

    /// Adds a set, or replaces it in place when reloading.
    pub fn insert(&mut self, set: LoadedSet) {
        match self.sets.iter_mut().find(|s| s.name == set.name) {
            Some(existing) => *existing = set,
            None => self.sets.push(set),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<LoadedSet> {
        let i = self.sets.iter().position(|s| s.name == name)?;
        Some(self.sets.remove(i))
    }

    pub fn set_of(&self, tab: TabId) -> Option<&str> {
        self.sets
            .iter()
            .find(|s| s.tabs.contains(&tab))
            .map(|s| s.name.as_str())
    }

    /// Forgets a tab closed by hand. Returns the set's name when it was the set's last tab;
    /// the set is then unloaded.
    pub fn tab_closed(&mut self, tab: TabId) -> Option<String> {
        let set = self.sets.iter_mut().find(|s| s.tabs.contains(&tab))?;
        set.tabs.retain(|t| *t != tab);
        if !set.tabs.is_empty() {
            return None;
        }
        let name = set.name.clone();
        self.remove(&name);
        Some(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.sets.iter().map(|s| s.name.clone()).collect()
    }

    /// Loaded sets whose file was edited or deleted since loading.
    pub fn changed(&self, paths: &HydraPaths) -> Vec<String> {
        self.sets
            .iter()
            .filter(|s| file_modified(&set_file(paths, &s.name)) != s.modified)
            .map(|s| s.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn set(name: &str, tabs: &[TabId]) -> LoadedSet {
        LoadedSet {
            name: name.into(),
            tabs: tabs.to_vec(),
            modified: None,
        }
    }

    #[test]
    fn tracks_sets_and_their_tabs_in_load_order() {
        let mut l = LoadedSets::default();
        l.insert(set("client-a", &[1, 2]));
        l.insert(set("personal", &[3]));
        assert_eq!(l.names(), vec!["client-a", "personal"]);
        assert_eq!(l.set_of(2), Some("client-a"));
        assert_eq!(l.set_of(9), None);
        assert_eq!(l.get("personal").unwrap().tabs, vec![3]);
    }

    #[test]
    fn reloading_replaces_in_place() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1]));
        l.insert(set("b", &[2]));
        l.insert(set("a", &[5, 6]));
        assert_eq!(l.names(), vec!["a", "b"]);
        assert_eq!(l.get("a").unwrap().tabs, vec![5, 6]);
    }

    #[test]
    fn closing_the_last_tab_unloads_the_set() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1, 2]));
        assert_eq!(l.tab_closed(1), None);
        assert_eq!(l.tab_closed(2), Some("a".to_string()));
        assert!(l.get("a").is_none());
        assert_eq!(l.tab_closed(7), None);
    }

    #[test]
    fn remove_returns_the_tabs_to_close() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1, 2]));
        assert_eq!(l.remove("a").unwrap().tabs, vec![1, 2]);
        assert!(l.remove("a").is_none());
    }

    #[test]
    fn changed_finds_edited_and_deleted_set_files() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::create_dir_all(crate::sets_dir(&paths)).unwrap();
        for n in ["same", "edited"] {
            std::fs::write(set_file(&paths, n), "").unwrap();
        }
        let mut l = LoadedSets::default();
        l.insert(LoadedSet {
            modified: file_modified(&set_file(&paths, "same")),
            ..set("same", &[1])
        });
        l.insert(LoadedSet {
            modified: Some(UNIX_EPOCH),
            ..set("edited", &[2])
        });
        l.insert(LoadedSet {
            modified: Some(UNIX_EPOCH),
            ..set("gone", &[3])
        });
        assert_eq!(l.changed(&paths), vec!["edited", "gone"]);
    }
}
