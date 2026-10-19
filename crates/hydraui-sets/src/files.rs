//! Layout and group files anywhere on disk (spec §7.9): group files are set files, layout
//! files are `session.toml`'s format; and the names and references that tie them to sets.
use std::path::{Path, PathBuf};

use hydra_core::name::EnvName;

use crate::file::SetFile;
use crate::layout::Context;
use crate::resolve::{SetError, parse_set};
use crate::session::Session;
use crate::spec::SetSpec;

/// A set in the session or a layout: from the sets folder by name, or a group file by path.
#[derive(Debug, Clone, PartialEq)]
pub enum SetRef {
    Named(String),
    File(PathBuf),
}

/// A valid set name can't contain `/`, `\`, `:` or `.`, so anything else is a path.
pub fn set_ref(entry: &str) -> SetRef {
    if EnvName::parse(entry).is_ok() {
        SetRef::Named(entry.to_string())
    } else {
        SetRef::File(PathBuf::from(entry))
    }
}

/// The group header's name for a file: its name made valid (lowercase letters, digits, `-`,
/// at most 32), unique among `taken` with `-2`, `-3`…
pub fn group_name(path: &Path, taken: &[String]) -> String {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = file.trim_end_matches(".toml").trim_end_matches(".hydraui");
    let mut base = String::new();
    for c in stem.chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if !(c == '-' && (base.is_empty() || base.ends_with('-'))) {
            base.push(c);
        }
    }
    base.truncate(32);
    let base = base.trim_end_matches('-').to_string();
    let base = if base.is_empty() {
        "group".to_string()
    } else {
        base
    };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let mut b = base.clone();
            b.truncate(32 - suffix.len());
            format!("{}{suffix}", b.trim_end_matches('-'))
        })
        .find(|name| !taken.contains(name))
        .unwrap_or(base)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Group,
    Layout,
}

/// Which kind of file `text` is, by its top-level keys; `None` when it is neither (or not TOML).
pub fn file_kind(text: &str) -> Option<FileKind> {
    let doc: toml::Table = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()?;
    if doc.contains_key("tab") {
        return Some(FileKind::Group);
    }
    ["sets", "loose", "active", "tree", "tree_width"]
        .iter()
        .any(|k| doc.contains_key(*k))
        .then_some(FileKind::Layout)
}

/// Opens a group file as a set named after the file (spec §5.4).
pub fn load_set_file(ctx: &Context, path: &Path, taken: &[String]) -> Result<SetSpec, SetError> {
    let text = std::fs::read_to_string(path).map_err(|source| SetError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if file_kind(&text) == Some(FileKind::Layout) {
        return Err(SetError::Invalid {
            path: path.to_path_buf(),
            problems: vec!["this is a layout file: use Open layout file…".into()],
        });
    }
    parse_set(ctx, &group_name(path, taken), path, &text)
}

pub fn write_set_file(path: &Path, set: &SetFile) -> std::io::Result<()> {
    write_atomic(path, &toml::to_string(set).map_err(std::io::Error::other)?)
}

pub fn read_layout(path: &Path) -> Result<Session, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    if file_kind(&text) == Some(FileKind::Group) {
        return Err(format!(
            "{} is a group file: use Open group file…",
            path.display()
        ));
    }
    toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text))
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_layout(path: &Path, layout: &Session) -> std::io::Result<()> {
    write_atomic(
        path,
        &toml::to_string(layout).map_err(std::io::Error::other)?,
    )
}

/// Writes through a temporary file beside `path`, then replaces it: a failure leaves the old
/// file and no temporary one.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::TabFile;

    #[test]
    fn set_refs_are_names_or_paths() {
        assert_eq!(set_ref("client-a"), SetRef::Named("client-a".into()));
        assert_eq!(
            set_ref("C:/work/a.toml"),
            SetRef::File(PathBuf::from("C:/work/a.toml"))
        );
        assert_eq!(
            set_ref("C:\\work\\b.toml"),
            SetRef::File(PathBuf::from("C:\\work\\b.toml"))
        );
        assert_eq!(
            set_ref("Not A Name"),
            SetRef::File(PathBuf::from("Not A Name"))
        );
    }

    #[test]
    fn group_names_come_from_file_names_and_dont_clash() {
        assert_eq!(
            group_name(Path::new("C:/x/My Layout.toml"), &[]),
            "my-layout"
        );
        assert_eq!(group_name(Path::new("C:/x/api__web!.toml"), &[]), "api-web");
        assert_eq!(group_name(Path::new("C:/x/___.toml"), &[]), "group");
        let taken = vec!["work".to_string(), "work-2".to_string()];
        assert_eq!(group_name(Path::new("C:/x/work.toml"), &taken), "work-3");
        let long = "a".repeat(40);
        assert_eq!(
            group_name(Path::new(&format!("C:/x/{long}.toml")), &[]).len(),
            32
        );
    }

    #[test]
    fn file_kind_tells_groups_from_layouts() {
        assert_eq!(file_kind("env = \"w\"\n[[tab]]\n"), Some(FileKind::Group));
        assert_eq!(file_kind("sets = [\"a\"]\n"), Some(FileKind::Layout));
        assert_eq!(
            file_kind("[[loose]]\nenv = \"w\"\n"),
            Some(FileKind::Layout)
        );
        assert_eq!(file_kind("not = toml = at all"), None);
        assert_eq!(file_kind("other = 1\n"), None);
    }

    #[test]
    fn a_group_file_written_reads_back_the_same() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.toml");
        let set = SetFile {
            env: Some("work".into()),
            tabs: vec![TabFile {
                title: Some("api".into()),
                command: Some("npm run dev".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        write_set_file(&p, &set).unwrap();
        let back = crate::file::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.tabs, set.tabs);
        assert_eq!(back.env, set.env);
    }

    #[test]
    fn a_layout_file_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.hydraui.toml");
        let layout = Session {
            sets: vec!["a".into(), "C:/g/b.toml".into()],
            tree: true,
            ..Default::default()
        };
        write_layout(&p, &layout).unwrap();
        assert_eq!(read_layout(&p).unwrap(), layout);
        std::fs::write(&p, "[[tab]]\n").unwrap();
        assert!(read_layout(&p).unwrap_err().contains("group file"));
    }
}
