//! The pipelines open when hydraui last ran, so a restart opens them again (core spec §38):
//! `~/.hydra/hydraui/open-pipelines.toml`, rewritten whenever the set changes.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One open pipeline: its file name, the environment its first orchestrator runs in, and
/// whether that orchestrator was in Automatic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpenPipeline {
    pub name: String,
    pub env: String,
    pub watch: bool,
    /// Folded to one line in the sidebar.
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "pipeline")]
    pipelines: Vec<OpenPipeline>,
}

fn file(root: &Path) -> PathBuf {
    root.join("hydraui").join("open-pipelines.toml")
}

/// The pipelines saved last; none when there's no file or it can't be read.
pub fn load(root: &Path) -> Vec<OpenPipeline> {
    std::fs::read_to_string(file(root))
        .ok()
        .and_then(|t| toml::from_str::<File>(&t).ok())
        .map(|f| f.pipelines)
        .unwrap_or_default()
}

/// Saves `open` in place of what was there.
pub fn save(root: &Path, open: &[OpenPipeline]) -> Result<(), String> {
    let path = file(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = toml::to_string(&File {
        pipelines: open.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    // Beside it, then moved over it: a crash mid-write never leaves half a file.
    let tmp = path.with_extension("toml.new");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(name: &str, env: &str, watch: bool) -> OpenPipeline {
        OpenPipeline {
            name: name.into(),
            env: env.into(),
            watch,
            collapsed: false,
        }
    }

    #[test]
    fn what_was_saved_is_loaded_and_nothing_saved_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(d.path()), []);
        let set = [
            OpenPipeline {
                collapsed: true,
                ..open("hydra-triage", "iov42", false)
            },
            open("inth", "iov42", true),
        ];
        save(d.path(), &set).unwrap();
        assert_eq!(load(d.path()), set);
        save(d.path(), &[]).unwrap();
        assert_eq!(load(d.path()), []);
    }

    #[test]
    fn a_file_from_before_collapsing_loads_expanded() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("hydraui")).unwrap();
        std::fs::write(
            file(d.path()),
            "[[pipeline]]\nname = \"inth\"\nenv = \"iov42\"\nwatch = true\n",
        )
        .unwrap();
        assert_eq!(load(d.path()), [open("inth", "iov42", true)]);
    }

    #[test]
    fn a_damaged_file_opens_nothing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("hydraui")).unwrap();
        std::fs::write(file(d.path()), "[[pipeline]]\nname = 3\n").unwrap();
        assert_eq!(load(d.path()), []);
    }
}
