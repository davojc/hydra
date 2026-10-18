//! What a window's layout file means for its title, and starting another window (spec §7.12).
use std::path::Path;

/// `hydraui - <layout>`, `hydraui - untitled`, or `hydraui` on the start page.
pub fn window_title(layout: Option<&Path>, start_page: bool) -> String {
    match (start_page, layout) {
        (true, _) => "hydraui".into(),
        (false, Some(p)) => format!("hydraui - {}", hydraui_sets::layout_name(p)),
        (false, None) => "hydraui - untitled".into(),
    }
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
