//! Where layout files live and what they are called (spec §7.12): `*.hydraui`, TOML inside.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;

pub const LAYOUT_EXT: &str = "hydraui";

pub fn layouts_dir(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("layouts")
}

/// The file name without `.hydraui`, `.hydraui.toml` or `.toml`, any case.
pub fn layout_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_lowercase();
    for ext in [".hydraui.toml", ".hydraui", ".toml"] {
        if lower.ends_with(ext) && lower.len() > ext.len() {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name
}

/// `stem.hydraui` in `dir`, else `stem-2.hydraui`, `stem-3.hydraui`…, whichever is free.
pub fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{LAYOUT_EXT}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.{LAYOUT_EXT}")))
        .find(|p| !p.exists())
        .expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_drop_every_layout_extension() {
        let l = Path::new(if cfg!(windows) { r"C:\l" } else { "/l" });
        assert_eq!(layout_name(&l.join("client-a.hydraui")), "client-a");
        assert_eq!(layout_name(&l.join("client-a.HydraUI.TOML")), "client-a");
        assert_eq!(layout_name(&l.join("work.toml")), "work");
    }

    #[test]
    fn free_paths_count_up() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            free_path(d.path(), "untitled"),
            d.path().join("untitled.hydraui")
        );
        std::fs::write(d.path().join("untitled.hydraui"), "").unwrap();
        std::fs::write(d.path().join("untitled-2.hydraui"), "").unwrap();
        assert_eq!(
            free_path(d.path(), "untitled"),
            d.path().join("untitled-3.hydraui")
        );
    }
}
