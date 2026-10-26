//! Ticket history (orchestrator spec §7.5; the design's View 3): one entry per run, read back
//! from the run logs (§8).

use std::path::Path;

use serde_json::Value;

use crate::pipeline_view::sentence;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Stopped,
    InProgress,
}

impl Outcome {
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Done => "Done",
            Outcome::Stopped => "Stopped",
            Outcome::InProgress => "In progress",
        }
    }
}

/// One run as Ticket history shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub run: String,
    pub ticket: String,
    /// The summary the orchestrator gave: the ticket's title.
    pub title: String,
    pub outcome: Outcome,
    /// Where and why it stopped, or the last stage's summary.
    pub detail: String,
    /// Seconds from start to end; `None` while it runs.
    pub took: Option<u64>,
    /// Each stage the log names, in order, with its latest score.
    pub stages: Vec<(String, Option<u8>)>,
    /// A person's note on it, when they gave one.
    pub note: Option<String>,
    /// When it started, as the log writes times (sortable).
    started: String,
}

/// Seconds since 1970 for a log time, `2026-10-22 09:00:00.000` (local; only differences are
/// used).
fn secs(at: &str) -> Option<i64> {
    let (date, time) = at.split_once(' ')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(['.', ':']).map(|p| p.parse::<i64>().ok());
    let (h, min, s) = (t.next()??, t.next()??, t.next()??);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + min * 60 + s)
}

/// How long a run took as the design shows it: `1h 48m`, `9m`; `–` while it runs.
pub fn took_text(took: Option<u64>) -> String {
    match took {
        None => "–".into(),
        Some(s) if s >= 3_600 => format!("{}h {:02}m", s / 3_600, s % 3_600 / 60),
        Some(s) => format!("{}m", s / 60),
    }
}

fn entry(run: &str, text: &str, live: bool) -> Option<Entry> {
    let (mut ticket, mut title, mut started) = (String::new(), String::new(), None);
    let (mut ended, mut end_outcome, mut end_reason) = (None, None, None);
    let (mut last_summary, mut needs_you, mut note) = (None, None, None);
    let mut stages: Vec<(String, Option<u8>)> = Vec::new();
    let mut end_scores: Option<Vec<(String, Option<u8>)>> = None;
    let stage_entry = |stages: &mut Vec<(String, Option<u8>)>, id: &str| -> usize {
        match stages.iter().position(|(s, _)| s == id) {
            Some(i) => i,
            None => {
                stages.push((id.to_string(), None));
                stages.len() - 1
            }
        }
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let at = s("at");
        match v.get("event").and_then(Value::as_str) {
            Some("run started") => {
                ticket = s("ticket").unwrap_or_default();
                title = s("summary").unwrap_or_default();
                started = at;
            }
            Some("stage started") => {
                if let Some(id) = s("stage") {
                    stage_entry(&mut stages, &id);
                }
            }
            Some("report") => {
                if let Some(id) = s("stage") {
                    let i = stage_entry(&mut stages, &id);
                    let rating = v.get("rating");
                    stages[i].1 = rating
                        .and_then(|r| r.get("score"))
                        .and_then(Value::as_u64)
                        .map(|n| n.min(100) as u8);
                    last_summary = rating
                        .and_then(|r| r.get("summary"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
            Some("needs you") => needs_you = s("reason"),
            Some("person") => {
                if let Some(n) = s("note").filter(|n| !n.trim().is_empty()) {
                    note = Some(n);
                }
            }
            Some("run ended") => {
                ended = at;
                end_outcome = s("outcome");
                end_reason = s("reason");
                end_scores = v.get("scores").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            let id = x.get("stage")?.as_str()?.to_string();
                            let score = x
                                .get("score")
                                .and_then(Value::as_u64)
                                .map(|n| n.min(100) as u8);
                            Some((id, score))
                        })
                        .collect()
                });
            }
            _ => {}
        }
    }
    let started = started?;
    let (outcome, detail) = match (end_outcome.as_deref(), live) {
        (Some("done"), _) => (
            Outcome::Done,
            last_summary
                .clone()
                .unwrap_or_else(|| "Every stage passed".into()),
        ),
        (Some(_), _) => (
            Outcome::Stopped,
            sentence(end_reason.as_deref().unwrap_or("stopped")),
        ),
        (None, true) => (
            Outcome::InProgress,
            needs_you.map_or("Running".into(), |why| format!("Needs you: {why}")),
        ),
        (None, false) => (
            Outcome::Stopped,
            "hydraui closed before the run ended".to_string(),
        ),
    };
    let took = ended
        .as_deref()
        .and_then(secs)
        .zip(secs(&started))
        .map(|(e, s)| (e - s).max(0) as u64);
    Some(Entry {
        run: run.to_string(),
        ticket,
        title,
        outcome,
        detail,
        took,
        stages: end_scores.unwrap_or(stages),
        note,
        started,
    })
}

/// Every run in `<HYDRA_HOME>/hydraui/runs`, newest first, the `live` ones (still in hydraui's
/// memory) pinned on top.
pub fn read(hydra_root: &Path, live: &[String]) -> Vec<Entry> {
    let dir = hydra_root.join("hydraui").join("runs");
    let mut out: Vec<Entry> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let run = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "jsonl").then_some(())?;
            let text = std::fs::read_to_string(&path).ok()?;
            entry(&run, &text, live.contains(&run))
        })
        .collect();
    out.sort_by(|a, b| {
        (b.outcome == Outcome::InProgress)
            .cmp(&(a.outcome == Outcome::InProgress))
            .then_with(|| b.started.cmp(&a.started))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes run `run`'s log from `lines`, each `(at, event, fields)`.
    fn log(root: &std::path::Path, run: &str, lines: &[(&str, &str, serde_json::Value)]) {
        let file = crate::run_log::file_for(root, run);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let text: String = lines
            .iter()
            .map(|(at, event, fields)| {
                let mut line = fields.clone();
                line["at"] = (*at).into();
                line["event"] = (*event).into();
                format!("{line}\n")
            })
            .collect();
        std::fs::write(file, text).unwrap();
    }

    fn rating(score: u8, summary: &str) -> serde_json::Value {
        serde_json::json!({"score": score, "summary": summary})
    }

    #[test]
    fn history_reads_outcomes_details_and_scores_from_the_logs() {
        use serde_json::json;
        let d = tempfile::tempdir().unwrap();
        log(
            d.path(),
            "INTH-477-1",
            &[
                (
                    "2026-10-22 09:00:00.000",
                    "run started",
                    json!({"ticket": "INTH-477", "summary": "Reset links expire", "pipeline": "ticket"}),
                ),
                (
                    "2026-10-22 09:01:00.000",
                    "stage started",
                    json!({"stage": "replicate", "label": "s-01"}),
                ),
                (
                    "2026-10-22 09:20:00.000",
                    "report",
                    json!({"stage": "replicate", "label": "s-01", "rating": rating(92, "Reproduced.")}),
                ),
                (
                    "2026-10-22 10:00:00.000",
                    "report",
                    json!({"stage": "develop", "label": "s-02", "rating": rating(88, "Fixed token TTL.")}),
                ),
                (
                    "2026-10-22 10:48:30.000",
                    "run ended",
                    json!({"outcome": "done", "scores": [{"stage": "replicate", "score": 92},
                                                  {"stage": "develop", "score": 88}]}),
                ),
            ],
        );
        log(
            d.path(),
            "INTH-471-1",
            &[
                (
                    "2026-10-22 11:00:00.000",
                    "run started",
                    json!({"ticket": "INTH-471", "summary": "Duplicate webhook", "pipeline": "ticket"}),
                ),
                (
                    "2026-10-22 11:05:00.000",
                    "report",
                    json!({"stage": "replicate", "label": "s-01", "rating": rating(41, "No webhook id.")}),
                ),
                (
                    "2026-10-22 11:05:00.000",
                    "needs you",
                    json!({"reason": "stage replicate: 41, bar 80. No webhook id."}),
                ),
                (
                    "2026-10-22 11:09:00.000",
                    "person",
                    json!({"choice": "stop", "note": null}),
                ),
                (
                    "2026-10-22 11:09:00.000",
                    "run ended",
                    json!({"outcome": "stopped", "reason": "stopped by you at Understand and replicate",
                    "scores": [{"stage": "replicate", "score": 41}, {"stage": "develop", "score": null}]}),
                ),
            ],
        );
        let h = read(d.path(), &[]);
        let runs: Vec<&str> = h.iter().map(|e| e.run.as_str()).collect();
        assert_eq!(runs, ["INTH-471-1", "INTH-477-1"], "newest first");
        let stopped = &h[0];
        assert_eq!(
            (
                stopped.ticket.as_str(),
                stopped.title.as_str(),
                stopped.outcome
            ),
            ("INTH-471", "Duplicate webhook", Outcome::Stopped)
        );
        assert_eq!(stopped.detail, "Stopped by you at Understand and replicate");
        assert_eq!(stopped.took, Some(9 * 60));
        assert_eq!(
            stopped.stages,
            [
                ("replicate".to_string(), Some(41)),
                ("develop".to_string(), None)
            ]
        );
        let done = &h[1];
        assert_eq!(
            (done.outcome, done.detail.as_str()),
            (Outcome::Done, "Fixed token TTL.")
        );
        assert_eq!(done.took, Some(108 * 60 + 30));
        assert_eq!(took_text(done.took), "1h 48m");
        assert_eq!(took_text(stopped.took), "9m");
    }

    #[test]
    fn a_log_without_an_end_is_stopped_unless_live() {
        use serde_json::json;
        let d = tempfile::tempdir().unwrap();
        log(
            d.path(),
            "INTH-482-1",
            &[
                (
                    "2026-10-22 12:00:00.000",
                    "run started",
                    json!({"ticket": "INTH-482", "summary": "Export drops rows", "pipeline": "ticket"}),
                ),
                (
                    "2026-10-22 12:01:00.000",
                    "stage started",
                    json!({"stage": "replicate", "label": "s-01"}),
                ),
            ],
        );
        log(
            d.path(),
            "INTH-480-1",
            &[(
                "2026-10-22 13:00:00.000",
                "run started",
                json!({"ticket": "INTH-480", "summary": "Old", "pipeline": "ticket"}),
            )],
        );
        let h = read(d.path(), &[]);
        assert!(h.iter().all(|e| e.outcome == Outcome::Stopped));
        assert_eq!(h[1].detail, "hydraui closed before the run ended");
        let h = read(d.path(), &["INTH-482-1".to_string()]);
        assert_eq!(
            (h[0].run.as_str(), h[0].outcome),
            ("INTH-482-1", Outcome::InProgress),
            "pinned"
        );
        assert_eq!(h[0].stages, [("replicate".to_string(), None)]);
        assert_eq!(h[0].took, None);
        // A missing folder is an empty history.
        assert!(read(&d.path().join("nowhere"), &[]).is_empty());
    }
}
