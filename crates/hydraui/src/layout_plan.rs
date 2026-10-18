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

/// The layout's sets that aren't loaded already (by name, or by path for files), in order.
pub fn open_plan(
    entries: &[String],
    loaded_names: &[String],
    loaded_files: &[PathBuf],
) -> Vec<SetRef> {
    entries
        .iter()
        .map(|e| set_ref(e))
        .filter(|r| match r {
            SetRef::Named(n) => !loaded_names.contains(n),
            SetRef::File(p) => !loaded_files.iter().any(|f| key(f) == key(p)),
        })
        .collect()
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
        assert_eq!(plan, vec![SetRef::Named("c".into())]);
    }
}
