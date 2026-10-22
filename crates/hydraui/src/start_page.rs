//! The start page's rows (spec §7.12): recent layouts, filtered, with when each was last
//! opened and whether another window has it. No UI.
use std::path::{Path, PathBuf};

use hydraui_sets::{RecentEntry, layout_name};

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub folder: String,
    pub ago: String,
    /// Another window has this layout open.
    pub elsewhere: bool,
}

/// "just now", "5 min ago", "2 h ago", "1 day ago", "3 days ago".
pub fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "1 day ago".into(),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// The file's folder, with `~` for the home folder.
pub fn folder_label(path: &Path, home: &Path) -> String {
    let dir = path.parent().unwrap_or(path);
    match dir.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => dir.display().to_string(),
    }
}

/// The recent layouts as rows, keeping those whose name or folder holds `filter` (any case).
pub fn rows(
    recent: &[RecentEntry],
    filter: &str,
    home: &Path,
    now: u64,
    claimed: &dyn Fn(&Path) -> bool,
) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    recent
        .iter()
        .map(|e| Row {
            name: layout_name(&e.path),
            folder: folder_label(&e.path, home),
            ago: ago(now.saturating_sub(e.opened)),
            elsewhere: claimed(&e.path),
            path: e.path.clone(),
        })
        .filter(|r| {
            needle.is_empty()
                || r.name.to_lowercase().contains(&needle)
                || r.folder.to_lowercase().contains(&needle)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(p: &str, opened: u64) -> RecentEntry {
        RecentEntry {
            path: PathBuf::from(p),
            opened,
        }
    }

    #[test]
    fn ago_reads_like_a_person_would_say_it() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(5 * 60), "5 min ago");
        assert_eq!(ago(2 * 3600 + 10), "2 h ago");
        assert_eq!(ago(3 * 86400), "3 days ago");
        assert_eq!(ago(86400), "1 day ago");
    }

    /// `windows` on Windows, `unix` elsewhere: the same place in each platform's spelling.
    fn os(windows: &'static str, unix: &'static str) -> &'static str {
        if cfg!(windows) { windows } else { unix }
    }

    #[test]
    fn folders_under_home_use_a_tilde() {
        let home = Path::new(os(r"C:\Users\d", "/Users/d"));
        assert_eq!(
            folder_label(&home.join(".hydra").join("x.hydraui"), home),
            os(r"~\.hydra", "~/.hydra")
        );
        let work = os(r"E:\work", "/work");
        assert_eq!(folder_label(&Path::new(work).join("x.hydraui"), home), work);
    }

    #[test]
    fn rows_filter_by_name_or_folder_and_mark_open_ones() {
        let home = Path::new(os(r"C:\Users\d", "/Users/d"));
        let (client, blog) = (
            os(r"E:\work\client-a.hydraui", "/work/client-a.hydraui"),
            os(r"C:\Users\d\blog.hydraui", "/Users/d/blog.hydraui"),
        );
        let recent = [entry(client, 1000), entry(blog, 900)];
        let open_elsewhere = |p: &Path| p.ends_with("blog.hydraui");
        let all = rows(&recent, "", home, 1000, &open_elsewhere);
        assert_eq!(
            all,
            [
                Row {
                    path: client.into(),
                    name: "client-a".into(),
                    folder: os(r"E:\work", "/work").into(),
                    ago: "just now".into(),
                    elsewhere: false,
                },
                Row {
                    path: blog.into(),
                    name: "blog".into(),
                    folder: "~".into(),
                    ago: "1 min ago".into(),
                    elsewhere: true,
                },
            ]
        );
        assert_eq!(
            rows(&recent, "WORK", home, 1000, &open_elsewhere).len(),
            1,
            "folder, any case"
        );
        assert_eq!(
            rows(&recent, "blo", home, 1000, &open_elsewhere)[0].name,
            "blog"
        );
        assert!(rows(&recent, "zzz", home, 1000, &open_elsewhere).is_empty());
    }
}
