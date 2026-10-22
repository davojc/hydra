//! The file tree's file operations (its right-click menu): new file or folder, rename,
//! delete to the Recycle Bin, reveal in Explorer and relative paths. No UI.
use std::path::{Path, PathBuf};

/// Characters Windows doesn't allow in a file name.
const NOT_IN_NAMES: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Why `name` can't be a new entry in `dir` (or `rename` of `current` there): `Some("")` while
/// nothing is typed, a sentence otherwise. `None` when it's fine.
pub fn name_problem(dir: &Path, name: &str, current: Option<&str>) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || Some(name) == current {
        return Some(String::new());
    }
    if name
        .chars()
        .any(|c| NOT_IN_NAMES.contains(&c) || c.is_control())
    {
        return Some(r#"A name can't contain < > : " / \ | ? or *."#.into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if name == "." || name == ".." || device {
        return Some(format!("{name} is a reserved name on Windows."));
    }
    if name.ends_with('.') {
        return Some("A name can't end with a dot.".into());
    }
    // A rename that only changes the case is the same file to Windows.
    let same = current.is_some_and(|c| c.eq_ignore_ascii_case(name));
    if !same && dir.join(name).exists() {
        return Some(format!("{name} already exists here."));
    }
    None
}

/// Creates an empty file `name` in `dir`.
pub fn create_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let path = dir.join(name.trim());
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("can't create {}: {e}", path.display()))?;
    Ok(path)
}

/// Creates the folder `name` in `dir`.
pub fn create_dir(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let path = dir.join(name.trim());
    std::fs::create_dir(&path).map_err(|e| format!("can't create {}: {e}", path.display()))?;
    Ok(path)
}

/// Renames `path` to `name` in the same folder, returning the new path. Never replaces
/// another entry.
pub fn rename(path: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = path.parent().unwrap_or(Path::new(""));
    let current = path.file_name().map(|n| n.to_string_lossy().into_owned());
    if let Some(p) = name_problem(dir, name, current.as_deref()).filter(|p| !p.is_empty()) {
        return Err(p);
    }
    let new = dir.join(name.trim());
    std::fs::rename(path, &new).map_err(|e| format!("can't rename {}: {e}", path.display()))?;
    Ok(new)
}

/// Lower-case with backslashes, for comparing Windows paths. ASCII lowering keeps byte
/// offsets, so a prefix's length is the same in the original.
fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_ascii_lowercase()
}

/// What follows `root` in `path`, with the platform's separators (backslashes on Windows):
/// `Some("")` for `root` itself.
fn rest_under(root: &Path, path: &Path) -> Option<String> {
    let (r, p) = (key(root), key(path));
    let r = r.trim_end_matches('\\');
    let original = path.to_string_lossy().replace('/', "\\");
    if p == r {
        Some(String::new())
    } else if p.starts_with(r) && p[r.len()..].starts_with('\\') {
        let rest = &original[r.len() + 1..];
        Some(if cfg!(windows) {
            rest.to_string()
        } else {
            rest.replace('\\', "/")
        })
    } else {
        None
    }
}

/// `path` relative to `root` with the platform's separators, or the whole path outside it.
pub fn relative_path(root: &Path, path: &Path) -> String {
    match rest_under(root, path) {
        Some(rest) if rest.is_empty() => ".".into(),
        Some(rest) => rest,
        None => path.to_string_lossy().into_owned(),
    }
}

/// Whether `path` is `root` or inside it.
pub fn is_under(path: &Path, root: &Path) -> bool {
    rest_under(root, path).is_some()
}

/// Where `path` is after `old` was renamed to `new`: itself, or something inside it.
pub fn moved(path: &Path, old: &Path, new: &Path) -> Option<PathBuf> {
    let rest = rest_under(old, path)?;
    Some(if rest.is_empty() {
        new.to_path_buf()
    } else {
        new.join(rest)
    })
}

/// Moves a file or folder to the Recycle Bin, without Explorer's own prompts.
#[cfg(windows)]
pub fn to_recycle_bin(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };
    // A list of paths, each null-terminated, ending with an empty one.
    let from: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: `op` points at `from`, which outlives the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted != 0 {
        return Err(format!(
            "can't move {} to the Recycle Bin (error {code:#x})",
            path.display()
        ));
    }
    Ok(())
}

/// Moves a file or folder to the Trash, as Finder's Move to Trash does.
#[cfg(target_os = "macos")]
pub fn to_recycle_bin(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSURL};
    let failed = |why: String| format!("can't move {} to the Trash: {why}", path.display());
    let url = NSURL::from_file_path(path).ok_or_else(|| failed("not a file path".into()))?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| failed(e.localizedDescription().to_string()))
}

/// Other platforms have no trash hydraui can reach yet, so nothing is deleted.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn to_recycle_bin(path: &Path) -> Result<(), String> {
    Err(format!(
        "can't move {} to the trash: not supported on this platform yet",
        path.display()
    ))
}

/// The menu item that runs `reveal_in_explorer`, named for the platform's file manager.
pub const REVEAL_LABEL: &str = if cfg!(windows) {
    "Reveal in Explorer"
} else if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else {
    "Show in file manager"
};

/// Opens Explorer on `path`'s folder with `path` selected.
#[cfg(windows)]
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't open Explorer: {e}"))
}

/// Opens Finder on `path`'s folder with `path` selected.
#[cfg(target_os = "macos")]
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't open Finder: {e}"))
}

/// Opens the file manager on `path`'s folder (it can't select `path` itself).
#[cfg(not(any(windows, target_os = "macos")))]
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    std::process::Command::new("xdg-open")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't open the file manager: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows_refuses_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("taken.txt"), "").unwrap();
        let p = |n: &str| name_problem(dir.path(), n, None);
        assert_eq!(p(""), Some(String::new()));
        assert_eq!(p("  "), Some(String::new()));
        assert_eq!(p("notes.md"), None);
        assert!(p("a/b").unwrap().contains("can't contain"));
        assert!(p("a:b").unwrap().contains("can't contain"));
        assert!(p("CON").unwrap().contains("reserved"));
        assert!(p("nul.txt").unwrap().contains("reserved"));
        assert!(p("..").unwrap().contains("reserved"));
        assert!(p("trailing.").unwrap().contains("end"));
        assert!(p("TAKEN.txt").unwrap().contains("already exists"));
    }

    #[test]
    fn a_rename_may_change_only_the_case() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.md"), "x").unwrap();
        assert_eq!(
            name_problem(dir.path(), "README.md", Some("readme.md")),
            None
        );
        assert_eq!(
            name_problem(dir.path(), "readme.md", Some("readme.md")),
            Some(String::new())
        );
        let new = rename(&dir.path().join("readme.md"), "README.md").unwrap();
        assert_eq!(new, dir.path().join("README.md"));
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["README.md"]);
    }

    #[test]
    fn new_entries_and_renames_land_where_asked() {
        let dir = tempfile::tempdir().unwrap();
        let f = create_file(dir.path(), "a.txt").unwrap();
        assert!(f.is_file());
        assert!(
            create_file(dir.path(), "a.txt").is_err(),
            "never overwrites"
        );
        let d = create_dir(dir.path(), "sub").unwrap();
        assert!(d.is_dir());
        let moved_to = rename(&f, "b.txt").unwrap();
        assert!(moved_to.is_file() && !f.exists());
        assert!(rename(&moved_to, "sub").is_err(), "never replaces");
    }

    #[cfg(unix)]
    #[test]
    fn relative_paths_use_slashes_and_fall_back_to_the_whole_path_on_unix() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_path(root, Path::new("/repo/src/main.rs")),
            "src/main.rs"
        );
        assert_eq!(relative_path(root, Path::new("/repo")), ".");
        assert_eq!(
            relative_path(root, Path::new("/elsewhere/x.txt")),
            "/elsewhere/x.txt"
        );
    }

    #[cfg(windows)]
    #[test]
    fn relative_paths_use_backslashes_and_fall_back_to_the_whole_path() {
        let root = Path::new(r"C:\repo");
        assert_eq!(
            relative_path(root, Path::new(r"C:\repo\src\main.rs")),
            r"src\main.rs"
        );
        assert_eq!(
            relative_path(root, Path::new("C:/Repo/src/lib.rs")),
            r"src\lib.rs"
        );
        assert_eq!(relative_path(root, Path::new(r"C:\repo")), ".");
        assert_eq!(relative_path(root, Path::new(r"D:\x.txt")), r"D:\x.txt");
    }

    // Windows' spellings: a different case is the same folder.
    #[cfg(windows)]
    #[test]
    fn open_files_follow_a_rename() {
        let (old, new) = (Path::new(r"C:\r\docs"), Path::new(r"C:\r\guide"));
        assert_eq!(
            moved(Path::new(r"C:\r\docs"), old, new),
            Some(new.to_path_buf())
        );
        assert_eq!(
            moved(Path::new(r"C:\R\Docs\a\b.md"), old, new),
            Some(PathBuf::from(r"C:\r\guide\a\b.md"))
        );
        assert_eq!(moved(Path::new(r"C:\r\docsx\b.md"), old, new), None);
        assert_eq!(moved(Path::new(r"C:\r\other.md"), old, new), None);
    }

    #[cfg(unix)]
    #[test]
    fn open_files_follow_a_rename_on_unix() {
        let (old, new) = (Path::new("/r/docs"), Path::new("/r/guide"));
        assert_eq!(moved(old, old, new), Some(new.to_path_buf()));
        assert_eq!(
            moved(Path::new("/r/docs/a/b.md"), old, new),
            Some(PathBuf::from("/r/guide/a/b.md"))
        );
        assert_eq!(moved(Path::new("/r/docsx/b.md"), old, new), None);
        assert_eq!(moved(Path::new("/r/other.md"), old, new), None);
    }
}
