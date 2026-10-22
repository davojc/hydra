//! Pipelines for the orchestrator (spec: docs/superpowers/specs/2026-10-22-hydraui-orchestrator-core-design.md
//! §2): `<HYDRA_HOME>/hydraui/pipelines/<name>.toml`, its stages, their bars and prompts.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A pipeline as `run_start` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub name: String,
    /// The bar every stage must clear unless it sets its own, 0 to 100.
    pub bar: u8,
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub id: String,
    pub title: String,
    pub bar: Option<u8>,
    /// Runs in the run's worktree (made for the first such stage, §3.2).
    pub worktree: bool,
    /// The stage session's first prompt, before `render` fills it in.
    pub prompt: String,
}

impl Pipeline {
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
    #[serde(default, rename = "stage")]
    stages: Vec<FileStage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStage {
    id: String,
    title: String,
    bar: Option<u8>,
    #[serde(default)]
    worktree: bool,
    prompt: String,
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
        stages.push(Stage {
            id: s.id,
            title: s.title,
            bar: s.bar,
            worktree: s.worktree,
            prompt: s.prompt,
        });
    }
    Ok(Pipeline {
        name: file.name,
        bar: file.bar.unwrap_or(DEFAULT_BAR),
        stages,
    })
}

/// `<HYDRA_HOME>/hydraui/pipelines`.
pub fn folder(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("pipelines")
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
    parse(&text).map_err(|e| format!("pipeline {name}: {e}"))
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
}

/// Added to every stage prompt, so a pipeline file can't leave it out (§2).
pub const CLOSING: &str = "When this stage is finished, or you find it can't be done, call \
stage_report once: outcome done or blocked, a score from 0 to 100 for your confidence that the \
stage is done well, a short summary, the evidence (tests, commands, files, links) and the reasons \
for the score, and a handoff for the next stage. Report blocked rather than guess. Do only this \
stage's work.";

/// The stage session's first prompt: `prompt` with its placeholders filled, the orchestrator's
/// `brief` when it gave one, then `CLOSING`.
pub fn render(prompt: &str, vars: &PromptVars, brief: &str) -> String {
    let mut text = prompt
        .trim_end()
        .replace("{ticket}", vars.ticket)
        .replace("{run}", vars.run)
        .replace("{stage}", vars.stage)
        .replace("{handoff}", vars.handoff);
    let brief = brief.trim();
    if !brief.is_empty() {
        text.push_str("\n\n");
        text.push_str(brief);
    }
    text.push_str("\n\n");
    text.push_str(CLOSING);
    text
}

/// The pipeline written when there is none: the four stages of the ticket pipeline with plain
/// prompts, enough to walk a ticket through by hand (phase 1's gate).
pub const EXAMPLE: &str = r#"# A ticket through four stages. Each stage is a fresh Claude session; it ends with
# stage_report, and the next stage starts only when the rating clears the bar.
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
    fn a_prompt_gets_its_values_the_brief_and_always_the_closing() {
        let p = parse(TWO).unwrap();
        let vars = PromptVars {
            ticket: "INTH-1",
            run: "INTH-1-1",
            stage: "replicate",
            handoff: "see log",
        };
        let text = render(&p.stages[0].prompt, &vars, "Focus on the API.");
        assert!(
            text.starts_with("Read INTH-1 (INTH-1-1, replicate). see log"),
            "{text}"
        );
        assert!(text.contains("\n\nFocus on the API.\n"), "{text}");
        assert!(text.ends_with(CLOSING), "{text}");
        let bare = render("Go.", &vars, "  ");
        assert_eq!(bare, format!("Go.\n\n{CLOSING}"));
    }

    #[test]
    fn the_example_parses_and_is_written_once() {
        assert_eq!(parse(EXAMPLE).unwrap().stages.len(), 4);
        let d = tempfile::tempdir().unwrap();
        ensure_example(d.path()).unwrap();
        let file = folder(d.path()).join("ticket.toml");
        assert_eq!(load(d.path(), "ticket").unwrap().name, "ticket");
        std::fs::write(&file, "edited").unwrap();
        ensure_example(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited");
    }

    #[test]
    fn a_pipeline_name_is_a_plain_file_name() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path(), "../x").unwrap_err().contains("../x"));
        assert!(load(d.path(), "missing").unwrap_err().contains("missing"));
    }
}
