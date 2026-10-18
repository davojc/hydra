//! What a window's layout file means for its title, and starting another window (spec §7.12).
use std::path::Path;

use hydraui_sets::Session;

/// `hydraui - <layout>`, `hydraui - untitled`, or `hydraui` on the start page.
pub fn window_title(layout: Option<&Path>, start_page: bool) -> String {
    match (start_page, layout) {
        (true, _) => "hydraui".into(),
        (false, Some(p)) => format!("hydraui - {}", hydraui_sets::layout_name(p)),
        (false, None) => "hydraui - untitled".into(),
    }
}

/// Whether a layout opens in this window: only an untitled window with no tabs takes one;
/// any other opens it in a new window, so layouts are never merged (spec §7.9).
pub fn opens_here(backed: bool, has_tabs: bool) -> bool {
    !backed && !has_tabs
}

/// After opening `layout`: let its file go when nothing in it opened (a missing folder, a
/// renamed environment), so the window doesn't save an empty layout over it.
pub fn let_go_after_open(opened_any: bool, layout: &Session) -> bool {
    !opened_any && *layout != Session::default()
}

/// What the window saves: what is open, plus the layout's sets and Loose tabs that didn't
/// open, so a folder that is missing today isn't dropped from the file for good.
pub fn with_unopened(mut current: Session, unopened: &Session) -> Session {
    for set in &unopened.sets {
        if !current.sets.contains(set) {
            current.sets.push(set.clone());
        }
    }
    current.loose.extend(unopened.loose.iter().cloned());
    current
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
    use hydraui_sets::file::TabFile;

    fn tab(cwd: &str) -> TabFile {
        TabFile {
            env: Some("work".into()),
            cwd: Some(cwd.into()),
            ..Default::default()
        }
    }

    #[test]
    fn only_an_empty_untitled_window_takes_a_layout() {
        assert!(opens_here(false, false));
        assert!(!opens_here(false, true), "untitled with tabs: no merging");
        assert!(!opens_here(true, false));
        assert!(!opens_here(true, true));
    }

    #[test]
    fn a_layout_that_opened_nothing_is_let_go() {
        let full = Session {
            loose: vec![tab("E:/gone")],
            ..Default::default()
        };
        assert!(let_go_after_open(false, &full));
        assert!(!let_go_after_open(true, &full));
        assert!(
            !let_go_after_open(false, &Session::default()),
            "an empty layout file is still a layout"
        );
    }

    #[test]
    fn entries_that_failed_to_open_stay_in_the_file() {
        let open = Session {
            sets: vec!["client-a".into()],
            loose: vec![tab("C:/here")],
            active: Some(1),
            ..Default::default()
        };
        let unopened = Session {
            sets: vec![r"\\share\groups\infra.toml".into(), "client-a".into()],
            loose: vec![tab("E:/gone")],
            ..Default::default()
        };
        let saved = with_unopened(open, &unopened);
        assert_eq!(
            saved.sets,
            ["client-a".to_string(), r"\\share\groups\infra.toml".into()],
            "appended once"
        );
        assert_eq!(saved.loose, [tab("C:/here"), tab("E:/gone")]);
        assert_eq!(saved.active, Some(1), "the open window's own state stays");
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
