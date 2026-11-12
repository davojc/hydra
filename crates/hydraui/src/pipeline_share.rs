//! Pipelines shared through a swarm (core spec §69). **Share to a swarm** writes a pipeline into
//! `<swarm folder>/pipelines/`, and teammates add it with **Import from a swarm**.
//!
//! A swarm that syncs `.toml` gets the pipeline file itself, `<name>.toml`. One that doesn't
//! (a swarm syncs only `.md` until an owner adds other types) gets `<name>.md`: a few lines
//! saying what it is, then the settings in a ```toml block. Import reads both.

use std::path::{Path, PathBuf};

/// The folder in a swarm's root that shared pipelines live in.
pub const SWARM_DIR: &str = "pipelines";

/// A pipeline's exported text as a swarm `.md` file.
pub fn to_md(title: &str, text: &str) -> String {
    let body = if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    };
    format!(
        "# {title}\n\nA hydraui pipeline shared through this swarm. To use it, open hydraui, \
         choose Create pipeline, then Import from a swarm.\n\n```toml\n{body}```\n"
    )
}

/// The pipeline text in a swarm `.md` file: its first ```toml block.
pub fn from_md(md: &str) -> Option<String> {
    let start = md.find("```toml")?;
    let rest = &md[start + "```toml".len()..];
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    let rest = rest.strip_prefix('\n')?;
    let end = rest
        .find("\n```")
        .map_or_else(|| rest.find("```"), |e| Some(e + 1))?;
    Some(rest[..end].to_string())
}

/// Whether a swarm whose synced file types are `types` (`.md`, `.toml`, as the roster keeps
/// them) syncs `.toml` files.
pub fn syncs_toml(types: &[String]) -> bool {
    types.iter().any(|t| {
        t.trim()
            .trim_start_matches('.')
            .eq_ignore_ascii_case("toml")
    })
}

/// Writes pipeline `name` (exported, so without this machine's `[autostart]`) into the swarm
/// at `swarm_folder`, making its `pipelines` folder when there's none: as `<name>.toml` when
/// the swarm syncs `.toml` (`toml`), else as `<name>.md`. A copy shared earlier in the other
/// form is removed, so the team sees it once. Returns the file and whether it replaced one.
pub fn share(
    hydra_root: &Path,
    name: &str,
    swarm_folder: &Path,
    toml: bool,
) -> Result<(PathBuf, bool), String> {
    let text = crate::pipeline::export(hydra_root, name)?;
    let title = crate::pipeline::parse(&text)
        .map(|p| p.name)
        .unwrap_or_else(|_| name.to_string());
    let dir = swarm_folder.join(SWARM_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    let (ext, other, body) = if toml {
        ("toml", "md", text.clone())
    } else {
        ("md", "toml", to_md(&title, &text))
    };
    let file = dir.join(format!("{name}.{ext}"));
    let old = dir.join(format!("{name}.{other}"));
    // Only a copy that is this pipeline, shared before: never another file of the same name.
    let old_copy = std::fs::read_to_string(&old).ok().is_some_and(|t| {
        let t = if other == "md" { from_md(&t) } else { Some(t) };
        t.and_then(|t| crate::pipeline::parse(&t).ok())
            .is_some_and(|p| p.name == title)
    });
    let replaced = file.exists() || old_copy;
    std::fs::write(&file, body).map_err(|e| format!("can't write {}: {e}", file.display()))?;
    if old_copy {
        std::fs::remove_file(&old).map_err(|e| format!("can't remove {}: {e}", old.display()))?;
    }
    Ok((file, replaced))
}

/// A pipeline found in a swarm's `pipelines` folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Shared {
    pub file: PathBuf,
    /// The pipeline's text, ready for `pipeline::import`.
    pub text: String,
    pub name: String,
    pub color: Option<u32>,
    pub icon: Option<String>,
    pub stages: usize,
    /// Where its work comes from, e.g. `Slack · #pull-requests`.
    pub source: String,
}

/// The pipelines in the swarm at `swarm_folder`, by name. A file that isn't one is left out.
pub fn shared(swarm_folder: &Path) -> Vec<Shared> {
    let dir = swarm_folder.join(SWARM_DIR);
    let mut out: Vec<Shared> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(|file| {
            let raw = std::fs::read_to_string(&file).ok()?;
            let text = match file.extension()?.to_str()? {
                "md" => from_md(&raw)?,
                "toml" => raw,
                _ => return None,
            };
            let p = crate::pipeline::parse(&text).ok()?;
            Some(Shared {
                source: crate::settings_words::source_short(&p),
                stages: p.stages.len(),
                name: p.name,
                color: p.color,
                icon: p.icon,
                text,
                file,
            })
        })
        .collect();
    out.sort_by_key(|s| s.name.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "name = \"Review PR's\"\nicon = \"git-pull-request\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n";

    #[test]
    fn a_pipeline_round_trips_through_its_md() {
        let md = to_md("Review PR's", ONE);
        assert!(md.starts_with("# Review PR's\n"), "{md}");
        assert!(md.contains("Import from a swarm"), "{md}");
        assert_eq!(from_md(&md).as_deref(), Some(ONE));
        assert_eq!(from_md("# notes\nno block here"), None);
        assert_eq!(from_md("```toml\na = 1\n```").as_deref(), Some("a = 1\n"));
    }

    fn home_with(name: &str, text: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = crate::pipeline::folder(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), text).unwrap();
        home
    }

    #[test]
    fn sharing_makes_the_folder_and_teammates_can_import_it() {
        let home = home_with("prs", ONE);
        let swarm = tempfile::tempdir().unwrap();
        let (file, replaced) = share(home.path(), "prs", swarm.path(), false).unwrap();
        assert_eq!(file, swarm.path().join("pipelines").join("prs.md"));
        assert!(!replaced);
        let (_, replaced) = share(home.path(), "prs", swarm.path(), false).unwrap();
        assert!(replaced, "sharing again updates it");

        // Another file in the folder that isn't a pipeline, and a plain .toml one.
        std::fs::write(swarm.path().join("pipelines").join("notes.md"), "# notes\n").unwrap();
        std::fs::write(
            swarm.path().join("pipelines").join("mail.toml"),
            "name = \"Mail\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n",
        )
        .unwrap();
        let found = shared(swarm.path());
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Mail", "Review PR's"]);
        let prs = &found[1];
        assert_eq!(
            (prs.icon.as_deref(), prs.stages),
            (Some("git-pull-request"), 1)
        );

        // A teammate imports it as a new pipeline of their own.
        let theirs = tempfile::tempdir().unwrap();
        let added = crate::pipeline::import(theirs.path(), &prs.text).unwrap();
        let p = crate::pipeline::load(theirs.path(), &added).unwrap();
        assert_eq!(p.name, "Review PR's");
        assert_eq!(p.icon.as_deref(), Some("git-pull-request"));
    }

    #[test]
    fn a_swarm_that_syncs_toml_gets_the_pipeline_file_itself() {
        assert!(syncs_toml(&[".md".into(), ".toml".into()]));
        assert!(syncs_toml(&["TOML".into()]));
        assert!(!syncs_toml(&[".md".into()]));

        let home = home_with("prs", ONE);
        let swarm = tempfile::tempdir().unwrap();
        let dir = swarm.path().join("pipelines");
        // Shared as .md before the swarm took .toml; now it does.
        share(home.path(), "prs", swarm.path(), false).unwrap();
        let (file, replaced) = share(home.path(), "prs", swarm.path(), true).unwrap();
        assert_eq!(file, dir.join("prs.toml"));
        assert!(replaced, "it replaces the .md copy");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), ONE);
        assert!(!dir.join("prs.md").exists(), "the team sees it once");
        let found = shared(swarm.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Review PR's");

        // Someone else's note of the same name isn't touched.
        std::fs::write(dir.join("prs.md"), "# my own notes\n").unwrap();
        share(home.path(), "prs", swarm.path(), true).unwrap();
        assert!(dir.join("prs.md").exists());
    }

    #[test]
    fn a_swarm_without_pipelines_has_none() {
        let swarm = tempfile::tempdir().unwrap();
        assert!(shared(swarm.path()).is_empty());
    }

    #[test]
    fn sharing_leaves_out_this_machines_autostart() {
        let home = home_with("prs", &format!("{ONE}[autostart]\nenv = \"mine\"\n"));
        let swarm = tempfile::tempdir().unwrap();
        let (file, _) = share(home.path(), "prs", swarm.path(), false).unwrap();
        let md = std::fs::read_to_string(file).unwrap();
        assert!(!md.contains("autostart"), "{md}");
    }
}
