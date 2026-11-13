//! A pipeline stage Jev does (email triage spec §4): `kind = "jev"`. No Claude session: hydraui
//! asks Jev one choice question about a file in the run's folder and files the answer as the
//! stage's report, Jev's confidence its score. So the same text gets the same answer, and a
//! score under the stage's bar asks a person, as any stage's does. Pure but for reading the
//! input; the call itself is `jev::ask`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::jev::{JevSettings, Verdict};

/// The file a Jev stage reads when it names none: what the mail trigger writes (spec §3).
pub const DEFAULT_INPUT: &str = "email.md";

/// The most of the input Jev is sent, in characters.
pub const MAX_INPUT: usize = 20_000;

/// What `none` means when the stage doesn't say.
pub const NONE_TEXT: &str = "None of these, or it can't be told from the text";

/// `options` with `none` offered, as every Jev stage has it: added with `NONE_TEXT` when left
/// out. Pipeline settings hides that row, so what it builds goes through this too.
pub fn with_none(mut options: BTreeMap<String, String>) -> BTreeMap<String, String> {
    options
        .entry(crate::jev::NONE.to_string())
        .or_insert_with(|| NONE_TEXT.to_string());
    options
}

/// A Jev stage's question, as its `[[stage]]` sets it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JevStage {
    /// A file in the run's folder.
    pub input: String,
    /// The Jev model; `None` is `JevSettings`' default.
    pub model: Option<String>,
    pub instructions: String,
    /// Each option's name and what it means; `none` is always among them.
    pub options: BTreeMap<String, String>,
    /// Categories that end the run here (`[stage.route] general = "end"`): the pipeline
    /// doesn't deal with them, so the run stops quietly and its email goes back unclaimed.
    #[serde(default)]
    pub ends: BTreeSet<String>,
}

impl JevStage {
    /// Checks and completes what a `[[stage]]` gave: instructions, at least two options with
    /// plain names, a plain file name for the input. `none` is added when left out.
    pub fn new(
        input: Option<String>,
        model: Option<String>,
        instructions: Option<String>,
        options: BTreeMap<String, String>,
        id: &str,
    ) -> Result<Self, String> {
        let instructions = instructions.unwrap_or_default().trim().to_string();
        if instructions.is_empty() {
            return Err(format!(
                "stage {id}: a Jev stage needs instructions, the question Jev answers"
            ));
        }
        if options
            .keys()
            .filter(|k| k.as_str() != crate::jev::NONE)
            .count()
            < 2
        {
            return Err(format!(
                "stage {id}: a Jev stage needs at least two options under [stage.options]"
            ));
        }
        for (name, means) in &options {
            let plain = !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !plain {
                return Err(format!(
                    "stage {id}: option {name:?} must be lowercase letters, digits and _"
                ));
            }
            if means.trim().is_empty() {
                return Err(format!("stage {id}: say what {name} means"));
            }
        }
        let options = with_none(options);
        let input = input
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| DEFAULT_INPUT.to_string());
        if input.contains(['/', '\\']) || input.starts_with('.') {
            return Err(format!(
                "stage {id}: input {input:?} must be a file name in the run's folder"
            ));
        }
        Ok(Self {
            input,
            model: model
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
            instructions,
            options,
            ends: BTreeSet::new(),
        })
    }

    /// Takes `[stage.route]`: a category set to `"end"` (or `"finish"`) ends the run; any other
    /// category goes on to the next stage.
    pub fn with_route(
        mut self,
        route: &BTreeMap<String, String>,
        id: &str,
    ) -> Result<Self, String> {
        for (name, to) in route {
            if name == crate::jev::NONE || !self.options.contains_key(name) {
                return Err(format!(
                    "stage {id}: [stage.route] names {name:?}, which isn't one of its options"
                ));
            }
            match to.trim() {
                "end" | "finish" => {
                    self.ends.insert(name.clone());
                }
                "next" => {}
                other => {
                    return Err(format!(
                        "stage {id}: route {name} = {other:?}: write \"end\" to end the run \
                         there, or leave it out to go on"
                    ));
                }
            }
        }
        Ok(self)
    }

    /// The settings `jev::ask` takes: a score of `bar` percent is enough.
    pub fn settings(&self, bar: u8) -> JevSettings {
        let base = JevSettings::default();
        JevSettings {
            model: self.model.clone().unwrap_or(base.model),
            min_confidence: f64::from(bar.min(100)) / 100.0,
            instructions: self.instructions.clone(),
            options: self.options.clone(),
        }
    }
}

/// What's wrong with a Jev step as Pipeline settings left it, in words for its save bar; `None`
/// when `JevStage::new` would take it.
pub fn problem(j: &JevStage) -> Option<&'static str> {
    if j.instructions.trim().is_empty() {
        return Some("Give the Jev step its question");
    }
    if j.options
        .keys()
        .filter(|k| k.as_str() != crate::jev::NONE)
        .count()
        < 2
    {
        return Some("A Jev step needs at least two categories");
    }
    for (name, means) in &j.options {
        let plain = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !plain {
            return Some("Name categories with lowercase letters, digits and _, like how_to");
        }
        if means.trim().is_empty() {
            return Some("Say what each Jev category means");
        }
    }
    let input = j.input.trim();
    if input.is_empty() || input.contains(['/', '\\']) || input.starts_with('.') {
        return Some("A Jev step reads a file name in the run's folder, like email.md");
    }
    None
}

/// The category a Jev report handed over (`category: how_to`).
pub fn category(handoff: &str) -> Option<&str> {
    handoff
        .lines()
        .find_map(|l| l.trim().strip_prefix("category:"))
        .map(str::trim)
        .filter(|c| !c.is_empty())
}

/// Why the run ends at this Jev stage, when Jev sorted it into a category set to end the run.
pub fn ended_by(j: &JevStage, handoff: &str) -> Option<String> {
    let c = category(handoff)?;
    j.ends
        .contains(c)
        .then(|| format!("Not for this pipeline: Jev sorted it as {c}"))
}

/// The run's folder, beside its log: `<root>/hydraui/runs/<run>/`.
pub fn run_folder(hydra_root: &Path, run: &str) -> PathBuf {
    crate::run_log::file_for(hydra_root, run).with_extension("")
}

/// The text Jev is asked about: the stage's input file in the run's folder, or else in the
/// folder a `[mail]` look kept its ticket's thread in, cut to `MAX_INPUT` characters.
pub fn input_text(
    hydra_root: &Path,
    run: &str,
    ticket: &str,
    stage: &JevStage,
) -> Result<String, String> {
    let own = run_folder(hydra_root, run).join(&stage.input);
    let mailed = crate::mail_watch::thread_folder(hydra_root, ticket).join(&stage.input);
    let path = if own.exists() || !mailed.exists() {
        own
    } else {
        mailed
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "the run has no {} for Jev to read ({}): {e}",
            stage.input,
            path.display()
        )
    })?;
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("the run's {} is empty", stage.input));
    }
    Ok(text.chars().take(MAX_INPUT).collect())
}

/// The `stage_report` arguments for Jev's verdict `v`: its pick and confidence as the summary
/// and score, every option's probability as evidence, and the pick in the handoff for the
/// next stage. A pick of `none` is blocked: a person decides.
pub fn report(v: &Verdict, model: &str) -> Value {
    let mut ranked: Vec<(&String, &f64)> = v.probabilities.iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(a.1).then(a.0.cmp(b.0)));
    let mut evidence: Vec<Value> = ranked
        .iter()
        .map(|(k, p)| json!({"kind": "note", "detail": format!("{k} {p:.2}")}))
        .collect();
    evidence.push(json!({"kind": "note", "detail": format!("model {model}")}));
    let runner_up = ranked
        .iter()
        .find(|(k, _)| **k != v.choice)
        .map(|(k, p)| format!("next most likely: {k} at {p:.2}"))
        .unwrap_or_else(|| "no other option was likely".to_string());
    let score = (v.confidence * 100.0).round().clamp(0.0, 100.0) as u8;
    let none = v.choice == crate::jev::NONE;
    json!({
        "outcome": if none { "blocked" } else { "done" },
        "score": score,
        "summary": if none {
            "Jev couldn't tell which option this is: a person decides.".to_string()
        } else {
            format!("Jev: {} ({:.2})", v.choice, v.confidence)
        },
        "evidence": evidence,
        "reasons": [
            {"side": "for", "text": format!(
                "Jev picked {} with confidence {:.2}", v.choice, v.confidence)},
            {"side": "against", "text": runner_up},
        ],
        "handoff": format!("category: {}\nconfidence: {:.2}", v.choice, v.confidence),
        // Shown only when a person must decide: Jev wasn't sure enough, or couldn't tell.
        "ask": if none {
            "Read the email and decide what kind it is".to_string()
        } else {
            format!("Confirm Jev's pick of {}, or stop the run", v.choice)
        },
    })
}

/// The `stage_report` arguments when Jev couldn't be asked: blocked, with why.
pub fn failed(why: &str) -> Value {
    json!({
        "outcome": "blocked",
        "score": 0,
        "summary": format!("Jev couldn't classify it: {why}"),
        "evidence": [{"kind": "note", "detail": why}],
        "reasons": [{"side": "against", "text": "Jev gave no answer"}],
        "handoff": "",
    })
}

/// Starting stage `s`'s Jev question, bar and run's ticket, when it's a Jev stage.
pub fn of(
    reg: &crate::mcp::registry::Registry,
    s: &crate::runs::StartStage,
) -> Option<(JevStage, u8, String)> {
    let r = reg.runs.run(&s.run)?;
    let j = r.pipeline.stages.get(s.stage)?.jev.clone()?;
    Some((j, r.pipeline.bar_for(s.stage), r.ticket.clone()))
}

/// Files `args` as starting stage `s`'s report: it takes the place of a session, under a token
/// of its own, and goes through `stage_report` as a session's report would, bar and all.
/// Returns the event to broadcast.
pub fn file(
    reg: &mut crate::mcp::registry::Registry,
    s: &crate::runs::StartStage,
    args: &Value,
) -> Result<Option<crate::mcp::server::McpEvent>, String> {
    let token = format!("jev:{}:{}", s.run, s.label);
    reg.runs.link(&s.run, s.stage, &token, "Jev");
    match crate::mcp::run_tools::call(reg, &token, "stage_report", args) {
        Some((out, _)) if out.is_error => Err(out.text),
        Some((_, event)) => Ok(event),
        None => Err("hydraui has no stage_report".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn email_stage() -> JevStage {
        JevStage::new(
            None,
            Some("jev-2026-10".into()),
            Some("A customer email. What are they asking for?".into()),
            opts(&[("how_to", "How to use it"), ("user_error", "Their mistake")]),
            "classify",
        )
        .unwrap()
    }

    #[test]
    fn a_jev_stage_reads_email_md_and_always_offers_none() {
        let s = email_stage();
        assert_eq!(s.input, "email.md");
        assert_eq!(s.options.len(), 3);
        assert_eq!(s.options["none"], NONE_TEXT);
        // What Pipeline settings builds (it hides `none`) completes to what loads.
        let shown: BTreeMap<String, String> = s
            .options
            .iter()
            .filter(|(k, _)| k.as_str() != crate::jev::NONE)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(with_none(shown), s.options);
        let set = s.settings(70);
        assert_eq!(set.model, "jev-2026-10");
        assert!((set.min_confidence - 0.7).abs() < 1e-9);
        assert_eq!(
            set.instructions,
            "A customer email. What are they asking for?"
        );
        assert_eq!(set.options, s.options);
        let unpinned = JevStage::new(None, None, Some("Q".into()), s.options.clone(), "c").unwrap();
        assert_eq!(unpinned.settings(80).model, JevSettings::default().model);
    }

    #[test]
    fn a_jev_stage_that_makes_no_sense_says_why() {
        let two = opts(&[("a", "A"), ("b", "B")]);
        let err = |input: Option<&str>, q: Option<&str>, o: BTreeMap<String, String>| {
            JevStage::new(
                input.map(str::to_string),
                None,
                q.map(str::to_string),
                o,
                "classify",
            )
            .unwrap_err()
        };
        assert!(err(None, None, two.clone()).contains("instructions"));
        assert!(err(None, Some(" "), two.clone()).contains("instructions"));
        assert!(err(None, Some("Q"), opts(&[("a", "A")])).contains("two options"));
        assert!(err(None, Some("Q"), opts(&[("a", "A"), ("none", "N")])).contains("two options"));
        assert!(err(None, Some("Q"), opts(&[("How to", "A"), ("b", "B")])).contains("How to"));
        assert!(err(None, Some("Q"), opts(&[("a", " "), ("b", "B")])).contains("what a means"));
        assert!(err(Some("../x.md"), Some("Q"), two.clone()).contains("run's folder"));
        assert!(err(Some("a/b.md"), Some("Q"), two.clone()).contains("run's folder"));
        assert!(err(Some("classify"), None, two).contains("classify"));
    }

    #[test]
    fn the_input_is_read_from_the_runs_folder_and_cut() {
        let home = tempfile::tempdir().unwrap();
        let s = email_stage();
        let missing = input_text(home.path(), "CS-SYNC-1", "CS-SYNC", &s).unwrap_err();
        assert!(missing.contains("email.md"), "{missing}");
        let dir = run_folder(home.path(), "CS-SYNC-1");
        assert!(dir.ends_with("hydraui/runs/CS-SYNC-1"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("email.md"), "  How do I share?  \n").unwrap();
        assert_eq!(
            input_text(home.path(), "CS-SYNC-1", "CS-SYNC", &s).unwrap(),
            "How do I share?"
        );
        std::fs::write(dir.join("email.md"), "é".repeat(MAX_INPUT + 10)).unwrap();
        assert_eq!(
            input_text(home.path(), "CS-SYNC-1", "CS-SYNC", &s)
                .unwrap()
                .chars()
                .count(),
            MAX_INPUT
        );
        std::fs::write(dir.join("email.md"), " \n").unwrap();
        assert!(
            input_text(home.path(), "CS-SYNC-1", "CS-SYNC", &s)
                .unwrap_err()
                .contains("empty")
        );
        // A mail run's thread is kept by its ticket, before the run has an id.
        let mailed = crate::mail_watch::thread_folder(home.path(), "MAIL-18C2");
        std::fs::create_dir_all(&mailed).unwrap();
        std::fs::write(mailed.join("email.md"), "# Sharing").unwrap();
        assert_eq!(
            input_text(home.path(), "MAIL-18C2-1", "MAIL-18C2", &s).unwrap(),
            "# Sharing"
        );
    }

    fn verdict(choice: &str, confidence: f64) -> Verdict {
        Verdict {
            choice: choice.into(),
            confidence,
            probabilities: [("how_to", 0.86), ("user_error", 0.1), ("none", 0.04)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn a_verdict_is_reported_with_its_score_and_every_probability() {
        let r = report(&verdict("how_to", 0.856), "jev-2026-10");
        assert_eq!(r["outcome"], "done");
        assert_eq!(r["score"], 86);
        assert_eq!(r["summary"], "Jev: how_to (0.86)");
        let evidence: Vec<&str> = r["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["detail"].as_str().unwrap())
            .collect();
        // Most likely first, and which model said so.
        assert_eq!(
            evidence,
            [
                "how_to 0.86",
                "user_error 0.10",
                "none 0.04",
                "model jev-2026-10"
            ]
        );
        assert_eq!(r["reasons"][0]["side"], "for");
        assert!(
            r["reasons"][1]["text"]
                .as_str()
                .unwrap()
                .contains("user_error")
        );
        assert_eq!(r["handoff"], "category: how_to\nconfidence: 0.86");
    }

    #[test]
    fn none_and_failures_are_blocked_for_a_person() {
        let r = report(&verdict("none", 0.9), "m");
        assert_eq!(r["outcome"], "blocked");
        assert!(r["summary"].as_str().unwrap().contains("couldn't tell"));
        let f = failed("HTTP 401");
        assert_eq!(
            (f["outcome"].as_str(), f["score"].as_i64()),
            (Some("blocked"), Some(0))
        );
        assert!(f["summary"].as_str().unwrap().contains("HTTP 401"));
        assert!(!f["evidence"].as_array().unwrap().is_empty());
        assert!(!f["reasons"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_run_passes_its_jev_stage_or_asks_a_person() {
        use crate::mcp::server::McpEvent;
        use crate::runs::{RunState, StageStatus};
        let home = tempfile::tempdir().unwrap();
        let dir = crate::pipeline::folder(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mail.toml"),
            "name = \"Mail\"\nruns_at_once = 2\n[[stage]]\nid = \"classify\"\ntitle = \"Classify\"\n\
             kind = \"jev\"\nbar = 70\ninstructions = \"Q\"\n[stage.options]\n\
             how_to = \"H\"\nuser_error = \"U\"\n\
             [[stage]]\nid = \"draft\"\ntitle = \"Draft\"\n",
        )
        .unwrap();
        let mut reg = crate::mcp::registry::Registry::default();
        reg.hydra_root = Some(home.path().to_path_buf());
        let orch = reg.register("orchestrator", home.path().to_path_buf());
        let mut start = |ticket: &str| {
            let call = |reg: &mut crate::mcp::registry::Registry, name: &str, args: Value| {
                crate::mcp::run_tools::call(reg, &orch, name, &args).unwrap()
            };
            let (out, _) = call(
                &mut reg,
                "run_start",
                json!({"pipeline": "mail", "ticket": ticket, "summary": "S"}),
            );
            assert!(!out.is_error, "{}", out.text);
            let run = format!("{ticket}-1");
            let (out, event) = call(
                &mut reg,
                "stage_start",
                json!({"run": run, "stage": "classify"}),
            );
            assert!(!out.is_error && out.text.contains("Jev's"), "{}", out.text);
            let Some(McpEvent::StageStart(s)) = event else {
                panic!("a stage start");
            };
            s
        };
        let sure = start("MAIL-1");
        let unsure = start("MAIL-2");
        let (j, bar, ticket) = of(&reg, &sure).unwrap();
        assert_eq!(
            (j.input.as_str(), bar, ticket.as_str()),
            ("email.md", 70, "MAIL-1")
        );

        let event = file(&mut reg, &sure, &report(&verdict("how_to", 0.86), "m")).unwrap();
        assert!(matches!(event, Some(McpEvent::RunChanged(_))));
        let r = reg.runs.run("MAIL-1-1").unwrap();
        assert_eq!(r.stages[0].status, StageStatus::Passed);
        assert_eq!(r.state, RunState::Running);

        file(&mut reg, &unsure, &report(&verdict("how_to", 0.48), "m")).unwrap();
        let r = reg.runs.run("MAIL-2-1").unwrap();
        assert_eq!(r.stages[0].status, StageStatus::Stopped);
        assert!(
            matches!(&r.state, RunState::NeedsYou(why) if why.contains("48")),
            "{:?}",
            r.state
        );
        // A stage reports once.
        assert!(file(&mut reg, &unsure, &report(&verdict("how_to", 0.9), "m")).is_err());
    }

    #[test]
    fn a_route_ends_the_run_for_its_categories() {
        let s = email_stage()
            .with_route(&opts(&[("user_error", "end"), ("how_to", "next")]), "c")
            .unwrap();
        assert_eq!(s.ends, ["user_error".to_string()].into());
        assert_eq!(
            ended_by(&s, "category: user_error\nconfidence: 0.91").as_deref(),
            Some("Not for this pipeline: Jev sorted it as user_error")
        );
        assert_eq!(ended_by(&s, "category: how_to\nconfidence: 0.91"), None);
        assert_eq!(ended_by(&s, "nothing handed over"), None);
        assert!(
            email_stage()
                .with_route(&opts(&[("how_to", "finish")]), "c")
                .unwrap()
                .ends
                .contains("how_to")
        );
        for bad in [("bug", "end"), ("none", "end"), ("how_to", "elsewhere")] {
            assert!(
                email_stage().with_route(&opts(&[bad]), "c").is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_run_ends_quietly_at_a_category_it_does_not_take() {
        use crate::runs::{Choice, RunState, StageStatus};
        let home = tempfile::tempdir().unwrap();
        let dir = crate::pipeline::folder(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mail.toml"),
            "name = \"Mail\"\nruns_at_once = 3\n[[stage]]\nid = \"classify\"\n\
             title = \"Classify\"\nkind = \"jev\"\nbar = 70\ninstructions = \"Q\"\n\
             [stage.options]\nhow_to = \"H\"\nuser_error = \"U\"\n\
             [stage.route]\nuser_error = \"end\"\n\
             [[stage]]\nid = \"draft\"\ntitle = \"Draft\"\n",
        )
        .unwrap();
        let mut reg = crate::mcp::registry::Registry::default();
        reg.hydra_root = Some(home.path().to_path_buf());
        let orch = reg.register("orchestrator", home.path().to_path_buf());
        let mut start = |ticket: &str| {
            let call = |reg: &mut crate::mcp::registry::Registry, name: &str, args: Value| {
                crate::mcp::run_tools::call(reg, &orch, name, &args).unwrap()
            };
            call(
                &mut reg,
                "run_start",
                json!({"pipeline": "mail", "ticket": ticket, "summary": "S"}),
            );
            let (_, event) = call(
                &mut reg,
                "stage_start",
                json!({"run": format!("{ticket}-1"), "stage": "classify"}),
            );
            let Some(crate::mcp::server::McpEvent::StageStart(s)) = event else {
                panic!("a stage start");
            };
            s
        };
        let (skip, take, unsure) = (start("MAIL-1"), start("MAIL-2"), start("MAIL-3"));

        file(&mut reg, &skip, &report(&verdict("user_error", 0.91), "m")).unwrap();
        let r = reg.runs.run("MAIL-1-1").unwrap();
        assert_eq!(r.stages[0].status, StageStatus::Passed);
        assert_eq!(r.stages[1].status, StageStatus::Waiting);
        assert_eq!(
            r.state,
            RunState::Stopped("Not for this pipeline: Jev sorted it as user_error".into())
        );

        file(&mut reg, &take, &report(&verdict("how_to", 0.91), "m")).unwrap();
        assert_eq!(reg.runs.run("MAIL-2-1").unwrap().state, RunState::Running);

        // Under its bar a person confirms Jev's pick, and the route still applies.
        file(&mut reg, &unsure, &report(&verdict("user_error", 0.5), "m")).unwrap();
        assert!(matches!(
            reg.runs.run("MAIL-3-1").unwrap().state,
            RunState::NeedsYou(_)
        ));
        reg.runs
            .person("MAIL-3-1", Choice::Checked { note: "yes".into() })
            .unwrap();
        assert!(matches!(
            &reg.runs.run("MAIL-3-1").unwrap().state,
            RunState::Stopped(why) if why.starts_with("Not for this pipeline")
        ));
    }

    #[test]
    fn reports_are_ones_stage_report_takes() {
        for args in [
            report(&verdict("how_to", 0.9), "m"),
            failed("not reachable"),
        ] {
            let rating = crate::rating::from_args(&args);
            assert!(rating.is_ok(), "{rating:?}");
        }
    }
}
