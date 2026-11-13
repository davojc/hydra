//! A pipeline's icon (core spec §64): `icon = "mail"` in its file draws that Lucide icon in its
//! colour wherever its ◆ was (its card, its tab strip, its settings), so pipelines are told
//! apart by shape as well as colour. Left out, it's the ◆. The icons are a chosen few, bundled
//! by `crate::assets`.

use gpui::prelude::*;
use gpui::{AnyElement, div, px, svg};

/// The icons a pipeline can have: the file's name for it, and what Pipeline settings calls it.
pub const ICONS: [(&str, &str); 30] = [
    ("git-pull-request", "Pull request"),
    ("git-branch", "Branch"),
    ("code", "Code"),
    ("bug", "Bug"),
    ("ticket", "Ticket"),
    ("clipboard-check", "Checklist"),
    ("flask-conical", "Test"),
    ("search", "Search"),
    ("shield", "Shield"),
    ("rocket", "Rocket"),
    ("zap", "Lightning"),
    ("sparkles", "Sparkles"),
    ("mail", "Mail"),
    ("inbox", "Inbox"),
    ("message-square", "Message"),
    ("bell", "Bell"),
    ("users", "People"),
    ("bot", "Bot"),
    ("calendar", "Calendar"),
    ("clock", "Clock"),
    ("book-open", "Book"),
    ("database", "Database"),
    ("package", "Package"),
    ("layers", "Layers"),
    ("wrench", "Wrench"),
    ("globe", "Globe"),
    ("target", "Target"),
    ("flag", "Flag"),
    ("star", "Star"),
    ("heart", "Heart"),
];

/// `icon`, checked: one of `ICONS`.
pub fn check(icon: &str) -> Result<String, String> {
    let icon = icon.trim();
    if ICONS.iter().any(|(name, _)| *name == icon) {
        Ok(icon.to_string())
    } else {
        Err(format!(
            "icon {icon:?} isn't one hydraui has: pick one in Pipeline settings, e.g. \"mail\""
        ))
    }
}

/// Its file among the app's assets.
pub fn path(icon: &str) -> String {
    format!("icons/{icon}.svg")
}

/// A pipeline's mark at `size` pixels: its icon in `accent`, or the ◆ when it has none.
pub fn mark(icon: Option<&str>, accent: u32, size: f32) -> AnyElement {
    let tint = crate::orchestrator_theme::color(accent);
    match icon {
        Some(i) => svg()
            .path(path(i))
            .size(px(size))
            .flex_none()
            .text_color(tint)
            .into_any_element(),
        None => div()
            .text_size(px(size * 0.85))
            .text_color(tint)
            .flex_none()
            .child("◆")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_icon_is_one_of_the_chosen_few() {
        assert_eq!(check(" mail ").as_deref(), Ok("mail"));
        assert!(
            check("not-an-icon")
                .unwrap_err()
                .contains("Pipeline settings")
        );
        let names: std::collections::BTreeSet<&str> = ICONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), ICONS.len(), "each icon once");
    }

    #[test]
    fn every_icon_is_bundled() {
        use gpui::AssetSource;
        // Create a pipeline's and Export to share's icons too.
        for name in ICONS.iter().map(|(n, _)| *n).chain([
            "share",
            "file-plus",
            "file-up",
            "circle-arrow-up",
        ]) {
            let bytes = crate::assets::AppAssets.load(&path(name)).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "{name} isn't bundled");
        }
    }
}
