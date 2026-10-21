//! hydraui's commands and their keys (spec §7.6): the defaults, `[keys]` overrides from
//! config.toml, and the GPUI actions they dispatch.
use std::collections::BTreeMap;

use gpui::{Action, DummyKeyboardMapper, KeyBinding, Keystroke};

gpui::actions!(
    hydraui,
    [
        NewTerminal,
        NextTerminal,
        PreviousTerminal,
        ClosePane,
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
    NewTerminal,
    ClosePane,
    NextTerminal,
    PreviousTerminal,
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
    pub const ALL: [Command; 13] = [
        Command::NewTerminal,
        Command::ClosePane,
        Command::NextTerminal,
        Command::PreviousTerminal,
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
            Command::NewTerminal => "new_terminal",
            Command::ClosePane => "close_pane",
            Command::NextTerminal => "next_terminal",
            Command::PreviousTerminal => "previous_terminal",
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
            Command::NewTerminal => "ctrl-t",
            Command::ClosePane => "ctrl-w",
            Command::NextTerminal => "ctrl-pagedown",
            Command::PreviousTerminal => "ctrl-pageup",
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

    /// What the command does, for the shortcuts list.
    pub fn label(self) -> &'static str {
        match self {
            Command::NewTerminal => "New terminal",
            Command::ClosePane => "Close terminal or file",
            Command::NextTerminal => "Next terminal",
            Command::PreviousTerminal => "Previous terminal",
            Command::NextTab => "Next environment tab",
            Command::PreviousTab => "Previous environment tab",
            Command::Copy => "Copy",
            Command::Paste => "Paste",
            Command::Find => "Find in terminal",
            Command::Launcher => "Launcher",
            Command::FileTree => "Show or hide the file tree",
            Command::OpenFile => "Open file…",
            Command::Search => "Search in files…",
        }
    }

    pub fn action(self) -> Box<dyn Action> {
        match self {
            Command::NewTerminal => Box::new(NewTerminal),
            Command::ClosePane => Box::new(ClosePane),
            Command::NextTerminal => Box::new(NextTerminal),
            Command::PreviousTerminal => Box::new(PreviousTerminal),
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

/// Commands that earlier versions had and this one dropped (hydraui has no splits). A `[keys]`
/// entry naming one is ignored silently, so an old config.toml keeps loading (spec §4).
pub const REMOVED: [&str; 7] = [
    "new_tab",
    "split_right",
    "split_down",
    "focus_left",
    "focus_right",
    "focus_up",
    "focus_down",
];

/// Each command's keys after `[keys]` overrides (`""` unbinds), plus what was wrong. A bad
/// keystroke keeps the default, so one typo doesn't lose a command.
pub fn keymap(overrides: &BTreeMap<String, String>) -> (Vec<(Command, String)>, Vec<String>) {
    let mut problems = Vec::new();
    for name in overrides.keys() {
        if !Command::ALL.iter().any(|c| c.name() == name) && !REMOVED.contains(&name.as_str()) {
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

/// Keys that aren't commands, so `[keys]` can't change them: (keys, what they do).
pub const FIXED: [(&str, &str); 5] = [
    ("Ctrl+S", "Save the file (in a file pane)"),
    ("Ctrl+V", "Paste text (in a terminal; an image goes to the program)"),
    ("Tab / Shift+Tab", "Completion in the shell (in a terminal)"),
    ("Ctrl+click", "Open a path or link in terminal output"),
    ("Ctrl+click", "Insert a file's path (in the file tree)"),
];

/// `keys` as people write them: "ctrl-shift-p" → "Ctrl+Shift+P", a chord's steps spaced.
pub fn display(keys: &str) -> String {
    keys.split_whitespace()
        .map(|step| {
            step.split('-')
                .map(|part| match part {
                    "ctrl" => "Ctrl".to_string(),
                    "shift" => "Shift".to_string(),
                    "alt" => "Alt".to_string(),
                    "cmd" | "win" | "super" => "Win".to_string(),
                    "pagedown" => "PgDn".to_string(),
                    "pageup" => "PgUp".to_string(),
                    "" => "-".to_string(),
                    p => {
                        let mut c = p.chars();
                        match c.next() {
                            Some(f) => f.to_uppercase().chain(c).collect(),
                            None => String::new(),
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        assert!(keys.contains(&(Command::NewTerminal, "ctrl-t".to_string())));
        assert!(keys.contains(&(Command::NextTab, "ctrl-tab".to_string())));
        assert!(keys.contains(&(Command::Launcher, "ctrl-shift-p".to_string())));
        assert!(keys.contains(&(Command::FileTree, "ctrl-b".to_string())));
        assert_eq!(Command::FileTree.name(), "file_tree");
        assert_eq!(Command::Copy.name(), "copy");
    }

    #[test]
    fn overrides_replace_and_an_empty_string_unbinds() {
        let (keys, problems) = keymap(&map(&[("new_terminal", "ctrl-alt-n"), ("find", "")]));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(keys.contains(&(Command::NewTerminal, "ctrl-alt-n".to_string())));
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
            problems[0].contains("new_terminal") && problems[0].contains("find"),
            "{problems:?}"
        );
    }

    #[test]
    fn removed_commands_are_ignored_not_errors() {
        let overrides = map(&[("split_right", "ctrl-x"), ("new_tab", "ctrl-y")]);
        let (keys, problems) = keymap(&overrides);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(keys.len(), Command::ALL.len());
        assert!(!keys.iter().any(|(_, k)| k == "ctrl-x" || k == "ctrl-y"));
    }

    #[test]
    fn terminal_keys_have_defaults() {
        let (keys, _) = keymap(&BTreeMap::new());
        for (name, key) in [
            ("new_terminal", "ctrl-t"),
            ("next_terminal", "ctrl-pagedown"),
            ("previous_terminal", "ctrl-pageup"),
        ] {
            assert!(
                keys.iter().any(|(c, k)| c.name() == name && k == key),
                "{name}"
            );
        }
    }

    #[test]
    fn keys_display_as_people_write_them() {
        assert_eq!(display("ctrl-shift-p"), "Ctrl+Shift+P");
        assert_eq!(display("ctrl-pagedown"), "Ctrl+PgDn");
        assert_eq!(display("ctrl-tab"), "Ctrl+Tab");
        assert_eq!(display("ctrl-k ctrl-s"), "Ctrl+K Ctrl+S");
        assert_eq!(display("f5"), "F5");
    }

    #[test]
    fn every_entry_becomes_a_binding() {
        let (keys, _) = keymap(&BTreeMap::new());
        assert_eq!(bindings(&keys).len(), Command::ALL.len());
    }
}
