//! Pipelines for the orchestrator (spec: docs/superpowers/specs/2026-10-22-hydraui-orchestrator-core-design.md
//! §2): `<HYDRA_HOME>/hydraui/pipelines/<name>.toml`, its stages, their bars and prompts.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A pipeline as `run_start` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    /// The name the views, notifications and Slack show (the design's orchestrator name).
    pub name: String,
    /// The file's name without `.toml`, as `load` was given it; empty from `parse` alone.
    pub file: String,
    /// Where a ticket's fix runs, as a pattern: `{ticket}` and `{ticket_lower}` are filled
    /// in, e.g. `https://fev2-solutions-{ticket_lower}.dev.iov42.net` (a branch deploy). Prompts
    /// use it as `{feature_env}`.
    pub feature_env: Option<String>,
    /// The bar every stage must clear unless it sets its own, 0 to 100.
    pub bar: u8,
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub id: String,
    pub title: String,
    pub bar: Option<u8>,
    /// The skill the stage's session uses, shown in the views.
    pub skill: Option<String>,
    /// The folder its session starts in, as written (`~` allowed): a project whose skills and
    /// instructions it needs, such as the QA or development one. Takes the place of the
    /// orchestrator's folder and the run's worktree.
    pub folder: Option<String>,
    /// Runs in the run's worktree (made for the first such stage, §3.2).
    pub worktree: bool,
    /// The stage session's first prompt, before `render` fills it in: the file's, or one made
    /// from the skill and title when it has none (§2).
    pub prompt: String,
}

impl Pipeline {
    /// `feature_env` for `ticket`; empty when the pipeline names none.
    pub fn feature_env_for(&self, ticket: &str) -> String {
        self.feature_env
            .as_deref()
            .map_or(String::new(), |pattern| {
                pattern
                    .replace("{ticket_lower}", &ticket.to_lowercase())
                    .replace("{ticket}", ticket)
            })
    }

    /// The bar stage `stage` must clear: its own, else the pipeline's.
    pub fn bar_for(&self, stage: usize) -> u8 {
        self.stages
            .get(stage)
            .and_then(|s| s.bar)
            .unwrap_or(self.bar)
    }
}

/// The bar when a pipeline names none (the user's decision).
pub const DEFAULT_BAR: u8 = 80;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    name: String,
    bar: Option<u8>,
    feature_env: Option<String>,
    #[serde(default, rename = "stage")]
    stages: Vec<FileStage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStage {
    id: String,
    title: String,
    bar: Option<u8>,
    skill: Option<String>,
    folder: Option<String>,
    #[serde(default)]
    worktree: bool,
    prompt: Option<String>,
}

/// The prompt of a stage that has none (§2): a stage added in Pipeline settings. Its skill comes
/// first anyway (`skill_line`).
fn default_prompt(title: &str) -> String {
    format!("Do the \"{title}\" stage of ticket {{ticket}}.")
}

fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_bar(bar: Option<u8>, what: &str) -> Result<(), String> {
    match bar {
        Some(b) if b > 100 => Err(format!("{what}: bar {b} is over 100")),
        _ => Ok(()),
    }
}

/// Reads a pipeline file, naming the first mistake.
pub fn parse(text: &str) -> Result<Pipeline, String> {
    let file: File = toml::from_str(text).map_err(|e| e.message().to_string())?;
    check_bar(file.bar, "the pipeline")?;
    if file.stages.is_empty() {
        return Err("the pipeline has no stages: add a [[stage]]".into());
    }
    let mut stages: Vec<Stage> = Vec::new();
    for s in file.stages {
        if !plain_name(&s.id) {
            return Err(format!(
                "stage id {:?} must be lowercase letters, digits and -",
                s.id
            ));
        }
        if stages.iter().any(|t| t.id == s.id) {
            return Err(format!("stage id {:?} is used twice", s.id));
        }
        check_bar(s.bar, &format!("stage {}", s.id))?;
        let skill = s
            .skill
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let prompt = s.prompt.unwrap_or_else(|| default_prompt(&s.title));
        let folder = s
            .folder
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty());
        stages.push(Stage {
            id: s.id,
            title: s.title,
            bar: s.bar,
            skill,
            folder,
            worktree: s.worktree,
            prompt,
        });
    }
    Ok(Pipeline {
        name: file.name,
        file: String::new(),
        feature_env: file
            .feature_env
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty()),
        bar: file.bar.unwrap_or(DEFAULT_BAR),
        stages,
    })
}

/// `<HYDRA_HOME>/hydraui/pipelines`.
pub fn folder(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("pipelines")
}

/// A pipeline as Pipeline settings edits it (spec §7.4): what the view shows, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub name: String,
    pub bar: u8,
    pub feature_env: Option<String>,
    pub stages: Vec<EditStage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditStage {
    /// The stage's id in the file; `None` for a stage added in the view.
    pub id: Option<String>,
    pub title: String,
    pub skill: Option<String>,
    /// The folder its session starts in; `None`: the orchestrator's folder or the worktree.
    pub folder: Option<String>,
    /// Its own bar; `None` follows the pipeline's.
    pub bar: Option<u8>,
    /// What its session is asked, before hydraui's note; empty: the default prompt.
    pub prompt: String,
}

impl From<&Pipeline> for Edit {
    fn from(p: &Pipeline) -> Self {
        Edit {
            name: p.name.clone(),
            bar: p.bar,
            feature_env: p.feature_env.clone(),
            stages: p
                .stages
                .iter()
                .map(|s| EditStage {
                    id: Some(s.id.clone()),
                    title: s.title.clone(),
                    skill: s.skill.clone(),
                    folder: s.folder.clone(),
                    bar: s.bar,
                    prompt: s.prompt.clone(),
                })
                .collect(),
        }
    }
}

/// What keeps `edit` from being saved, as the design's save bar says it.
pub fn validate(edit: &Edit) -> Result<(), &'static str> {
    if edit.name.trim().is_empty() {
        return Err("Give the orchestrator a name");
    }
    if edit.stages.is_empty() {
        return Err("Add at least one stage");
    }
    if edit.stages.iter().any(|s| s.title.trim().is_empty()) {
        return Err("Every stage needs a name");
    }
    Ok(())
}

/// An id for a new stage titled `title`: its words in lowercase joined by `-`, numbered when
/// `taken` has it.
fn new_id(title: &str, taken: &[String]) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let base = if words.is_empty() {
        "stage".to_string()
    } else {
        words.join("-")
    };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id))
        .expect("an unbounded range has a free id")
}

/// Writes `edit` into the pipeline file `<name>.toml`, keeping everything the view doesn't
/// show: comments, each kept stage's prompt and worktree, unknown keys stay refused by
/// `parse`. Stages are written in `edit`'s order; one it dropped goes. Written whole, then
/// moved into place, so a reader never sees half a file.
pub fn save(hydra_root: &Path, name: &str, edit: &Edit) -> std::io::Result<()> {
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};
    if !plain_name(name) {
        return Err(std::io::Error::other(format!(
            "{name:?} isn't a pipeline name"
        )));
    }
    let file = folder(hydra_root).join(format!("{name}.toml"));
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc: DocumentMut = text.parse().map_err(std::io::Error::other)?;
    doc["name"] = value(edit.name.trim());
    doc["bar"] = value(i64::from(edit.bar));
    match edit
        .feature_env
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(f) => doc["feature_env"] = value(f),
        None => {
            doc.remove("feature_env");
        }
    }
    let old: Vec<Table> = doc
        .get("stage")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    let mut taken: Vec<String> = edit.stages.iter().filter_map(|s| s.id.clone()).collect();
    let mut stages = ArrayOfTables::new();
    for s in &edit.stages {
        let (mut table, id) = match &s.id {
            Some(id) => (
                old.iter()
                    .find(|t| t.get("id").and_then(Item::as_str) == Some(id.as_str()))
                    .cloned()
                    .unwrap_or_default(),
                id.clone(),
            ),
            None => {
                let id = new_id(&s.title, &taken);
                taken.push(id.clone());
                (Table::new(), id)
            }
        };
        table["id"] = value(id);
        table["title"] = value(s.title.trim());
        match &s.skill {
            Some(skill) => table["skill"] = value(skill.as_str()),
            None => {
                table.remove("skill");
            }
        }
        match &s.folder {
            Some(folder) => table["folder"] = value(folder.as_str()),
            None => {
                table.remove("folder");
            }
        }
        match s.bar {
            Some(bar) => table["bar"] = value(i64::from(bar)),
            None => {
                table.remove("bar");
            }
        }
        // An emptied prompt goes back to the default one (§2).
        match s.prompt.trim() {
            "" => {
                table.remove("prompt");
            }
            _ => table["prompt"] = value(s.prompt.trim_end()),
        }
        // Parsed tables are written by their place in the old file: number them afresh so
        // the file follows `edit`'s order.
        table.set_position(stages.len() + 1_000);
        stages.push(table);
    }
    doc["stage"] = Item::ArrayOfTables(stages);
    std::fs::create_dir_all(folder(hydra_root))?;
    let fresh = file.with_extension("toml.saving");
    std::fs::write(&fresh, doc.to_string())?;
    std::fs::rename(&fresh, &file)
}

/// The pipelines in `folder`, by file name without `.toml`, sorted.
pub fn names(hydra_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder(hydra_root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "toml").then(|| path.file_stem()?.to_str().map(str::to_string))?
        })
        .filter(|n| plain_name(n))
        .collect();
    names.sort();
    names
}

/// The pipeline `<name>.toml` in `folder`.
pub fn load(hydra_root: &Path, name: &str) -> Result<Pipeline, String> {
    if !plain_name(name) {
        return Err(format!(
            "{name:?} isn't a pipeline name: lowercase letters, digits and -"
        ));
    }
    let file = folder(hydra_root).join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("can't read pipeline {name} ({}): {e}", file.display()))?;
    let mut pipeline = parse(&text).map_err(|e| format!("pipeline {name}: {e}"))?;
    pipeline.file = name.to_string();
    Ok(pipeline)
}

/// Writes `ticket.toml` when the folder holds no pipeline yet (§2).
pub fn ensure_example(hydra_root: &Path) -> std::io::Result<()> {
    let dir = folder(hydra_root);
    let has_one = std::fs::read_dir(&dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "toml"))
    });
    if has_one {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("ticket.toml"), EXAMPLE)
}

/// Values a stage prompt's placeholders are filled with.
pub struct PromptVars<'a> {
    pub ticket: &'a str,
    pub run: &'a str,
    pub stage: &'a str,
    /// The previous stage's handoff (§4); empty for the first stage.
    pub handoff: &'a str,
    /// The pipeline's `feature_env` for the ticket; empty without one.
    pub feature_env: &'a str,
}

/// The stage session's first prompt as the pipeline writes it: its placeholders filled, then
/// the orchestrator's `brief` when it gave one. hydraui adds a short note of its own after it
/// (`runs`).
pub fn render(prompt: &str, vars: &PromptVars, brief: &str) -> String {
    let mut text = prompt
        .trim_end()
        .replace("{ticket_lower}", &vars.ticket.to_lowercase())
        .replace("{ticket}", vars.ticket)
        .replace("{run}", vars.run)
        .replace("{stage}", vars.stage)
        .replace("{handoff}", vars.handoff)
        .replace("{feature_env}", vars.feature_env);
    let brief = brief.trim();
    if !brief.is_empty() {
        text.push_str("\n\n");
        text.push_str(brief);
    }
    text
}

/// The pipeline written when there is none: the four stages of the ticket pipeline with plain
/// prompts, enough to walk a ticket through by hand (phase 1's gate).
pub const EXAMPLE: &str = r#"# A ticket through four stages. Each stage is a fresh Claude session; it ends with
# stage_report, and the next stage starts only when the rating clears the bar. A stage may
# name the skill its session uses (skill = "..."); a stage with no prompt gets one from it.
name = "ticket"
bar = 80

[[stage]]
id = "replicate"
title = "Understand and replicate"
prompt = """
Read ticket {ticket}. Decide whether it has enough information to act on. If it does, reproduce
the problem it describes and record how.
"""

[[stage]]
id = "develop"
title = "Develop"
worktree = true
prompt = """
Fix ticket {ticket} in this worktree, test first. What the previous stage found: {handoff}
Commit the fix when its tests pass.
"""

[[stage]]
id = "review"
title = "Review"
worktree = true
prompt = """
Open a pull request for the fix to ticket {ticket} from this worktree's branch, and see it through
review until it is approved. What the previous stage handed over: {handoff}
"""

[[stage]]
id = "verify"
title = "Verify"
worktree = true
prompt = """
Prove that ticket {ticket} is fixed: repeat the reproduction and show it no longer fails.
What the previous stage handed over: {handoff}
"""
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "name = \"t\"\n[[stage]]\nid = \"replicate\"\ntitle = \"Replicate\"\nprompt = \"Read {ticket} ({run}, {stage}). {handoff}\"\n[[stage]]\nid = \"develop\"\ntitle = \"Develop\"\nbar = 85\nworktree = true\nprompt = \"Fix it.\"\n";

    #[test]
    fn bars_default_to_80_and_a_stage_can_set_its_own() {
        let p = parse(TWO).unwrap();
        assert_eq!((p.bar, p.bar_for(0), p.bar_for(1)), (80, 80, 85));
        assert!(p.stages[1].worktree && !p.stages[0].worktree);
        assert_eq!(p.stages[0].title, "Replicate");
    }

    #[test]
    fn mistakes_are_named() {
        for (text, says) in [
            ("name = \"t\"\n", "no stages"),
            (
                "name = \"t\"\nbar = 101\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n",
                "bar",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nbar = 120\nprompt = \"p\"\n",
                "bar",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"A b\"\ntitle = \"A\"\nprompt = \"p\"\n",
                "A b",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n[[stage]]\nid = \"a\"\ntitle = \"B\"\nprompt = \"q\"\n",
                "twice",
            ),
            (
                "name = \"t\"\nspeed = 2\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n",
                "speed",
            ),
            (
                "name = \"t\"\n[[stage]]\nid = \"a\"\nprompt = \"p\"\n",
                "title",
            ),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn a_prompt_gets_its_values_and_the_brief() {
        let p = parse(TWO).unwrap();
        let vars = PromptVars {
            ticket: "INTH-1",
            run: "INTH-1-1",
            stage: "replicate",
            handoff: "see log",
            feature_env: "",
        };
        let text = render(&p.stages[0].prompt, &vars, "Focus on the API.");
        assert_eq!(
            text,
            "Read INTH-1 (INTH-1-1, replicate). see log\n\nFocus on the API."
        );
        assert_eq!(render("Go.", &vars, "  "), "Go.");
    }

    #[test]
    fn the_feature_env_follows_the_ticket() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(
            folder(d.path()).join("t.toml"),
            "name = \"t\"\nfeature_env = \"https://fev2-solutions-{ticket_lower}.dev.iov42.net\"\n\
             [[stage]]\nid = \"verify\"\ntitle = \"Verify\"\nprompt = \"test {ticket} against {feature_env}\"\n",
        )
        .unwrap();
        let p = load(d.path(), "t").unwrap();
        let env = p.feature_env_for("INTH-161");
        assert_eq!(env, "https://fev2-solutions-inth-161.dev.iov42.net");
        let vars = PromptVars {
            ticket: "INTH-161",
            run: "INTH-161-1",
            stage: "verify",
            handoff: "",
            feature_env: &env,
        };
        assert_eq!(
            render(&p.stages[0].prompt, &vars, ""),
            "test INTH-161 against https://fev2-solutions-inth-161.dev.iov42.net"
        );
        // Settings keep it, change it and clear it.
        let mut edit = Edit::from(&p);
        assert_eq!(
            edit.feature_env.as_deref(),
            Some("https://fev2-solutions-{ticket_lower}.dev.iov42.net")
        );
        edit.feature_env = None;
        save(d.path(), "t", &edit).unwrap();
        assert_eq!(load(d.path(), "t").unwrap().feature_env_for("INTH-161"), "");
    }

    #[test]
    fn the_example_parses_and_is_written_once() {
        assert_eq!(parse(EXAMPLE).unwrap().stages.len(), 4);
        let d = tempfile::tempdir().unwrap();
        ensure_example(d.path()).unwrap();
        let file = folder(d.path()).join("ticket.toml");
        let loaded = load(d.path(), "ticket").unwrap();
        assert_eq!(
            (loaded.name.as_str(), loaded.file.as_str()),
            ("ticket", "ticket")
        );
        std::fs::write(&file, "edited").unwrap();
        ensure_example(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited");
    }

    #[test]
    fn a_stage_without_a_prompt_gets_one_from_its_skill() {
        let p = parse(
            "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"Replicate\"\nskill = \"QA-Skills\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"Check\"\n",
        )
        .unwrap();
        assert_eq!(p.stages[0].skill.as_deref(), Some("QA-Skills"));
        assert_eq!(
            p.stages[0].prompt,
            "Do the \"Replicate\" stage of ticket {ticket}."
        );
        assert_eq!(p.stages[1].skill, None);
        assert_eq!(
            p.stages[1].prompt,
            "Do the \"Check\" stage of ticket {ticket}."
        );
        assert_eq!(p.file, "");
    }

    const COMMENTED: &str = "# The team's pipeline.\nname = \"Tickets\"\nbar = 80\n\n\
# Reproduce first.\n[[stage]]\nid = \"replicate\"\ntitle = \"Replicate\"\nworktree = false\n\
prompt = \"\"\"\nRead {ticket}.\n\"\"\"\n\n[[stage]]\nid = \"develop\"\ntitle = \"Develop\"\n\
bar = 85\nworktree = true\nprompt = \"Fix it.\"\n";

    fn edit_of(p: &Pipeline) -> Edit {
        Edit::from(p)
    }

    #[test]
    fn saving_keeps_prompts_and_comments() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        let file = folder(d.path()).join("tickets.toml");
        std::fs::write(&file, COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        edit.name = "INTH SLA tickets".into();
        edit.bar = 75;
        edit.stages[0].skill = Some("QA-Skills".into());
        edit.stages[1].bar = None;
        save(d.path(), "tickets", &edit).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        for kept in [
            "# The team's pipeline.",
            "# Reproduce first.",
            "Read {ticket}.",
            "Fix it.",
            "worktree = true",
        ] {
            assert!(text.contains(kept), "{kept}: {text}");
        }
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!((p.name.as_str(), p.bar), ("INTH SLA tickets", 75));
        assert_eq!(p.stages[0].skill.as_deref(), Some("QA-Skills"));
        assert_eq!(p.stages[1].bar, None);
        assert!(p.stages[1].worktree);
    }

    #[test]
    fn a_new_stage_gets_a_unique_id_from_its_title() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("tickets.toml"), COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        for title in ["Security review", "Develop"] {
            edit.stages.push(EditStage {
                id: None,
                title: title.into(),
                skill: Some("Security review".into()),
                folder: None,
                bar: Some(90),
                prompt: String::new(),
            });
        }
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        let ids: Vec<&str> = p.stages.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["replicate", "develop", "security-review", "develop-2"]
        );
        assert_eq!(
            p.stages[2].prompt,
            "Do the \"Security review\" stage of ticket {ticket}."
        );
        assert_eq!(p.bar_for(2), 90);
    }

    #[test]
    fn reordering_and_removing_stages() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("tickets.toml"), COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        edit.stages.swap(0, 1);
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!(p.stages[0].id, "develop");
        assert_eq!(p.stages[1].prompt.trim(), "Read {ticket}.");
        edit.stages.remove(0);
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!(p.stages.len(), 1);
        assert_eq!(p.stages[0].id, "replicate");
    }

    #[test]
    fn a_stage_can_start_in_a_folder() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(
            folder(d.path()).join("t.toml"),
            "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nfolder = \"~/Projects/QA-Skills\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"B\"\n",
        )
        .unwrap();
        let p = load(d.path(), "t").unwrap();
        assert_eq!(p.stages[0].folder.as_deref(), Some("~/Projects/QA-Skills"));
        assert_eq!(p.stages[1].folder, None);
        let mut edit = Edit::from(&p);
        assert_eq!(
            edit.stages[0].folder.as_deref(),
            Some("~/Projects/QA-Skills")
        );
        edit.stages[0].folder = None;
        edit.stages[1].folder = Some("~/Projects/iov42-agents".into());
        save(d.path(), "t", &edit).unwrap();
        let p = load(d.path(), "t").unwrap();
        assert_eq!(p.stages[0].folder, None);
        assert_eq!(
            p.stages[1].folder.as_deref(),
            Some("~/Projects/iov42-agents")
        );
    }

    #[test]
    fn settings_edit_each_stages_prompt() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder(d.path())).unwrap();
        std::fs::write(folder(d.path()).join("tickets.toml"), COMMENTED).unwrap();
        let mut edit = edit_of(&load(d.path(), "tickets").unwrap());
        // What the stage uses now is what the settings show.
        assert_eq!(edit.stages[0].prompt.trim(), "Read {ticket}.");
        assert_eq!(edit.stages[1].prompt, "Fix it.");
        edit.stages[0].prompt = "replicate {ticket}".into();
        edit.stages.push(EditStage {
            id: None,
            title: "Verify".into(),
            skill: None,
            folder: None,
            bar: None,
            prompt: "test {ticket} against {feature_env}".into(),
        });
        save(d.path(), "tickets", &edit).unwrap();
        let p = load(d.path(), "tickets").unwrap();
        assert_eq!(p.stages[0].prompt, "replicate {ticket}");
        assert_eq!(p.stages[2].prompt, "test {ticket} against {feature_env}");
        // A prompt left empty falls back to the default one.
        let mut edit = edit_of(&p);
        edit.stages[1].prompt = "  ".into();
        save(d.path(), "tickets", &edit).unwrap();
        assert_eq!(
            load(d.path(), "tickets").unwrap().stages[1].prompt,
            "Do the \"Develop\" stage of ticket {ticket}."
        );
    }

    #[test]
    fn validation_names_what_is_missing() {
        let mut edit = edit_of(&parse(COMMENTED).unwrap());
        assert_eq!(validate(&edit), Ok(()));
        edit.stages[1].title = " ".into();
        assert_eq!(validate(&edit), Err("Every stage needs a name"));
        edit.name = "".into();
        assert_eq!(validate(&edit), Err("Give the orchestrator a name"));
        edit.name = "x".into();
        edit.stages.clear();
        assert_eq!(validate(&edit), Err("Add at least one stage"));
    }

    #[test]
    fn names_lists_the_pipeline_files() {
        let d = tempfile::tempdir().unwrap();
        assert!(names(d.path()).is_empty());
        let dir = folder(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["zeta.toml", "alpha.toml", "notes.md", "Bad Name.toml"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        assert_eq!(names(d.path()), ["alpha", "zeta"]);
    }

    #[test]
    fn a_pipeline_name_is_a_plain_file_name() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path(), "../x").unwrap_err().contains("../x"));
        assert!(load(d.path(), "missing").unwrap_err().contains("missing"));
    }
}
