//! What a window's layout file means for its title, and starting another window (spec §7.12).
use std::path::Path;

use hydraui_sets::layout::Layout;

/// `hydraui - <layout>`, `hydraui - untitled`, or `hydraui` on the start page.
pub fn window_title(layout: Option<&Path>, start_page: bool) -> String {
    match (start_page, layout) {
        (true, _) => "hydraui".into(),
        (false, Some(p)) => format!("hydraui - {}", hydraui_sets::layout_name(p)),
        (false, None) => "hydraui - untitled".into(),
    }
}

/// Whether a layout opens in this window: only an untitled window with no environments takes
/// one; any other opens it in a new window, so layouts are never merged (spec §7.9).
pub fn opens_here(backed: bool, has_envs: bool) -> bool {
    !backed && !has_envs
}

/// After opening `layout`: let its file go when it named environments and none opened (a
/// deleted environment, a missing folder), so the window doesn't save an empty layout over it.
pub fn let_go_after_open(opened_any: bool, layout: &Layout) -> bool {
    !opened_any && !layout.envs.is_empty()
}

/// What the window saves: what is open, plus the layout's environments and terminals that
/// didn't open, so a folder that is missing today isn't dropped from the file for good. An
/// unopened terminal joins its environment when that is open; else the environment is appended.
pub fn with_unopened(mut current: Layout, unopened: &Layout) -> Layout {
    for env in &unopened.envs {
        match current.envs.iter_mut().find(|e| e.name == env.name) {
            Some(open) => open.terminals.extend(env.terminals.iter().cloned()),
            None => current.envs.push(env.clone()),
        }
    }
    current
}

/// Forgets `env`'s entries that didn't open, once the user closes that environment, so the
/// next save doesn't bring it back into the file.
pub fn forget_env(unopened: &mut Layout, env: &str) {
    unopened.envs.retain(|e| e.name != env);
}

/// Unix seconds, for the recent list.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Another hydraui window: one layout per window, so a second layout opens in a new process.
pub fn spawn_hydraui(file: Option<&Path>) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    if let Some(f) = file {
        cmd.arg(f);
    }
    cmd.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydraui_sets::layout::{EnvFile, TerminalFile};

    fn env(name: &str, cwds: &[&str]) -> EnvFile {
        EnvFile {
            name: name.into(),
            terminals: cwds
                .iter()
                .map(|c| TerminalFile {
                    cwd: (*c).into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn only_an_empty_untitled_window_takes_a_layout() {
        assert!(opens_here(false, false));
        assert!(!opens_here(false, true), "untitled with envs: no merging");
        assert!(!opens_here(true, false));
        assert!(!opens_here(true, true));
    }

    #[test]
    fn let_go_only_when_entries_existed_and_none_opened() {
        let full = Layout {
            envs: vec![env("gone", &["E:/gone"])],
            ..Default::default()
        };
        assert!(let_go_after_open(false, &full));
        assert!(
            !let_go_after_open(false, &Layout::default()),
            "an empty layout file is still a layout"
        );
        assert!(!let_go_after_open(true, &full));
        assert!(!let_go_after_open(true, &Layout::default()));
    }

    #[test]
    fn unopened_terminals_and_envs_are_kept_on_write() {
        let current = Layout {
            active_env: Some("work".into()),
            envs: vec![env("work", &["a"])],
            ..Default::default()
        };
        let unopened = Layout {
            envs: vec![env("work", &["b"]), env("gone", &["c"])],
            ..Default::default()
        };
        let saved = with_unopened(current, &unopened);
        assert_eq!(
            saved.envs,
            vec![env("work", &["a", "b"]), env("gone", &["c"])]
        );
        assert_eq!(
            saved.active_env.as_deref(),
            Some("work"),
            "the open window's own state stays"
        );
    }

    #[test]
    fn a_closed_environment_leaves_what_did_not_open() {
        let mut unopened = Layout {
            envs: vec![env("work", &["b"]), env("closed", &["c"])],
            ..Default::default()
        };
        forget_env(&mut unopened, "closed");
        let current = Layout {
            envs: vec![env("work", &["a"])],
            ..Default::default()
        };
        assert_eq!(
            with_unopened(current, &unopened).envs,
            vec![env("work", &["a", "b"])],
            "the closed environment's skipped terminals don't bring it back"
        );
    }

    #[test]
    fn the_title_names_the_layout_not_the_tab() {
        assert_eq!(
            window_title(Some(Path::new(r"C:\l\client-a.hydraui")), false),
            "hydraui - client-a"
        );
        assert_eq!(window_title(None, false), "hydraui - untitled");
        assert_eq!(window_title(None, true), "hydraui");
        assert_eq!(
            window_title(Some(Path::new(r"C:\l\x.hydraui")), true),
            "hydraui"
        );
    }
}
