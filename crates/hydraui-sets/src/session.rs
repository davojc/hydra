//! `<HYDRA_HOME>/hydraui/session.toml`: what was open at last exit, from before layouts were
//! files (spec §7.12). Only `migrate_session` reads it now; `Session` is the layout format.
use std::path::PathBuf;

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};

use crate::file::TabFile;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    #[serde(default)]
    pub sets: Vec<String>,
    /// The tab in front, by its place in the tab strip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
    /// The file tree was open.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_width: Option<f32>,
    /// Tabs opened by hand, in the set file's tab format (every pane with its env, cwd and
    /// shell). Last, because TOML writes tables after plain values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loose: Vec<TabFile>,
}

pub fn session_file(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("session.toml")
}

/// The saved session; empty when the file is missing or unreadable, so a bad session file
/// never stops hydraui starting.
pub fn read_session(paths: &HydraPaths) -> Session {
    std::fs::read_to_string(session_file(paths))
        .ok()
        .and_then(|t| toml::from_str(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok())
        .unwrap_or_default()
}

/// Writes through a temporary file, so a crash mid-write leaves the old session.
pub fn write_session(paths: &HydraPaths, session: &Session) -> std::io::Result<()> {
    let text = toml::to_string(session).map_err(std::io::Error::other)?;
    crate::files::write_atomic(&session_file(paths), &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::NodeFile;

    #[test]
    fn round_trips() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let pane = |cwd: &str, shell: &str| NodeFile {
            env: Some("iov".into()),
            cwd: Some(cwd.into()),
            shell: Some(shell.into()),
            ..Default::default()
        };
        let s = Session {
            sets: vec!["client-a".into(), "personal".into()],
            loose: vec![
                TabFile {
                    layout: Some(NodeFile {
                        split: Some("right".into()),
                        ratio: Some(0.6),
                        panes: Some(vec![pane("C:/w", "pwsh"), pane("C:/u", "bash")]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                TabFile {
                    env: Some("iov".into()),
                    cwd: Some("C:/x".into()),
                    shell: Some("pwsh".into()),
                    ..Default::default()
                },
            ],
            active: Some(2),
            tree: true,
            tree_width: Some(320.0),
        };
        write_session(&paths, &s).unwrap();
        assert_eq!(read_session(&paths), s);
    }

    #[test]
    fn a_session_with_only_sets_still_loads() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::create_dir_all(session_file(&paths).parent().unwrap()).unwrap();
        std::fs::write(
            session_file(&paths),
            "sets = [\"a\"]
",
        )
        .unwrap();
        assert_eq!(
            read_session(&paths),
            Session {
                sets: vec!["a".into()],
                ..Default::default()
            }
        );
    }

    #[test]
    fn missing_or_corrupt_session_means_nothing_loaded() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert_eq!(read_session(&paths), Session::default());
        std::fs::create_dir_all(session_file(&paths).parent().unwrap()).unwrap();
        for junk in [
            "sets = 3\n",
            "\u{0}\u{1}garbage",
            "[[sets]]\n",
            "extra = 1\n",
        ] {
            std::fs::write(session_file(&paths), junk).unwrap();
            assert_eq!(read_session(&paths), Session::default(), "{junk:?}");
        }
    }
}
