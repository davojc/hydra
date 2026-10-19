//! The version 2 layout file (`*.hydraui`, TOML): a window's environments in tab order, each
//! one's terminals and file pane, the tree, and what is selected. A file without
//! `version = 2` is an older hydraui's: it is refused, never converted or modified.
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::files::write_atomic;

pub const LAYOUT_VERSION: u32 = 2;

/// A whole layout file. Fields are ordered plain values first, since TOML needs tables last.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// Always written as [`LAYOUT_VERSION`].
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeFile>,
    #[serde(default, rename = "env", skip_serializing_if = "Vec::is_empty")]
    pub envs: Vec<EnvFile>,
}

/// The file tree: shown or hidden, and its dragged width.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeFile {
    #[serde(default)]
    pub open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
}

/// One environment tab: its terminals, the shown one, and its file pane if open.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvFile {
    pub name: String,
    /// Index into `terminals`: the one shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<FilesFile>,
    #[serde(default, rename = "terminal", skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<TerminalFile>,
}

/// An environment's file pane: the open files and the shown one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesFile {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
}

/// One terminal: its folder when saved, and optionally a title, shell and start command.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Hand-written only: run whenever the layout opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    /// Not a `*.hydraui` file.
    NotLayout(PathBuf),
    /// Made by an older hydraui (no `version = 2`).
    Old(PathBuf),
    /// Unreadable, or not valid TOML for a layout.
    Read { path: PathBuf, message: String },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLayout(p) => write!(f, "{} isn't a layout file (*.hydraui)", p.display()),
            Self::Old(p) => write!(
                f,
                "{} was made by an older hydraui and can't be opened; start a new layout",
                p.display()
            ),
            Self::Read { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Parses layout text read from `path`: anything without `version = 2` is `Old`.
pub fn parse_layout(path: &Path, text: &str) -> Result<Layout, LayoutError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let read_err = |e: toml::de::Error| LayoutError::Read {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    let table: toml::Table = toml::from_str(text).map_err(read_err)?;
    let current = table.get("version").and_then(|v| v.as_integer()) == Some(LAYOUT_VERSION.into());
    if !current {
        return Err(LayoutError::Old(path.to_path_buf()));
    }
    toml::from_str(text).map_err(read_err)
}

/// Reads a `*.hydraui` file (extension in any case).
pub fn read_layout(path: &Path) -> Result<Layout, LayoutError> {
    let is_layout = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("hydraui"));
    if !is_layout {
        return Err(LayoutError::NotLayout(path.to_path_buf()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| LayoutError::Read {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    parse_layout(path, &text)
}

/// Writes `layout` as version 2, replacing the file atomically.
pub fn write_layout(path: &Path, layout: &Layout) -> std::io::Result<()> {
    let mut layout = layout.clone();
    layout.version = LAYOUT_VERSION;
    write_atomic(
        path,
        &toml::to_string(&layout).map_err(std::io::Error::other)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
version = 2
active_env = "work"
tree = { open = true, width = 280.0 }

[[env]]
name = "work"
active = 0                       # index into this env's terminals: the one shown
files = { files = ["E:/work/api/README.md"], active = 0 }   # the env's file pane, if open

[[env.terminal]]
title = "claude"                 # optional: set by Rename
cwd = "E:/work/api"              # its current folder when saved
shell = "pwsh"                   # optional
command = "claude"               # optional: run when the layout opens (hand-written only)

[[env.terminal]]
cwd = "E:/work/api"

[[env]]
name = "personal"

[[env.terminal]]
cwd = "~/blog"
"#;

    #[test]
    fn the_spec_example_parses() {
        let l = parse_layout(Path::new("a.hydraui"), SPEC).unwrap();
        assert_eq!(l.active_env, Some("work".into()));
        assert_eq!(
            l.tree,
            Some(TreeFile {
                open: true,
                width: Some(280.0)
            })
        );
        assert_eq!(l.envs.len(), 2);
        assert_eq!(l.envs[0].terminals.len(), 2);
        assert_eq!(l.envs[0].terminals[0].command, Some("claude".into()));
        assert_eq!(
            l.envs[0].files.as_ref().unwrap().files,
            ["E:/work/api/README.md"]
        );
        assert_eq!(l.envs[1].terminals[0].cwd, "~/blog");
    }

    #[test]
    fn a_layout_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.hydraui");
        let layout = Layout {
            version: LAYOUT_VERSION,
            active_env: Some("b".into()),
            tree: Some(TreeFile {
                open: false,
                width: None,
            }),
            envs: vec![
                EnvFile {
                    name: "a".into(),
                    active: Some(1),
                    files: Some(FilesFile {
                        files: vec!["C:/x.md".into()],
                        active: Some(0),
                    }),
                    terminals: vec![
                        TerminalFile {
                            title: Some("t".into()),
                            cwd: "C:/a".into(),
                            shell: Some("pwsh".into()),
                            command: Some("claude".into()),
                        },
                        TerminalFile {
                            cwd: "C:/b".into(),
                            ..Default::default()
                        },
                    ],
                },
                EnvFile {
                    name: "b".into(),
                    ..Default::default()
                },
            ],
        };
        write_layout(&p, &layout).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .starts_with("version = 2")
        );
        assert_eq!(read_layout(&p).unwrap(), layout);
    }

    #[test]
    fn a_layout_without_version_2_is_old() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("old.hydraui");
        for text in [
            "sets = [\"a\"]\n",
            "[[loose]]\nenv = \"w\"\n",
            "version = 1\n",
            "",
        ] {
            std::fs::write(&p, text).unwrap();
            assert!(
                matches!(read_layout(&p), Err(LayoutError::Old(_))),
                "{text:?}"
            );
            assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        }
    }

    #[test]
    fn only_hydraui_files_are_layouts() {
        let d = tempfile::tempdir().unwrap();
        for name in ["notes.txt", "a.hydraui.toml"] {
            let p = d.path().join(name);
            std::fs::write(&p, "version = 2\n").unwrap();
            let e = read_layout(&p).unwrap_err();
            assert!(matches!(e, LayoutError::NotLayout(_)));
            assert!(e.to_string().contains(name));
        }
        let p = d.path().join("UP.HYDRAUI");
        std::fs::write(&p, "version = 2\n").unwrap();
        assert!(read_layout(&p).is_ok());
    }

    #[test]
    fn unknown_keys_and_bad_toml_name_the_file() {
        for text in ["version = 2\nbogus = 1\n", "version = 2\n[[env]]\n"] {
            let e = parse_layout(Path::new("dir/bad.hydraui"), text).unwrap_err();
            assert!(matches!(e, LayoutError::Read { .. }), "{text:?}");
            assert!(e.to_string().contains("bad.hydraui"));
        }
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        assert!(parse_layout(Path::new("a.hydraui"), "\u{feff}version = 2\n").is_ok());
    }
}
