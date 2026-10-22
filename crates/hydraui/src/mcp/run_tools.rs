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
pub const NAMES: [&str; 5] = [
    "run_start",
    "stage_start",
    "run_status",
    "stage_brief",
    "stage_report",
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
                stage's work yourself. One ticket at a time: refused while your last run is still \
                going or waiting for a person.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pipeline": string("The pipeline's file name in <HYDRA_HOME>/hydraui/pipelines, \
                        without .toml (ticket is the example hydraui writes)."),
                    "ticket": string("The ticket, e.g. INTH-123."),
                    "summary": string("The ticket's title, in one line."),
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
                    "run": string("The run's id; your current run when left out."),
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
                for and against. Report blocked rather than guess when the stage can't be done \
                (not enough information, no access, a service down).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "outcome": {"type": "string", "enum": ["done", "blocked"]},
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
                },
                "required": ["outcome", "score", "summary", "evidence", "reasons"],
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
        ),
        "stage_start" => stage_start(reg, &root, token, &arg("run"), &arg("stage"), &arg("brief")),
        "run_status" => (run_status(reg, token, &arg("run")), None),
        "stage_brief" => (stage_brief(reg, token), None),
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
    log(
        root,
        &id,
        "run started",
        json!({"ticket": ticket, "summary": summary, "pipeline": pipeline_name}),
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
    let run = match run {
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
                "Stage {stage} of run {} is starting as {}: its session opens beside the user. \
                 You'll be told when it reports, or check run_status.",
                s.run, s.label
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
    let found = match run {
        "" => reg.runs.current(token),
        id => reg.runs.run(id),
    };
    match found {
        None => Outcome::err("no run: start one with run_start"),
        Some(r) if r.orchestrator != token => {
            Outcome::err("only the run's orchestrator reads its status")
        }
        Some(r) => {
            let live = |t: &str| reg.session(t).map(super::SessionState::word);
            Outcome::ok(pretty(&reg.runs.status_json(r, reg.min_week_left, live)))
        }
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
    let rating = match rating::from_args(args) {
        Ok(rating) => rating,
        Err(e) => {
            return (
                Outcome::err(format!("Not recorded: {e}. Fix these and report again.")),
                None,
            );
        }
    };
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
