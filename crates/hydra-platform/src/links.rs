use std::path::{Path, PathBuf};

/// Makes `link` point at the directory `target`: a junction on Windows (no admin
/// rights needed), a symlink elsewhere.
pub fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        junction::create(target, link)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

/// True for junctions and symlinks; false for real files, folders and missing paths.
pub fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Where a junction or symlink points; `None` for anything that isn't a readable link.
pub fn link_target(link: &Path) -> Option<PathBuf> {
    if !is_link(link) {
        return None;
    }
    #[cfg(windows)]
    {
        junction::get_target(link).ok()
    }
    #[cfg(not(windows))]
    {
        std::fs::read_link(link).ok()
    }
}

/// True when `link` is a link to `target` (compared after resolving both, case-insensitively on Windows).
pub fn points_at(link: &Path, target: &Path) -> bool {
    link_target(link).is_some_and(|t| same_path(&t, target))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let s = p.to_string_lossy().into_owned();
        if cfg!(windows) {
            s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
        } else {
            s.trim_end_matches('/').to_string()
        }
    };
    norm(a) == norm(b)
}

/// Removes a link without touching what it points at. Refuses anything that isn't a link.
pub fn unlink(link: &Path) -> std::io::Result<()> {
    if !is_link(link) {
        return Err(std::io::Error::other(format!(
            "{} is not a link",
            link.display()
        )));
    }
    #[cfg(windows)]
    {
        std::fs::remove_dir(link)
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base_with_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base").join("skills");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("keep.md"), "precious").unwrap();
        (dir, base)
    }

    #[test]
    fn link_reads_through_and_unlink_keeps_the_target() {
        let (dir, base) = base_with_file();
        let link = dir.path().join("env").join("skills");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        link_dir(&base, &link).unwrap();
        assert!(is_link(&link));
        assert_eq!(
            std::fs::read_to_string(link.join("keep.md")).unwrap(),
            "precious"
        );
        unlink(&link).unwrap();
        assert!(!link.exists());
        assert_eq!(
            std::fs::read_to_string(base.join("keep.md")).unwrap(),
            "precious"
        );
    }

    #[test]
    fn is_link_is_false_for_folders_and_missing_paths() {
        let (dir, base) = base_with_file();
        assert!(!is_link(&base));
        assert!(!is_link(&dir.path().join("nope")));
        assert!(unlink(&base).is_err(), "unlink must refuse a real folder");
        assert!(base.join("keep.md").exists());
    }

    #[test]
    fn link_target_says_where_a_link_points() {
        let (dir, base) = base_with_file();
        let link = dir.path().join("skills-link");
        link_dir(&base, &link).unwrap();
        let t = link_target(&link).unwrap();
        assert_eq!(
            std::fs::canonicalize(&t).unwrap(),
            std::fs::canonicalize(&base).unwrap()
        );
        assert!(points_at(&link, &base));
        let upper = PathBuf::from(base.to_string_lossy().to_uppercase());
        #[cfg(windows)]
        assert!(points_at(&link, &upper), "case-insensitive on Windows");
        #[cfg(not(windows))]
        let _ = upper;
        assert!(!points_at(&link, dir.path()));
        assert_eq!(link_target(&base), None, "a real folder isn't a link");
        assert_eq!(link_target(&dir.path().join("nope")), None);
        assert!(!points_at(&base, &base), "a real folder never 'points at'");
    }

    #[test]
    fn remove_dir_all_does_not_follow_junctions() {
        let (dir, base) = base_with_file();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        link_dir(&base, &state.join("skills")).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
        assert_eq!(
            std::fs::read_to_string(base.join("keep.md")).unwrap(),
            "precious"
        );
    }
}
