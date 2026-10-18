//! Layout files (spec §7.12): `*.hydraui`, TOML inside in `Session`'s format; older
//! `.toml` layouts converted when opened; the one-time move of `session.toml`.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;

use crate::files::{read_layout, write_layout};
use crate::session::{Session, read_session, session_file};

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

fn is_current(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(LAYOUT_EXT))
}

pub fn converted_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.{LAYOUT_EXT}", layout_name(path)))
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

#[derive(Debug, PartialEq)]
pub enum Opened {
    /// A `.hydraui` file.
    Current(Session),
    /// An older layout, now written to `to`; `from` is deleted.
    Converted {
        layout: Session,
        from: PathBuf,
        to: PathBuf,
    },
    /// An older layout whose `.hydraui` name is taken: opened untitled, nothing touched.
    Untitled { layout: Session, exists: PathBuf },
}

/// Reads a layout file, converting an older one to `.hydraui` beside it.
pub fn open_layout_file(path: &Path) -> Result<Opened, String> {
    let layout = read_layout(path)?;
    if is_current(path) {
        return Ok(Opened::Current(layout));
    }
    let to = converted_path(path);
    if to.exists() {
        return Ok(Opened::Untitled { layout, exists: to });
    }
    write_layout(&to, &layout).map_err(|e| format!("can't write {}: {e}", to.display()))?;
    // The new file holds everything; an old file that can't be deleted is only clutter.
    let _ = std::fs::remove_file(path);
    Ok(Opened::Converted {
        layout,
        from: path.to_path_buf(),
        to,
    })
}

/// Moves `session.toml` (from before layouts were files) into the layouts folder, once.
/// Returns the new file; an empty or unreadable session is deleted and gives `None`.
pub fn migrate_session(paths: &HydraPaths) -> Option<PathBuf> {
    let file = session_file(paths);
    if !file.exists() {
        return None;
    }
    let session = read_session(paths);
    if session == Session::default() {
        let _ = std::fs::remove_file(&file);
        return None;
    }
    let to = free_path(&layouts_dir(paths), "last-session");
    // Couldn't write: keep session.toml for the next start.
    write_layout(&to, &session).ok()?;
    let _ = std::fs::remove_file(&file);
    Some(to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::TabFile;
    use crate::session::{session_file, write_session};

    fn a_layout() -> Session {
        Session {
            sets: vec!["client-a".into()],
            loose: vec![TabFile {
                env: Some("work".into()),
                cwd: Some("C:/w".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn names_drop_every_layout_extension() {
        assert_eq!(layout_name(Path::new(r"C:\l\client-a.hydraui")), "client-a");
        assert_eq!(
            layout_name(Path::new(r"C:\l\client-a.HydraUI.TOML")),
            "client-a"
        );
        assert_eq!(layout_name(Path::new(r"C:\l\work.toml")), "work");
        assert_eq!(
            converted_path(Path::new(r"C:\l\client-a.hydraui.toml")),
            PathBuf::from(r"C:\l\client-a.hydraui")
        );
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

    #[test]
    fn a_hydraui_file_opens_as_it_is() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.hydraui");
        write_layout(&p, &a_layout()).unwrap();
        assert_eq!(open_layout_file(&p).unwrap(), Opened::Current(a_layout()));
    }

    #[test]
    fn an_old_file_becomes_a_hydraui_file() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("a.hydraui.toml");
        write_layout(&old, &a_layout()).unwrap();
        let new = d.path().join("a.hydraui");
        assert_eq!(
            open_layout_file(&old).unwrap(),
            Opened::Converted {
                layout: a_layout(),
                from: old.clone(),
                to: new.clone()
            }
        );
        assert!(!old.exists(), "the old file is deleted");
        assert_eq!(read_layout(&new).unwrap(), a_layout());
    }

    #[test]
    fn conversion_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("a.hydraui.toml");
        write_layout(&old, &a_layout()).unwrap();
        let new = d.path().join("a.hydraui");
        std::fs::write(&new, "sets = [\"other\"]\n").unwrap();
        assert_eq!(
            open_layout_file(&old).unwrap(),
            Opened::Untitled {
                layout: a_layout(),
                exists: new.clone()
            }
        );
        assert!(old.exists(), "nothing deleted");
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "sets = [\"other\"]\n"
        );
    }

    #[test]
    fn a_bad_file_is_an_error_naming_it() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.hydraui");
        std::fs::write(&p, "sets = 3\n").unwrap();
        assert!(open_layout_file(&p).unwrap_err().contains("bad.hydraui"));
        assert!(open_layout_file(&d.path().join("gone.hydraui")).is_err());
    }

    #[test]
    fn the_session_moves_into_the_layouts_folder_once() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        write_session(&paths, &a_layout()).unwrap();
        let moved = migrate_session(&paths).unwrap();
        assert_eq!(moved, layouts_dir(&paths).join("last-session.hydraui"));
        assert_eq!(read_layout(&moved).unwrap(), a_layout());
        assert!(!session_file(&paths).exists());
        assert_eq!(migrate_session(&paths), None, "only once");
    }

    #[test]
    fn an_empty_or_broken_session_is_just_deleted() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        for text in ["", "sets = 3\n"] {
            std::fs::create_dir_all(session_file(&paths).parent().unwrap()).unwrap();
            std::fs::write(session_file(&paths), text).unwrap();
            assert_eq!(migrate_session(&paths), None, "{text:?}");
            assert!(!session_file(&paths).exists(), "{text:?}");
        }
        assert!(!layouts_dir(&paths).exists(), "nothing written");
    }

    #[test]
    fn the_migrated_session_takes_a_free_name() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::create_dir_all(layouts_dir(&paths)).unwrap();
        std::fs::write(layouts_dir(&paths).join("last-session.hydraui"), "").unwrap();
        write_session(&paths, &a_layout()).unwrap();
        assert_eq!(
            migrate_session(&paths).unwrap(),
            layouts_dir(&paths).join("last-session-2.hydraui")
        );
    }
}
