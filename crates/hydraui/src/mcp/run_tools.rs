//! The orchestrator's and stage sessions' tools (orchestrator spec §9): each one checked and
//! applied under the registry's lock, so a gate check and the change it allows are one step.

use std::path::Path;

use serde_json::{Value, json};

use super::registry::Registry;
use super::server::McpEvent;
use super::tools::Outcome;
use crate::pipeline;
use crate::rating;
use crate::run_log;
use crate::runs::{Change, Decided, Refusal, Role, StageStatus};

/// The tools this module answers, in the order `tools/list` shows them.
pub const NAMES: [&str; 7] = [
    "run_start",
    "stage_start",
    "run_status",
    "stage_brief",
    "stage_report",
    "stage_ask",
    "stage_answer",
];

/// The `tools/list` entries for `NAMES`.
pub fn schemas() -> Vec<Value> {
    let string = |what: &str| json!({ "type": "string", "description": what });
    vec![
        json!({
            "name": "run_start",
            "description": "Orchestrator: start a run of one ticket through a pipeline's stages. \
                Returns the run's id, its stages with their skills and confidence bars, and the \
                first stage. You coordinate: start each stage with stage_start and never do a \
                stage's work yourself. You may hold as many tickets at once as the pipeline's \
                runs_at_once (one unless it says more): each run has its own id and its own \
                stage sessions, and hydraui's messages name the run they're about. Refused \
                when that many are still going or waiting for a person.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pipeline": string("The pipeline's file name in <HYDRA_HOME>/hydraui/pipelines, \
                        without .toml (ticket is the example hydraui writes)."),
                    "ticket": string("The ticket, e.g. INTH-123."),
                    "summary": string("The ticket's title, in one line."),
                    "link": string("What the run is about, as a link: the pull request a \
                        Slack-picked run reviews (optional; stages get it as {pr})."),
                },
                "required": ["pipeline", "ticket", "summary"],
            },
        }),
        json!({
            "name": "stage_start",
            "description": "Orchestrator: start the next stage of your run. hydraui opens a fresh \
                Claude session for it beside the user and gives it the stage's prompt and your \
                brief. Refused, with the reason, unless every stage before it passed its \
                confidence bar, no stage is running, the user hasn't paused the run and enough \
                of the seven-day Claude limit is left. Then wait: you're told when the stage \
                reports, or check run_status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run": string("The run's id, from run_start."),
                    "stage": string("The stage's id, from run_start or run_status."),
                    "brief": string("Anything the stage's session should know beyond its prompt \
                        (optional)."),
                },
                "required": ["run", "stage"],
            },
        }),
        json!({
            "name": "run_status",
            "description": "Orchestrator: your run's state (running, needs_you with the reason, \
                finished, stopped), whether the user paused it, each stage's status, bar and \
                attempts (session, live state, rating), and whether the next stage may start now \
                or why not. When the run needs a person, tell the user why and wait.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run": string("The run's id; left out, your only open run (or latest), and \
                        when you hold several open, each of them."),
                },
            },
        }),
        json!({
            "name": "stage_brief",
            "description": "Stage session: what this stage is: the run, the ticket, the stage's \
                title, skill and confidence bar, this attempt, a person's note if they re-ran it, \
                and the previous stage's summary and handoff. Do only this stage's work.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "stage_report",
            "description": "Stage session: end your stage with its confidence rating, once. The \
                next stage starts only if your score is at or above the stage's bar; below it, \
                or blocked, a person is asked to step in. Be honest: give evidence and reasons \
                for and against. Don't approve or merge a pull request (or take any step outside \
                hydraui that can't be undone) unless you report at or above the bar: the person \
                decides below it. Report blocked rather than guess when the stage can't be done \
                (not enough information, no access, a service down). Report check when the work \
                is done as far as you can prove it but the last proof needs a person to try \
                something by hand that no tool here can (a real browser feature automation \
                can't drive, a device, a printout): say exactly what to try and what they \
                should see in check. A person then passes the stage or sends it back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "outcome": {"type": "string", "enum": ["done", "blocked", "check"]},
                    "score": {
                        "type": "integer", "minimum": 0, "maximum": 100,
                        "description": "Your confidence that the stage is done well.",
                    },
                    "summary": string("One or two sentences: what the stage found or did."),
                    "evidence": {
                        "type": "array", "minItems": 1, "maxItems": rating::MAX_EVIDENCE,
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": {"type": "string",
                                         "enum": ["test", "command", "file", "link", "note"]},
                                "detail": {"type": "string", "maxLength": rating::MAX_DETAIL},
                            },
                            "required": ["kind", "detail"],
                        },
                    },
                    "reasons": {
                        "type": "array", "minItems": 1, "maxItems": rating::MAX_EVIDENCE,
                        "description": "Why this score: for (what makes it this high) and \
                            against (what keeps it from higher).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "side": {"type": "string", "enum": ["for", "against"]},
                                "text": {"type": "string", "maxLength": rating::MAX_DETAIL},
                            },
                            "required": ["side", "text"],
                        },
                    },
                    "handoff": string("What the next stage needs to know (may be empty for the \
                        last stage)."),
                    "check": string("With outcome check: the steps a person should take by \
                        hand and what they should see if it works."),
                    "screenshots": {
                        "type": "array", "maxItems": rating::MAX_SHOTS,
                        "items": {"type": "string"},
                        "description": "Absolute paths to screenshots (.png, .jpg, .gif, .webp) \
                            of what the person should look at, when you can capture them (a \
                            browser screenshot of the page, before and after). With check, \
                            capture them whenever you can: the person sees them in hydraui \
                            beside the button that passes the stage.",
                    },
                },
                "required": ["outcome", "score", "summary", "evidence", "reasons"],
            },
        }),
        json!({
            "name": "stage_ask",
            "description": "Stage session: your stage waits on someone. who \"product\" (the \
                default): a decision isn't yours to make (a change beyond the ticket that alters \
                what users see or do); ask it, once you've posted it where your stage note says. \
                who \"author\": you asked a pull request's author for changes and wait for them \
                to push or reply; give the pull request's link. Either way the ticket waits, \
                not marked as needing the user. Then stop: hydraui tells you when to look again, \
                or gives you the answer. Don't make the change meanwhile.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": string("The decision needed, in a sentence or two: what you'd \
                        change, the options and your recommendation."),
                    "link": string("The Slack message you asked in (optional), or the pull \
                        request's link for who \"author\" (required)."),
                    "who": string("\"product\" (default) or \"author\"."),
                },
                "required": ["question"],
            },
        }),
        json!({
            "name": "stage_answer",
            "description": "Stage session: the answer to your stage_ask came (you found it in \
                the Slack thread). Record it, then carry on with your stage as it says.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "answer": string("The answer, and who gave it."),
                },
                "required": ["answer"],
            },
        }),
    ]
}

/// Runs tool `name` for the terminal holding `token`; `None` for a name not in `NAMES`.
pub fn call(
    reg: &mut Registry,
    token: &str,
    name: &str,
    args: &Value,
) -> Option<(Outcome, Option<McpEvent>)> {
    if !NAMES.contains(&name) {
        return None;
    }
    let Some(root) = reg.hydra_root.clone() else {
        return Some((
            Outcome::err("hydraui has no hydra home for pipelines and run logs"),
            None,
        ));
    };
    let arg = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Some(match name {
        "run_start" => run_start(
            reg,
            &root,
            token,
            &arg("pipeline"),
            &arg("ticket"),
            &arg("summary"),
            &arg("link"),
        ),
        "stage_start" => stage_start(reg, &root, token, &arg("run"), &arg("stage"), &arg("brief")),
        "run_status" => (run_status(reg, token, &arg("run")), None),
        "stage_brief" => (stage_brief(reg, token), None),
        "stage_ask" => stage_ask(
            reg,
            &root,
            token,
            &arg("question"),
            &arg("link"),
            &arg("who"),
        ),
        "stage_answer" => stage_answer(reg, &root, token, &arg("answer")),
        _ => stage_report(reg, &root, token, args),
    })
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("json serialises")
}

/// Appends to run `run`'s log, noting a failure in hydraui's log (§8: never stops a run).
fn log(root: &Path, run: &str, event: &str, fields: Value) {
    if let Err(e) = run_log::append(&run_log::file_for(root, run), event, fields) {
        crate::action_log::write(format!("run log {run}: {e}"));
    }
}

fn run_start(
    reg: &mut Registry,
    root: &Path,
    token: &str,
    pipeline_name: &str,
    ticket: &str,
    summary: &str,
    link: &str,
) -> (Outcome, Option<McpEvent>) {
    if let Role::Stage(..) = reg.runs.role(token) {
        return (Outcome::err("a stage session can't start a run"), None);
    }
    if let Err(e) = pipeline::ensure_example(root) {
        crate::action_log::write(format!("example pipeline: {e}"));
    }
    let folder = pipeline::folder(root);
    let p = match pipeline::load(root, pipeline_name) {
        Ok(p) => p,
        Err(e) => {
            return (
                Outcome::err(format!("{e} (pipelines are in {})", folder.display())),
                None,
            );
        }
    };
    let logged = |id: &str| run_log::file_for(root, id).exists();
    let (id, reply) = match reg
        .runs
        .start_run_avoiding(token, p, ticket, summary, logged)
    {
        Err(e) => return (Outcome::err(e), None),
        Ok(r) => {
            let stages: Vec<Value> = r
                .pipeline
                .stages
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    json!({"id": s.id, "title": s.title, "skill": s.skill,
                           "folder": s.folder, "bar": r.pipeline.bar_for(i)})
                })
                .collect();
            let reply = json!({
                "run": r.id,
                "pipeline": r.pipeline.name,
                "stages": stages,
                "first": r.pipeline.stages[0].id,
            });
            (r.id.clone(), reply)
        }
    };
    reg.runs.set_link(&id, link);
    // The pipeline as it is now, so a restart resumes the run with it though it's edited
    // meanwhile (core spec §45).
    let pipeline_text =
        std::fs::read_to_string(pipeline::folder(root).join(format!("{pipeline_name}.toml"))).ok();
    log(
        root,
        &id,
        "run started",
        json!({"ticket": ticket, "summary": summary, "pipeline": pipeline_name, "link": link,
               "pipeline_text": pipeline_text}),
    );
    let started = Change {
        run: id,
        stage: None,
        what: Decided::RunStarted,
    };
    (
        Outcome::ok(pretty(&reply)),
        Some(McpEvent::RunChanged(vec![started])),
    )
}

fn stage_start(
    reg: &mut Registry,
    root: &Path,
    token: &str,
    run: &str,
    stage: &str,
    brief: &str,
) -> (Outcome, Option<McpEvent>) {
    let open: Vec<String> = reg
        .runs
        .open_of(token)
        .iter()
        .map(|r| r.id.clone())
        .collect();
    let run = match run {
        "" if open.len() > 1 => {
            return (
                Outcome::err(format!(
                    "you hold {} tickets ({}): say which run",
                    open.len(),
                    open.join(", ")
                )),
                None,
            );
        }
        "" => reg
            .runs
            .current(token)
            .map(|r| r.id.clone())
            .unwrap_or_default(),
        id => id.to_string(),
    };
    let min = reg.min_week_left;
    match reg.runs.begin(token, &run, stage, brief, min) {
        Ok(s) => {
            log(
                root,
                &s.run,
                "stage started",
                json!({"stage": stage, "label": s.label}),
            );
            let text = format!(
                "Stage {stage} of run {} is starting as {} ({}): its session opens beside the \
                 user. You'll be told when it reports, or check run_status.",
                s.run, s.name, s.label
            );
            (Outcome::ok(text), Some(McpEvent::StageStart(s)))
        }
        Err(Refusal { why, changes }) => {
            run_log::record(root, &reg.runs, &changes);
            let event = (!changes.is_empty()).then_some(McpEvent::RunChanged(changes));
            (Outcome::err(why), event)
        }
    }
}

fn run_status(reg: &Registry, token: &str, run: &str) -> Outcome {
    let live = |t: &str| reg.session(t).map(super::SessionState::word);
    let open = reg.runs.open_of(token);
    if run.is_empty() && open.len() > 1 {
        let all: Vec<Value> = open
            .iter()
            .map(|r| reg.runs.status_json(r, reg.min_week_left, live))
            .collect();
        return Outcome::ok(pretty(&json!({ "open_runs": all })));
    }
    let found = match run {
        "" => reg.runs.current(token),
        id => reg.runs.run(id),
    };
    match found {
        None => Outcome::err("no run: start one with run_start"),
        Some(r) if r.orchestrator != token => {
            Outcome::err("only the run's orchestrator reads its status")
        }
        Some(r) => Outcome::ok(pretty(&reg.runs.status_json(r, reg.min_week_left, live))),
    }
}

fn stage_brief(reg: &Registry, token: &str) -> Outcome {
    let Role::Stage(r, i) = reg.runs.role(token) else {
        return Outcome::err("only a stage's own session asks for its brief");
    };
    let spec = &r.pipeline.stages[i];
    let latest = r.stages[i].latest();
    let previous = r.stages[..i]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(j, s)| s.rating().map(|rating| (j, rating)))
        .map(|(j, rating)| {
            json!({"stage": r.pipeline.stages[j].title, "summary": rating.summary,
                   "handoff": rating.handoff})
        });
    Outcome::ok(pretty(&json!({
        "run": r.id,
        "ticket": r.ticket,
        "summary": r.summary,
        "stage": {"id": spec.id, "title": spec.title, "skill": spec.skill,
                  "folder": spec.folder, "bar": r.pipeline.bar_for(i)},
        "attempt": latest.map(|a| a.label.as_str()),
        "note": latest.and_then(|a| a.note.as_deref()),
        "previous": previous,
    })))
}

fn stage_ask(
    reg: &mut Registry,
    root: &Path,
    token: &str,
    question: &str,
    link: &str,
    who: &str,
) -> (Outcome, Option<McpEvent>) {
    let link = (!link.is_empty()).then_some(link);
    let Some(whom) = crate::runs::Whom::parse(who) else {
        return (
            Outcome::err(format!("who {who:?} must be \"product\" or \"author\"")),
            None,
        );
    };
    let changes = match reg
        .runs
        .ask(token, question, link, std::time::SystemTime::now(), whom)
    {
        Ok(changes) => changes,
        Err(e) => return (Outcome::err(e), None),
    };
    run_log::record(root, &reg.runs, &changes);
    (
        Outcome::ok(
            "Recorded: your ticket waits for the answer. Stop here and don't make the change: \
             hydraui will ask you to look for the answer, or give it to you."
                .to_string(),
        ),
        Some(McpEvent::RunChanged(changes)),
    )
}

fn stage_answer(
    reg: &mut Registry,
    root: &Path,
    token: &str,
    answer: &str,
) -> (Outcome, Option<McpEvent>) {
    let Role::Stage(r, i) = reg.runs.role(token) else {
        return (
            Outcome::err("only the stage that asked records the answer"),
            None,
        );
    };
    if r.asking.as_ref().map(|a| a.stage) != Some(i) {
        return (Outcome::err("your stage isn't waiting for an answer"), None);
    }
    let run = r.id.clone();
    let changes = match reg.runs.answered(&run, answer) {
        Ok(changes) => changes,
        Err(e) => return (Outcome::err(e), None),
    };
    run_log::record(root, &reg.runs, &changes);
    (
        Outcome::ok("Recorded. Carry on with your stage as the answer says.".to_string()),
        Some(McpEvent::RunChanged(changes)),
    )
}

/// Copies `rating`'s screenshots into `<run log>/../<run>/shots/<label>-<n>.<ext>` and points
/// it at the copies; a missing file is named.
fn keep_shots(
    root: &Path,
    run: &str,
    label: &str,
    rating: &mut rating::Rating,
) -> Result<(), String> {
    if rating.screenshots.is_empty() {
        return Ok(());
    }
    let dir = run_log::file_for(root, run)
        .with_extension("")
        .join("shots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't keep screenshots: {e}"))?;
    let mut kept = Vec::new();
    for (n, shot) in rating.screenshots.iter().enumerate() {
        let from = Path::new(shot);
        if !from.is_file() {
            return Err(format!("screenshot {shot} doesn't exist"));
        }
        let ext = from
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let to = dir.join(format!("{label}-{}.{ext}", n + 1));
        std::fs::copy(from, &to).map_err(|e| format!("can't copy screenshot {shot}: {e}"))?;
        kept.push(to.display().to_string());
    }
    rating.screenshots = kept;
    Ok(())
}

fn stage_report(
    reg: &mut Registry,
    root: &Path,
    token: &str,
    args: &Value,
) -> (Outcome, Option<McpEvent>) {
    let Role::Stage(r, i) = reg.runs.role(token) else {
        return (
            Outcome::err("only a running stage's own session reports"),
            None,
        );
    };
    let (run, file, stage) = (
        r.id.clone(),
        r.pipeline.file.clone(),
        r.pipeline.stages[i].id.clone(),
    );
    let label = r.stages[i].latest().map(|a| a.label.clone());
    let mut rating = match rating::from_args(args) {
        Ok(rating) => rating,
        Err(e) => {
            return (
                Outcome::err(format!("Not recorded: {e}. Fix these and report again.")),
                None,
            );
        }
    };
    // Screenshots are copied beside the run's log, so they outlive the session's temp files.
    if let Err(e) = keep_shots(root, &run, label.as_deref().unwrap_or("s"), &mut rating) {
        return (
            Outcome::err(format!("Not recorded: {e}. Fix it and report again.")),
            None,
        );
    }
    // Bars follow the file to the gate (§5.1); one that no longer reads keeps the run's.
    if let Ok(now) = pipeline::load(root, &file) {
        reg.runs.refresh_from_file(&file, &now);
    }
    let logged = serde_json::to_value(&rating).expect("json serialises");
    let changes = match reg.runs.report(token, rating) {
        Ok(changes) => changes,
        Err(e) => return (Outcome::err(e), None),
    };
    log(
        root,
        &run,
        "report",
        json!({"stage": stage, "label": label, "rating": logged}),
    );
    run_log::record(root, &reg.runs, &changes);
    let r = reg.runs.run(&run).expect("it just reported");
    let bar = r.pipeline.bar_for(i);
    let score = r.stages[i].rating().map_or(0, |x| x.score);
    let text = if r.stages[i].status == StageStatus::Passed {
        format!(
            "Recorded: stage {stage} passed ({score}, bar {bar}). Your stage is done; the \
             orchestrator takes it from here."
        )
    } else {
        format!(
            "Recorded: stage {stage} stopped ({score}, bar {bar}). A person decides what \
             happens next; stay available in case they step in to this session."
        )
    };
    (Outcome::ok(text), Some(McpEvent::RunChanged(changes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(shots: Vec<String>) -> rating::Rating {
        let mut r = rating::from_args(&json!({"outcome": "done", "score": 90, "summary": "s",
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "for", "text": "r"}]}))
        .unwrap();
        r.screenshots = shots;
        r
    }

    #[test]
    fn screenshots_are_kept_beside_the_run_log_and_a_missing_one_is_named() {
        let home = tempfile::tempdir().unwrap();
        let shot = home.path().join("from-session.PNG");
        std::fs::write(&shot, b"png").unwrap();
        let mut r = checked(vec![shot.display().to_string()]);
        keep_shots(home.path(), "PRE-1-1", "s-03", &mut r).unwrap();
        let kept = std::path::PathBuf::from(&r.screenshots[0]);
        assert!(kept.ends_with("runs/PRE-1-1/shots/s-03-1.png"), "{kept:?}");
        assert_eq!(std::fs::read(&kept).unwrap(), b"png");

        let mut gone = checked(vec!["/nowhere/x.png".into()]);
        let e = keep_shots(home.path(), "PRE-1-1", "s-03", &mut gone).unwrap_err();
        assert!(e.contains("/nowhere/x.png"), "{e}");
    }
}
