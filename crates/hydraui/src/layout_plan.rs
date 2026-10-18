//! What opening a layout opens, and how the session names a loaded set (spec §5.4, §7.9).
use std::path::{Path, PathBuf};

use hydraui_sets::{SetRef, set_ref};

/// Windows paths: case and slash direction don't matter.
fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// How the session names a loaded set: by name when it is the sets folder's
/// `<name>.toml`, else by its file's path.
pub fn session_entry(name: &str, file: &Path, sets_dir: &Path) -> String {
    if key(file) == key(&sets_dir.join(format!("{name}.toml"))) {
        name.to_string()
    } else {
        file.to_string_lossy().into_owned()
    }
}

/// The layout's sets in order, each marked when it is loaded already (by name, or by path
/// for files): those aren't opened again, but their tabs still count for the active tab.
pub fn open_plan(
    entries: &[String],
    loaded_names: &[String],
    loaded_files: &[PathBuf],
) -> Vec<(SetRef, bool)> {
    entries
        .iter()
        .map(|e| {
            let r = set_ref(e);
            let loaded = match &r {
                SetRef::Named(n) => loaded_names.contains(n),
                SetRef::File(p) => loaded_files.iter().any(|f| key(f) == key(p)),
            };
            (r, loaded)
        })
        .collect()
}

/// Where a loaded set reloads from: the sets folder by name, or its own group file.
pub fn reload_from(name: &str, file: &Path, sets_dir: &Path) -> SetRef {
    if key(file) == key(&sets_dir.join(format!("{name}.toml"))) {
        SetRef::Named(name.to_string())
    } else {
        SetRef::File(file.to_path_buf())
    }
}

/// The names a group file can't take: loaded sets' and the sets folder's (a group named like
/// a folder set would hide it).
pub fn taken_names(loaded: &[String], in_folder: &[String]) -> Vec<String> {
    let mut out = loaded.to_vec();
    for n in in_folder {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_folder_sets_are_saved_by_name_others_by_path() {
        let dir = Path::new("C:/h/hydraui/sets");
        assert_eq!(
            session_entry("a", Path::new("C:/h/hydraui/sets/a.toml"), dir),
            "a"
        );
        assert_eq!(
            session_entry("a", Path::new("C:/repo/a.toml"), dir),
            "C:/repo/a.toml"
        );
    }

    #[test]
    fn open_plan_skips_sets_already_loaded() {
        let entries = vec!["a".to_string(), "C:/g/b.toml".to_string(), "c".to_string()];
        let plan = open_plan(
            &entries,
            &["a".to_string()],
            &[PathBuf::from("c:\\g\\B.toml")],
        );
        // Every set, in order, marked when it's loaded already, so the layout's active-tab
        // position still lines up with its tabs.
        assert_eq!(
            plan,
            vec![
                (SetRef::Named("a".into()), true),
                (SetRef::File(PathBuf::from("C:/g/b.toml")), true),
                (SetRef::Named("c".into()), false),
            ]
        );
    }

    #[test]
    fn a_group_reloads_from_its_own_file() {
        let dir = Path::new("C:/h/hydraui/sets");
        assert_eq!(
            reload_from("work", Path::new("C:/h/hydraui/sets/work.toml"), dir),
            SetRef::Named("work".into())
        );
        assert_eq!(
            reload_from("work", Path::new("C:/x/work.toml"), dir),
            SetRef::File(PathBuf::from("C:/x/work.toml"))
        );
    }

    #[test]
    fn group_files_dont_take_names_of_sets_in_the_folder() {
        let taken = taken_names(&["a".to_string()], &["work".to_string(), "a".to_string()]);
        assert_eq!(taken, vec!["a".to_string(), "work".to_string()]);
    }
}
