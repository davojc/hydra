//! hydraui's commands and their keys (spec §7.6): the defaults, `[keys]` overrides from
//! config.toml, and the GPUI actions they dispatch.
use std::collections::BTreeMap;

use gpui::{Action, DummyKeyboardMapper, KeyBinding, Keystroke};

gpui::actions!(
    hydraui,
    [
        NewTab,
        ClosePane,
        SplitRight,
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        NextTab,
        PreviousTab,
        CopyText,
        PasteText,
        Find,
        Launcher,
        FileTree,
        OpenFile,
        SearchFiles,
        TerminalTab,
        TerminalBackTab,
        SaveFile,
        TestDialog
    ]
);

/// F12 opens HYDRAUI_TEST_DIALOG's dialog, for smoke tests; bound only in debug builds.
pub fn test_bindings() -> Vec<KeyBinding> {
    if cfg!(debug_assertions) {
        vec![KeyBinding::new("f12", TestDialog, None)]
    } else {
        Vec::new()
    }
}

/// Ctrl+S in a file pane (spec §7.8).
pub fn file_pane_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("ctrl-s", SaveFile, Some("FilePane"))]
}

/// Tab and Shift+Tab for the shell (completion), inside a pane. Without these, gpui-component's
/// Root takes them to move the focus between controls.
pub fn terminal_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", TerminalTab, Some("Terminal")),
        KeyBinding::new("shift-tab", TerminalBackTab, Some("Terminal")),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    NewTab,
    ClosePane,
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    NextTab,
    PreviousTab,
    Copy,
    Paste,
    Find,
    Launcher,
    FileTree,
    OpenFile,
    Search,
}

impl Command {
    pub const ALL: [Command; 17] = [
        Command::NewTab,
        Command::ClosePane,
        Command::SplitRight,
        Command::SplitDown,
        Command::FocusLeft,
        Command::FocusRight,
        Command::FocusUp,
        Command::FocusDown,
        Command::NextTab,
        Command::PreviousTab,
        Command::Copy,
        Command::Paste,
        Command::Find,
        Command::Launcher,
        Command::FileTree,
        Command::OpenFile,
        Command::Search,
    ];

    /// The name under `[keys]` in config.toml.
    pub fn name(self) -> &'static str {
        match self {
            Command::NewTab => "new_tab",
            Command::ClosePane => "close_pane",
            Command::SplitRight => "split_right",
            Command::SplitDown => "split_down",
            Command::FocusLeft => "focus_left",
            Command::FocusRight => "focus_right",
            Command::FocusUp => "focus_up",
            Command::FocusDown => "focus_down",
            Command::NextTab => "next_tab",
            Command::PreviousTab => "previous_tab",
            Command::Copy => "copy",
            Command::Paste => "paste",
            Command::Find => "find",
            Command::Launcher => "launcher",
            Command::FileTree => "file_tree",
            Command::OpenFile => "open_file",
            Command::Search => "search",
        }
    }

    /// The spec §7.6 defaults.
    pub fn default_keys(self) -> &'static str {
        match self {
            Command::NewTab => "ctrl-t",
            Command::ClosePane => "ctrl-w",
            Command::SplitRight => "ctrl-shift-d",
            Command::SplitDown => "ctrl-shift-e",
            Command::FocusLeft => "alt-left",
            Command::FocusRight => "alt-right",
            Command::FocusUp => "alt-up",
            Command::FocusDown => "alt-down",
            Command::NextTab => "ctrl-tab",
            Command::PreviousTab => "ctrl-shift-tab",
            Command::Copy => "ctrl-shift-c",
            Command::Paste => "ctrl-shift-v",
            Command::Find => "ctrl-f",
            Command::Launcher => "ctrl-shift-p",
            Command::FileTree => "ctrl-b",
            Command::OpenFile => "ctrl-shift-o",
            Command::Search => "ctrl-shift-f",
        }
    }

    pub fn action(self) -> Box<dyn Action> {
        match self {
            Command::NewTab => Box::new(NewTab),
            Command::ClosePane => Box::new(ClosePane),
            Command::SplitRight => Box::new(SplitRight),
            Command::SplitDown => Box::new(SplitDown),
            Command::FocusLeft => Box::new(FocusLeft),
            Command::FocusRight => Box::new(FocusRight),
            Command::FocusUp => Box::new(FocusUp),
            Command::FocusDown => Box::new(FocusDown),
            Command::NextTab => Box::new(NextTab),
            Command::PreviousTab => Box::new(PreviousTab),
            Command::Copy => Box::new(CopyText),
            Command::Paste => Box::new(PasteText),
            Command::Find => Box::new(Find),
            Command::Launcher => Box::new(Launcher),
            Command::FileTree => Box::new(FileTree),
            Command::OpenFile => Box::new(OpenFile),
            Command::Search => Box::new(SearchFiles),
        }
    }
}

/// Each command's keys after `[keys]` overrides (`""` unbinds), plus what was wrong. A bad
/// keystroke keeps the default, so one typo doesn't lose a command.
pub fn keymap(overrides: &BTreeMap<String, String>) -> (Vec<(Command, String)>, Vec<String>) {
    let mut problems = Vec::new();
    for name in overrides.keys() {
        if !Command::ALL.iter().any(|c| c.name() == name) {
            problems.push(format!("[keys]: unknown command {name:?}"));
        }
    }
    let mut out = Vec::new();
    for c in Command::ALL {
        let keys = overrides
            .get(c.name())
            .map_or(c.default_keys(), |k| k.trim());
        if keys.is_empty() {
            continue;
        }
        match keys
            .split_whitespace()
            .try_for_each(|k| Keystroke::parse(k).map(|_| ()))
        {
            Ok(()) => out.push((c, keys.to_string())),
            Err(e) => {
                problems.push(format!("[keys] {} = {keys:?}: {e}", c.name()));
                out.push((c, c.default_keys().to_string()));
            }
        }
    }
    let mut seen: BTreeMap<String, Command> = BTreeMap::new();
    for (c, k) in &out {
        if let Some(earlier) = seen.insert(k.to_lowercase(), *c) {
            problems.push(format!(
                "[keys] {k:?} is bound to both {} and {}",
                earlier.name(),
                c.name()
            ));
        }
    }
    (out, problems)
}

/// GPUI key bindings for `keymap`'s result. They have no context, so they work wherever
/// the focus is.
pub fn bindings(keys: &[(Command, String)]) -> Vec<KeyBinding> {
    keys.iter()
        .filter_map(|(c, k)| {
            KeyBinding::load(k, c.action(), None, false, None, &DummyKeyboardMapper).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    gpui::actions!(test_root, [RootTab]);

    #[test]
    fn ctrl_s_saves_in_a_file_pane() {
        let keymap = gpui::Keymap::new(file_pane_bindings());
        let stack = [
            gpui::KeyContext::parse("Workspace").unwrap(),
            gpui::KeyContext::parse("FilePane").unwrap(),
        ];
        let (found, _) = keymap.bindings_for_input(&[Keystroke::parse("ctrl-s").unwrap()], &stack);
        assert!(found[0].action().partial_eq(&SaveFile));
    }

    #[test]
    fn tab_and_shift_tab_reach_the_terminal_not_focus_cycling() {
        // gpui-component's Root binds tab to move the focus; the terminal sits inside it.
        let mut keymap = gpui::Keymap::new(terminal_bindings());
        keymap.add_bindings([
            KeyBinding::new("tab", RootTab, Some("Root")),
            KeyBinding::new("shift-tab", RootTab, Some("Root")),
        ]);
        let stack = [
            gpui::KeyContext::parse("Root").unwrap(),
            gpui::KeyContext::parse("Terminal").unwrap(),
        ];
        let first = |key: &str| {
            let (found, _) = keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &stack);
            found[0].action().boxed_clone()
        };
        assert!(first("tab").partial_eq(&TerminalTab));
        assert!(first("shift-tab").partial_eq(&TerminalBackTab));
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_defaults_bind_every_command() {
        let (keys, problems) = keymap(&BTreeMap::new());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(keys.len(), Command::ALL.len());
        assert!(keys.contains(&(Command::SplitRight, "ctrl-shift-d".to_string())));
        assert!(keys.contains(&(Command::NextTab, "ctrl-tab".to_string())));
        assert!(keys.contains(&(Command::Launcher, "ctrl-shift-p".to_string())));
        assert!(keys.contains(&(Command::FileTree, "ctrl-b".to_string())));
        assert_eq!(Command::FileTree.name(), "file_tree");
        assert_eq!(Command::Copy.name(), "copy");
    }

    #[test]
    fn overrides_replace_and_an_empty_string_unbinds() {
        let (keys, problems) = keymap(&map(&[("split_right", "ctrl-alt-d"), ("find", "")]));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(keys.contains(&(Command::SplitRight, "ctrl-alt-d".to_string())));
        assert!(!keys.iter().any(|(c, _)| *c == Command::Find));
        assert_eq!(keys.len(), Command::ALL.len() - 1);
    }

    #[test]
    fn mistakes_are_reported_and_the_default_kept() {
        let (keys, problems) = keymap(&map(&[
            ("split_rigth", "ctrl-x"),
            ("copy", "ctrl-shift-nope-c"),
        ]));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems.iter().any(|p| p.contains("split_rigth")),
            "{problems:?}"
        );
        assert!(problems.iter().any(|p| p.contains("copy")), "{problems:?}");
        assert!(keys.contains(&(Command::Copy, "ctrl-shift-c".to_string())));
    }

    #[test]
    fn the_same_keys_on_two_commands_is_reported() {
        let (_, problems) = keymap(&map(&[("find", "ctrl-t")]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("new_tab") && problems[0].contains("find"),
            "{problems:?}"
        );
    }

    #[test]
    fn every_entry_becomes_a_binding() {
        let (keys, _) = keymap(&BTreeMap::new());
        assert_eq!(bindings(&keys).len(), Command::ALL.len());
    }
}
