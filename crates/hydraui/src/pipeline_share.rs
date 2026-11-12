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

/// A short fingerprint of a shared pipeline's text (FNV-1a, so it's the same on every machine
/// and every build), to tell when the copy in the swarm has changed.
pub fn version(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.trim().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Adds `s`, found in swarm `swarm_id` (`swarm_name`), as a new pipeline here, recording where
/// it came from so a newer version can be offered (core spec §70). Returns its file name.
pub fn import_shared(
    hydra_root: &Path,
    s: &Shared,
    swarm_id: &str,
    swarm_name: &str,
) -> Result<String, String> {
    let name = crate::pipeline::import(hydra_root, &s.text)?;
    let stem = s
        .file
        .file_stem()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let v = version(&s.text);
    edit_file(hydra_root, &name, |doc| {
        let mut t = toml_edit::Table::new();
        t["swarm"] = toml_edit::value(swarm_id);
        t["swarm_name"] = toml_edit::value(swarm_name);
        t["file"] = toml_edit::value(stem.as_str());
        t["version"] = toml_edit::value(v.as_str());
        doc["shared"] = toml_edit::Item::Table(t);
    })?;
    Ok(name)
}

/// A newer version of an imported pipeline, found in its swarm.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub version: String,
    pub text: String,
    /// The swarm's name now.
    pub swarm_name: String,
}

/// The newer version of `p` in the swarm it came from, if there is one it hasn't skipped.
/// `swarm` finds a swarm you're in by id: its folder and name.
pub fn newer(
    p: &crate::pipeline::Pipeline,
    swarm: impl Fn(&str) -> Option<(PathBuf, String)>,
) -> Option<Update> {
    let from = p.shared.as_ref()?;
    let (folder, swarm_name) = swarm(&from.swarm)?;
    let dir = folder.join(SWARM_DIR);
    let text = ["toml", "md"].iter().find_map(|ext| {
        let raw = std::fs::read_to_string(dir.join(format!("{}.{ext}", from.file))).ok()?;
        if *ext == "md" {
            from_md(&raw)
        } else {
            Some(raw)
        }
    })?;
    crate::pipeline::parse(&text).ok()?;
    let v = version(&text);
    (v != from.version && from.skipped.as_deref() != Some(v.as_str())).then_some(Update {
        version: v,
        text,
        swarm_name,
    })
}

/// Takes update `u` into pipeline `name`: its stages and settings become the shared ones, but
/// it keeps what is this machine's: its name here, `[autostart]`, `[schedule]`, `[mail]`,
/// whether its Linear or Slack trigger is on, and where it came from (now at `u`'s version).
pub fn update(hydra_root: &Path, name: &str, u: &Update) -> Result<(), String> {
    use toml_edit::{DocumentMut, Item, value};
    let path = crate::pipeline::folder(hydra_root).join(format!("{name}.toml"));
    let local: DocumentMut = std::fs::read_to_string(&path)
        .map_err(|e| format!("can't read {name}: {e}"))?
        .parse()
        .map_err(|e| format!("{name}: {e}"))?;
    let mut doc: DocumentMut = u
        .text
        .parse()
        .map_err(|e| format!("the shared version: {e}"))?;
    for key in ["name", "autostart", "schedule", "mail", "shared"] {
        match local.get(key) {
            Some(item) => doc[key] = item.clone(),
            None => {
                doc.remove(key);
            }
        }
    }
    for trigger in ["linear", "slack"] {
        let on = local
            .get(trigger)
            .and_then(|t| t.get("watch"))
            .and_then(Item::as_bool)
            .unwrap_or(false);
        if let Some(t) = doc.get_mut(trigger).and_then(Item::as_table_mut) {
            t["watch"] = value(on);
        }
    }
    if let Some(t) = doc.get_mut("shared").and_then(Item::as_table_mut) {
        t["version"] = value(u.version.as_str());
        t.remove("skipped");
    }
    let text = doc.to_string();
    crate::pipeline::parse(&text).map_err(|e| format!("the shared version doesn't fit: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// **Not now**: pipeline `name` isn't offered version `v` again, only a newer one.
pub fn skip(hydra_root: &Path, name: &str, v: &str) -> Result<(), String> {
    edit_file(hydra_root, name, |doc| {
        if let Some(t) = doc
            .get_mut("shared")
            .and_then(toml_edit::Item::as_table_mut)
        {
            t["skipped"] = toml_edit::value(v);
        }
    })
}

fn edit_file(
    hydra_root: &Path,
    name: &str,
    f: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<(), String> {
    let path = crate::pipeline::folder(hydra_root).join(format!("{name}.toml"));
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)
        .map_err(|e| format!("can't read {name}: {e}"))?
        .parse()
        .map_err(|e| format!("{name}: {e}"))?;
    f(&mut doc);
    let text = doc.to_string();
    crate::pipeline::parse(&text).map_err(|e| format!("{name}: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("can't write {}: {e}", path.display()))
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
    fn an_imported_pipeline_is_offered_its_owners_updates() {
        let swarm = tempfile::tempdir().unwrap();
        let folder = swarm.path().to_path_buf();
        let find = |id: &str| (id == "sw1").then(|| (folder.clone(), "Team".to_string()));

        // The owner shares it.
        let owner = home_with("prs", ONE);
        share(owner.path(), "prs", swarm.path(), true).unwrap();

        // A teammate imports it, then turns its Slack trigger on and sets it to start here.
        let mine = tempfile::tempdir().unwrap();
        let found = shared(swarm.path());
        let name = import_shared(mine.path(), &found[0], "sw1", "Team").unwrap();
        let p = crate::pipeline::load(mine.path(), &name).unwrap();
        let from = p.shared.clone().unwrap();
        assert_eq!((from.swarm.as_str(), from.file.as_str()), ("sw1", "prs"));
        assert_eq!(newer(&p, find), None, "nothing newer yet");

        // The owner adds a stage and shares again.
        let two = format!("{ONE}[[stage]]\nid = \"b\"\ntitle = \"B\"\n");
        std::fs::write(crate::pipeline::folder(owner.path()).join("prs.toml"), &two).unwrap();
        share(owner.path(), "prs", swarm.path(), true).unwrap();
        let u = newer(&p, find).expect("the owner's update is offered");
        assert_eq!(u.swarm_name, "Team");

        // Not now: that version isn't offered again.
        skip(mine.path(), &name, &u.version).unwrap();
        let p = crate::pipeline::load(mine.path(), &name).unwrap();
        assert_eq!(newer(&p, find), None);

        // A later version is, and updating keeps what is this machine's.
        let three =
            format!("{two}[slack]\nchannel = \"C0C217CER6G\"\nlabel = \"prs\"\nwatch = true\n");
        std::fs::write(
            crate::pipeline::folder(owner.path()).join("prs.toml"),
            &three,
        )
        .unwrap();
        share(owner.path(), "prs", swarm.path(), true).unwrap();
        let u = newer(&p, find).expect("a newer version is offered again");
        update(mine.path(), &name, &u).unwrap();
        let p = crate::pipeline::load(mine.path(), &name).unwrap();
        assert_eq!(p.stages.len(), 2, "its stages are the owner's");
        assert!(
            p.slack.as_ref().is_some_and(|s| !s.watch),
            "its trigger stays off here"
        );
        assert_eq!(p.name, "Review PR's");
        assert_eq!(newer(&p, find), None, "up to date");
        // Re-exported, where it came from stays here.
        assert!(
            !crate::pipeline::export(mine.path(), &name)
                .unwrap()
                .contains("[shared]")
        );
        // A swarm it isn't in any more offers nothing.
        assert_eq!(newer(&p, |_| None), None);
    }

    #[test]
    fn versions_are_stable_and_ignore_edge_whitespace() {
        assert_eq!(version("a = 1\n"), version("a = 1"));
        assert_ne!(version("a = 1"), version("a = 2"));
        // FNV-1a 64 of "a", as published.
        assert_eq!(version("a"), "af63dc4c8601ec8c");
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
