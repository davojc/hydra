//! `<HYDRA_HOME>/hydraui/session.toml`: the sets loaded at last exit, reloaded at start.
use std::path::PathBuf;

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    #[serde(default)]
    pub sets: Vec<String>,
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
    let path = session_file(paths);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(session).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let s = Session {
            sets: vec!["client-a".into(), "personal".into()],
        };
        write_session(&paths, &s).unwrap();
        assert_eq!(read_session(&paths), s);
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
