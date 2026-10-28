//! Runs that were open when hydraui closed (orchestrator spec §13): read back from their logs
//! (§8) so a card can offer to pick each one up where it was.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::pipeline::Pipeline;
use crate::runs::{Choice, Run, Runs};

/// The orchestrator token a replayed run carries until Resume moves it to a live terminal.
pub const REPLAYED: &str = "replayed-orchestrator";

/// A run hydraui closed on, rebuilt from its log.
#[derive(Debug, Clone)]
pub struct Interrupted {
    /// As it stood, owned by `REPLAYED`; each attempt's session is its label.
    pub run: Run,
    /// The orchestrator's environment and folder, and its Claude session.
    pub env: String,
    pub folder: PathBuf,
    pub claude: String,
    /// Per attempt label: the folder its terminal opened in and its Claude session, as logged.
    pub stages: HashMap<String, (Option<PathBuf>, Option<String>)>,
    /// Its Linear ticket as logged when it started (phase 2 §5): the card's link and SLA.
    pub ticket: Option<crate::linear::Ticket>,
}

/// Every run under `<hydra_root>/hydraui/runs` that never ended, whose hydraui is no longer
/// `alive`, and whose orchestrator's Claude session was logged, oldest first.
pub fn interrupted(hydra_root: &Path, alive: impl Fn(u32) -> bool) -> Vec<Interrupted> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(hydra_root.join("hydraui").join("runs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|f| read(hydra_root, f, &alive))
        .collect()
}

/// The folder attempt `label` of run `run` opened in and its Claude session, as its log has
/// them; what reopening that attempt's conversation needs.
pub fn logged_session(
    hydra_root: &Path,
    run: &str,
    label: &str,
) -> (Option<PathBuf>, Option<String>) {
    let Ok(log) = std::fs::read_to_string(crate::run_log::file_for(hydra_root, run)) else {
        return (None, None);
    };
    let (mut folder, mut claude) = (None, None);
    for e in log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        match event(&e) {
            "stage folder" if text(&e, "label") == label => {
                folder = Some(PathBuf::from(text(&e, "folder")));
            }
            "claude session" if text(&e, "who") == label => {
                claude = Some(text(&e, "id").to_string()).filter(|c| !c.is_empty());
            }
            _ => {}
        }
    }
    (folder, claude)
}

fn event(e: &Value) -> &str {
    e.get("event").and_then(Value::as_str).unwrap_or_default()
}

fn text<'a>(e: &'a Value, key: &str) -> &'a str {
    e.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The run logged in `file`, when it is one to offer back.
fn read(hydra_root: &Path, file: &Path, alive: &impl Fn(u32) -> bool) -> Option<Interrupted> {
    let id = file.file_stem()?.to_str()?;
    let events: Vec<Value> = std::fs::read_to_string(file)
        .ok()?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if events.iter().any(|e| event(e) == "run ended") {
        return None;
    }
    // The latest: a resumed run logs its new orchestrator again.
    let orch = events.iter().rev().find(|e| event(e) == "orchestrator")?;
    let pid = u32::try_from(orch.get("pid")?.as_u64()?).ok()?;
    if alive(pid) {
        return None;
    }
    let claude = events
        .iter()
        .rev()
        .find(|e| event(e) == "claude session" && text(e, "who") == "orchestrator")
        .map(|e| text(e, "id"))
        .filter(|c| !c.is_empty())?;
    let started = events.iter().find(|e| event(e) == "run started")?;
    let pipeline = crate::pipeline::load(hydra_root, text(started, "pipeline")).ok()?;
    let run = replay(id, pipeline, &events)?;
    let mut stages: HashMap<String, (Option<PathBuf>, Option<String>)> = HashMap::new();
    for e in &events {
        match event(e) {
            "stage folder" => {
                stages.entry(text(e, "label").to_string()).or_default().0 =
                    Some(PathBuf::from(text(e, "folder")));
            }
            "claude session" if text(e, "who") != "orchestrator" => {
                stages.entry(text(e, "who").to_string()).or_default().1 =
                    Some(text(e, "id").to_string());
            }
            _ => {}
        }
    }
    let ticket = events
        .iter()
        .rev()
        .find(|e| event(e) == "linear ticket")
        .map(|e| crate::linear::Ticket {
            id: String::new(),
            identifier: run.ticket.clone(),
            title: run.summary.clone(),
            url: text(e, "url").to_string(),
            sla_breaches_at: e["sla_breaches_at"].as_str().map(str::to_string),
            sla_high_risk_at: e["sla_high_risk_at"].as_str().map(str::to_string),
            assignee: None,
            state: String::new(),
            priority: 0,
            created_at: String::new(),
            labels: Vec::new(),
        });
    Some(Interrupted {
        ticket,
        run,
        env: text(orch, "env").to_string(),
        folder: PathBuf::from(text(orch, "folder")),
        claude: claude.to_string(),
        stages,
    })
}

/// Where stage `stage` (its id) is in run `id`'s pipeline.
fn stage_index(runs: &Runs, id: &str, stage: &str) -> Option<usize> {
    runs.run(id)?
        .pipeline
        .stages
        .iter()
        .position(|s| s.id == stage)
}

/// Run `id` as `events` made it, through the calls that made it live, with `pipeline` as its
/// file is now; `None` when the log and the rules disagree.
fn replay(id: &str, pipeline: Pipeline, events: &[Value]) -> Option<Run> {
    let mut runs = Runs::default();
    // The session of `stage`'s latest attempt: its label, as `link` set it below.
    let latest = |runs: &Runs, stage: &str| -> Option<String> {
        let r = runs.run(id)?;
        let i = r.pipeline.stages.iter().position(|s| s.id == stage)?;
        r.stages[i].latest()?.session.clone()
    };
    for e in events {
        match event(e) {
            "run started" => {
                let ticket = text(e, "ticket");
                // Ids are `<ticket>-<n>`; anything else would never be found.
                id.strip_prefix(ticket)?
                    .strip_prefix('-')?
                    .parse::<u32>()
                    .ok()?;
                runs.start_run_avoiding(
                    REPLAYED,
                    pipeline.clone(),
                    ticket,
                    text(e, "summary"),
                    |c| c != id,
                )
                .ok()?;
                runs.set_link(id, text(e, "link"));
            }
            "stage started" => {
                let s = runs.begin(REPLAYED, id, text(e, "stage"), "", 0).ok()?;
                runs.link(id, s.stage, text(e, "label"), &s.name);
            }
            // A person sent the run back here and its session reopened (Go back).
            "stage reopened" => {
                let r = runs.run(id)?;
                let i = r
                    .pipeline
                    .stages
                    .iter()
                    .position(|s| s.id == text(e, "stage"))?;
                runs.link(id, i, text(e, "label"), text(e, "label"));
            }
            "skill used" => {
                if let Some(token) = latest(&runs, text(e, "stage")) {
                    runs.skill_used(&token, text(e, "skill"));
                }
            }
            "report" => {
                let rating = crate::rating::from_args(e.get("rating")?).ok()?;
                runs.report(text(e, "label"), rating).ok()?;
            }
            "stage failed" => {
                if let Some(token) = latest(&runs, text(e, "stage")) {
                    runs.session_gone(&token);
                }
            }
            "person" => {
                let choice = match text(e, "choice") {
                    "re-run" => Choice::Retry {
                        note: text(e, "note").to_string(),
                    },
                    "stop" => Choice::Stop,
                    // A stage asked for a check by hand and the person said it works.
                    "checked" => Choice::Checked {
                        note: text(e, "note").to_string(),
                    },
                    "back" => Choice::Back {
                        stage: text(e, "stage").to_string(),
                        note: text(e, "note").to_string(),
                    },
                    _ => continue,
                };
                runs.person(id, choice).ok()?;
            }
            "paused" => {
                runs.pause(id, true).ok()?;
            }
            // A person marked a reason against Must address, and sent the stage back for it.
            "marked" => {
                let i = stage_index(&runs, id, text(e, "stage"))?;
                let reason = e.get("reason").and_then(Value::as_u64)? as usize;
                let on = e.get("on").and_then(Value::as_bool).unwrap_or(true);
                runs.mark(id, i, reason, on).ok()?;
            }
            // A person marked a reason against Not needed.
            "not needed" => {
                let i = stage_index(&runs, id, text(e, "stage"))?;
                let reason = e.get("reason").and_then(Value::as_u64)? as usize;
                let on = e.get("on").and_then(Value::as_bool).unwrap_or(true);
                runs.mark_as(id, i, reason, crate::runs::Mark::NotNeeded, on)
                    .ok()?;
            }
            // A person worked in the stopped stage's session and asked it to score again.
            "scoring again" => {
                let i = stage_index(&runs, id, text(e, "stage"))?;
                runs.rescore(id, i).ok()?;
            }
            "sent back" => {
                let i = stage_index(&runs, id, text(e, "stage"))?;
                runs.send_back(id, i).ok()?;
            }
            "send-back cancelled" => {
                runs.cancel_fix(id).ok()?;
            }
            // A stage asked a person (product) and the ticket waited.
            "asked" => {
                let token = latest(&runs, text(e, "stage"))?;
                let link = e.get("link").and_then(Value::as_str);
                // When it was asked, as logged, so the wait doesn't restart with hydraui.
                let now = std::time::SystemTime::now();
                let asked = crate::run_history::ago(text(e, "at"), &crate::action_log::now())
                    .and_then(|d| now.checked_sub(d))
                    .unwrap_or(now);
                // Logs from before `who` are product questions.
                let whom = crate::runs::Whom::parse(text(e, "who")).unwrap_or_default();
                runs.ask(&token, text(e, "question"), link, asked, whom)
                    .ok()?;
            }
            "answered" => {
                runs.answered(id, text(e, "answer")).ok()?;
            }
            "resumed" => {
                runs.pause(id, false).ok()?;
            }
            _ => {}
        }
    }
    runs.run(id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::{RunState, StageStatus};

    const RATING: &str = r#"{"outcome":"done","score":88,"summary":"Reproduced.","evidence":[{"kind":"note","detail":"d"}],"reasons":[{"side":"for","text":"r"}],"handoff":"h"}"#;

    fn home_with(log: &[String]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = crate::pipeline::folder(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("two.toml"),
            "name = \"Two\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nskill = \"qa\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"B\"\n",
        )
        .unwrap();
        let file = crate::run_log::file_for(home.path(), "INTH-7-2");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, log.join("\n") + "\n").unwrap();
        home
    }

    fn line(event: &str, rest: &str) -> String {
        format!(r#"{{"at":"2026-10-22 10:00:00.000","event":"{event}"{rest}}}"#)
    }

    /// Stage a passed, stage b running in session s-02, orchestrator in hydraui 4242.
    fn mid_run() -> Vec<String> {
        vec![
            line(
                "run started",
                r#","ticket":"INTH-7","summary":"Export","pipeline":"two""#,
            ),
            line(
                "orchestrator",
                r#","env":"iov42","folder":"/w/orch","pid":4242"#,
            ),
            line("claude session", r#","who":"orchestrator","id":"orch-c""#),
            line("stage started", r#","stage":"a","label":"s-01""#),
            line("stage folder", r#","label":"s-01","folder":"/w/qa""#),
            line("claude session", r#","who":"s-01","id":"a-c""#),
            line("skill used", r#","stage":"a","skill":"qa""#),
            line(
                "report",
                &format!(r#","stage":"a","label":"s-01","rating":{RATING}"#),
            ),
            line("stage passed", r#","stage":"a""#),
            line("stage started", r#","stage":"b","label":"s-02""#),
            line("stage folder", r#","label":"s-02","folder":"/w/dev""#),
            line("claude session", r#","who":"s-02","id":"b-c""#),
        ]
    }

    #[test]
    fn a_question_waiting_for_its_answer_comes_back_and_an_answered_one_does_not() {
        let mut log = mid_run();
        log.push(line(
            "asked",
            r#","stage":"b","question":"Remove auto-translate?","link":"https://s/t/1""#,
        ));
        let home = home_with(&log);
        let i = &interrupted(home.path(), |_| false)[0];
        let ask = i.run.asking.as_ref().expect("still asking");
        assert_eq!(
            (ask.stage, ask.question.as_str()),
            (1, "Remove auto-translate?")
        );
        assert_eq!(ask.link.as_deref(), Some("https://s/t/1"));
        assert!(matches!(i.run.state, RunState::NeedsYou(_)));

        log.push(line("answered", r#","stage":"b","answer":"Keep it""#));
        let home = home_with(&log);
        let i = &interrupted(home.path(), |_| false)[0];
        assert!(i.run.asking.is_none());
        assert_eq!(i.run.state, RunState::Running);
    }

    #[test]
    fn a_run_left_open_is_rebuilt_where_it_was() {
        let home = home_with(&mid_run());
        let found = interrupted(home.path(), |_| false);
        assert_eq!(found.len(), 1);
        let i = &found[0];
        assert_eq!(i.run.id, "INTH-7-2");
        assert_eq!(i.run.ticket, "INTH-7");
        assert_eq!(i.run.orchestrator, REPLAYED);
        assert_eq!(i.run.state, RunState::Running);
        assert_eq!(i.run.stages[0].status, StageStatus::Passed);
        assert_eq!(i.run.stages[0].rating().map(|r| r.score), Some(88));
        assert_eq!(i.run.stages[1].status, StageStatus::Running);
        assert_eq!(i.run.stages[1].latest().unwrap().label, "s-02");
        assert_eq!((i.env.as_str(), i.claude.as_str()), ("iov42", "orch-c"));
        assert_eq!(i.folder, PathBuf::from("/w/orch"));
        assert_eq!(
            i.stages.get("s-02"),
            Some(&(Some(PathBuf::from("/w/dev")), Some("b-c".to_string())))
        );
    }

    #[test]
    fn ended_live_or_unresumable_runs_stay_closed() {
        let mut ended = mid_run();
        ended.push(line("run ended", r#","outcome":"stopped","reason":"x""#));
        assert!(interrupted(home_with(&ended).path(), |_| false).is_empty());
        // Its hydraui is still running it.
        assert!(interrupted(home_with(&mid_run()).path(), |pid| pid == 4242).is_empty());
        // Logged before hydraui kept the orchestrator's session.
        let old: Vec<String> = mid_run()
            .into_iter()
            .filter(|l| !l.contains("orch-c"))
            .collect();
        assert!(interrupted(home_with(&old).path(), |_| false).is_empty());
    }

    #[test]
    fn a_re_run_and_a_pause_replay_too() {
        let mut log = mid_run()[..7].to_vec();
        let low = RATING.replace("88", "40");
        log.push(line(
            "report",
            &format!(r#","stage":"a","label":"s-01","rating":{low}"#),
        ));
        log.push(line(
            "person",
            r#","choice":"re-run","note":"look at row 500""#,
        ));
        log.push(line("stage started", r#","stage":"a","label":"s-02""#));
        log.push(line("paused", ""));
        let found = interrupted(home_with(&log).path(), |_| false);
        let r = &found[0].run;
        assert_eq!(r.stages[0].attempts.len(), 2);
        assert_eq!(r.stages[0].status, StageStatus::Running);
        assert!(r.paused);
    }

    #[test]
    fn a_linear_runs_ticket_comes_back_from_its_log() {
        let mut log = mid_run();
        log.insert(
            3,
            line(
                "linear ticket",
                r#","url":"https://linear.app/iov42/issue/INTH-7","sla_breaches_at":"2026-11-04T14:00:00.000Z","sla_high_risk_at":null"#,
            ),
        );
        let found = interrupted(home_with(&log).path(), |_| false);
        let t = found[0].ticket.as_ref().unwrap();
        assert_eq!(t.identifier, "INTH-7");
        assert_eq!(t.url, "https://linear.app/iov42/issue/INTH-7");
        assert_eq!(
            t.sla_breaches_at.as_deref(),
            Some("2026-11-04T14:00:00.000Z")
        );
        assert!(
            interrupted(home_with(&mid_run()).path(), |_| false)[0]
                .ticket
                .is_none()
        );
    }

    #[test]
    fn an_attempts_folder_and_session_are_read_back_for_reopening() {
        let home = home_with(&mid_run());
        assert_eq!(
            logged_session(home.path(), "INTH-7-2", "s-01"),
            (Some(PathBuf::from("/w/qa")), Some("a-c".to_string()))
        );
        assert_eq!(
            logged_session(home.path(), "INTH-7-2", "s-09"),
            (None, None)
        );
        assert_eq!(logged_session(home.path(), "NOPE-1", "s-01"), (None, None));
    }

    #[test]
    fn a_send_back_waiting_for_the_running_stage_replays() {
        let against = RATING.replace(
            r#"[{"side":"for","text":"r"}]"#,
            r#"[{"side":"for","text":"r"},{"side":"against","text":"No mobile check."}]"#,
        );
        let mut log = mid_run();
        log[7] = line(
            "report",
            &format!(r#","stage":"a","label":"s-01","rating":{against}"#),
        );
        log.push(line("marked", r#","stage":"a","reason":1,"on":true"#));
        log.push(line("sent back", r#","stage":"a""#));
        let r = &interrupted(home_with(&log).path(), |_| false)[0].run;
        assert_eq!(r.stages[0].latest().unwrap().must, [1]);
        let fix = r.fix_after.as_ref().expect("still queued");
        assert_eq!(fix.stage, 0);
        assert!(fix.note.contains("No mobile check."), "{}", fix.note);
    }

    #[test]
    fn a_run_sent_back_to_an_earlier_stage_replays() {
        let mut log = mid_run();
        let low = RATING.replace("88", "30");
        log.push(line(
            "report",
            &format!(r#","stage":"b","label":"s-02","rating":{low}"#),
        ));
        log.push(line(
            "person",
            r#","choice":"back","note":"rename the branch","stage":"a""#,
        ));
        log.push(line("stage reopened", r#","stage":"a","label":"s-03""#));
        let found = interrupted(home_with(&log).path(), |_| false);
        let r = &found[0].run;
        assert_eq!(r.stages[0].status, StageStatus::Running);
        assert_eq!(r.stages[0].attempts.len(), 2);
        assert_eq!(
            r.stages[0].latest().unwrap().session.as_deref(),
            Some("s-03")
        );
        assert_eq!(r.stages[1].status, StageStatus::Waiting);
        assert_eq!(r.stages[1].tries(), 0);
    }

    #[test]
    fn a_stage_scoring_again_replays_as_running_on_the_same_try() {
        let mut log = mid_run();
        let low = RATING.replace("88", "30");
        log.push(line(
            "report",
            &format!(r#","stage":"b","label":"s-02","rating":{low}"#),
        ));
        log.push(line("scoring again", r#","stage":"b""#));
        let found = interrupted(home_with(&log).path(), |_| false);
        let r = &found[0].run;
        assert_eq!(r.state, crate::runs::RunState::Running);
        assert_eq!(r.stages[1].status, StageStatus::Running);
        assert_eq!(r.stages[1].attempts.len(), 1);
    }

    #[test]
    fn a_check_passed_by_a_person_replays_as_passed() {
        let mut log = mid_run();
        let check = RATING.replace(
            r#""outcome":"done""#,
            r#""outcome":"check","check":"Try it in Chrome.""#,
        );
        log.push(line(
            "report",
            &format!(r#","stage":"b","label":"s-02","rating":{check}"#),
        ));
        let waiting = interrupted(home_with(&log).path(), |_| false);
        assert!(matches!(
            waiting[0].run.state,
            crate::runs::RunState::NeedsYou(_)
        ));
        log.push(line(
            "person",
            r#","choice":"checked","note":"works","stage":null"#,
        ));
        log.push(line("checked", r#","stage":"b","note":"works""#));
        let found = interrupted(home_with(&log).path(), |_| false);
        let r = &found[0].run;
        assert_eq!(r.stages[1].status, StageStatus::Passed);
        assert_eq!(r.state, crate::runs::RunState::Finished);
    }
}
