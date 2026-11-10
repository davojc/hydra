//! History (core spec §50): one timeline of what every pipeline did, newest first, read back from
//! the run logs (§8), for catching up after time away. Ticket history (§7.5) has one line per
//! run; this has one line per thing that happened.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::pipeline_view::sentence;

/// How a line is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// A stage passed, a ticket finished.
    Good,
    /// A stage failed, a run stopped.
    Bad,
    /// A person is needed: a run needs you, a question went to product.
    Attention,
    Neutral,
}

/// One thing that happened in one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The log time, `2026-10-22 09:00:00.000` (local; sorts as text).
    pub at: String,
    pub run: String,
    /// The pipeline's name (`INTH SLA tickets`), else its file name.
    pub pipeline: String,
    pub ticket: String,
    /// The ticket's title, as the orchestrator gave it.
    pub title: String,
    /// The ticket or pull request.
    pub link: Option<String>,
    pub text: String,
    pub tone: Tone,
    /// What a person needs to read beside it: for a stage, its report's summary and its
    /// reasons against (such as a change left for a human to approve).
    pub details: Vec<String>,
}

/// The first line of `s`, cut to `max` characters with `…`.
fn short(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max).collect();
        format!("{}…", cut.trim_end())
    } else {
        line.to_string()
    }
}

/// Every notable thing in run `run`'s log `text`, oldest first. Session bookkeeping (folders,
/// Claude session ids, restarts, Linear syncing) is left out.
pub fn items(run: &str, text: &str) -> Vec<Item> {
    let (mut pipeline, mut ticket, mut title, mut link) =
        (String::new(), String::new(), String::new(), None::<String>);
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut scores: HashMap<String, u64> = HashMap::new();
    let mut out: Vec<(String, String, Tone, Vec<String>)> = Vec::new();
    // Each stage's latest report: its summary and its reasons against.
    let mut reports: HashMap<String, (String, Vec<String>)> = HashMap::new();
    let mut last_reported = String::new();
    // Who the last question went to: product, or a pull request's author.
    let mut asked_author = false;
    // A parked line just went: the orchestrator closing that follows is part of it.
    let mut parked = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let at = s("at");
        // A stage by its title, else its id as a sentence (`verify-2` is a later attempt's id).
        let stage = |id: &str| {
            titles
                .get(id)
                .or_else(|| titles.get(id.rsplit_once('-').map_or(id, |(base, _)| base)))
                .cloned()
                .unwrap_or_else(|| sentence(id))
        };
        let mut details: Option<Vec<String>> = None;
        let mut push = |text: String, tone| out.push((at.clone(), text, tone, Vec::new()));
        match v.get("event").and_then(Value::as_str) {
            Some("run started") => {
                ticket = s("ticket");
                title = s("summary");
                link = Some(s("link")).filter(|l| !l.is_empty());
                pipeline = s("pipeline");
                if let Ok(p) = crate::pipeline::parse(&s("pipeline_text")) {
                    if !p.name.is_empty() {
                        pipeline = p.name.clone();
                    }
                    titles = p
                        .stages
                        .iter()
                        .map(|st| (st.id.clone(), st.title.clone()))
                        .collect();
                }
                // Its first stage's start says it began.
            }
            Some("linear ticket") => {
                if link.is_none() {
                    link = Some(s("url")).filter(|u| !u.is_empty());
                }
            }
            Some("stage started") => push(format!("{} started", stage(&s("stage"))), Tone::Neutral),
            Some("report") => {
                let rating = v.get("rating");
                if let Some(score) = rating.and_then(|r| r.get("score")).and_then(Value::as_u64) {
                    scores.insert(s("stage"), score.min(100));
                }
                let summary = rating
                    .and_then(|r| r.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let against: Vec<String> = rating
                    .and_then(|r| r.get("reasons"))
                    .and_then(Value::as_array)
                    .map(|rs| {
                        rs.iter()
                            .filter(|r| r.get("side").and_then(Value::as_str) == Some("against"))
                            .filter_map(|r| r.get("text").and_then(Value::as_str))
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                last_reported = s("stage");
                reports.insert(last_reported.clone(), (summary, against));
            }
            Some("stage passed") => {
                let id = s("stage");
                let text = match scores.get(&id) {
                    Some(n) => format!("{} passed, {n}", stage(&id)),
                    None => format!("{} passed", stage(&id)),
                };
                push(text, Tone::Good);
                details = reports.get(&id).map(|(_, against)| against.clone());
            }
            Some("stage failed") => push(
                format!(
                    "{} failed: {}",
                    stage(&s("stage")),
                    short(&s("reason"), 120)
                ),
                Tone::Bad,
            ),
            Some("needs you") => {
                let reason = s("reason");
                let (id, headline, rest) = match needs_you_parts(&reason) {
                    Some(Parts::Scored {
                        stage: id,
                        score,
                        bar,
                        rest,
                    }) => (
                        id.to_string(),
                        format!(
                            "Needs you: {} scored {score}, under its bar of {bar}",
                            stage(id)
                        ),
                        rest,
                    ),
                    Some(Parts::Blocked { stage: id, rest }) => (
                        id.to_string(),
                        format!("Needs you: {} is blocked", stage(id)),
                        rest,
                    ),
                    Some(Parts::Check { stage: id, rest }) => (
                        id.to_string(),
                        format!("Needs you: {} needs a person to check", stage(id)),
                        rest,
                    ),
                    None => (
                        last_reported.clone(),
                        format!("Needs you: {}", short(&reason, 120)),
                        "",
                    ),
                };
                push(headline, Tone::Attention);
                let mut d: Vec<String> = Vec::new();
                let report = reports.get(&id);
                let rest = rest.trim();
                if !rest.is_empty() {
                    d.push(rest.to_string());
                } else if let Some((summary, _)) = report.filter(|(s, _)| !s.is_empty()) {
                    d.push(summary.clone());
                }
                if let Some((_, against)) = report {
                    d.extend(
                        against
                            .iter()
                            .filter(|a| !rest.contains(a.as_str()))
                            .cloned(),
                    );
                }
                details = Some(d);
            }
            // A review waiting on a pull request's author needs nothing from the user.
            Some("asked") if s("who") == "author" => {
                asked_author = true;
                push(
                    format!("Waiting on the author: {}", short(&s("question"), 120)),
                    Tone::Neutral,
                )
            }
            Some("asked") => {
                asked_author = false;
                push(
                    format!("Asked product: {}", short(&s("question"), 120)),
                    Tone::Attention,
                )
            }
            Some("answered") => push(
                format!(
                    "{}: {}",
                    if asked_author {
                        "The author replied"
                    } else {
                        "Product answered"
                    },
                    short(&s("answer"), 120)
                ),
                Tone::Neutral,
            ),
            Some("person") => {
                let note = s("note");
                let choice = sentence(&s("choice"));
                push(
                    if note.trim().is_empty() {
                        format!("You chose {choice}")
                    } else {
                        format!("You chose {choice}: {}", short(&note, 120))
                    },
                    Tone::Neutral,
                )
            }
            // Parking closes the orchestrator (core spec §54): the parked line says it all.
            Some("orchestrator closed") if parked => parked = false,
            Some("orchestrator closed") => push(
                "Interrupted: its orchestrator closed. Resume it from History".into(),
                Tone::Attention,
            ),
            Some("parked") => {
                parked = true;
                let hours = v["idle_hours"].as_u64().unwrap_or_default();
                push(
                    format!(
                        "Parked: nothing changed for {}, so its sessions closed. Resume it \
                         from History",
                        crate::run_park::hours_text(u32::try_from(hours).unwrap_or_default())
                    ),
                    Tone::Attention,
                )
            }
            Some("resumed after restart") => push("Resumed".into(), Tone::Neutral),
            Some("went back") => push(
                format!("Went back to {}", stage(&s("stage"))),
                Tone::Neutral,
            ),
            Some("run ended") if s("outcome") == "done" => push("Done".into(), Tone::Good),
            Some("run ended") => push(format!("Stopped: {}", short(&s("reason"), 120)), Tone::Bad),
            _ => {}
        }
        if let (Some(d), Some(last)) = (details, out.last_mut()) {
            last.3 = d;
        }
    }
    out.into_iter()
        .map(|(at, text, tone, details)| Item {
            at,
            run: run.to_string(),
            pipeline: pipeline.clone(),
            ticket: ticket.clone(),
            title: title.clone(),
            link: link.clone(),
            text,
            tone,
            details,
        })
        .collect()
}

/// A run's "needs you" reason, as the gate writes it.
#[derive(Debug, PartialEq, Eq)]
enum Parts<'a> {
    /// `stage <id>: <score>, bar <bar>. <rest>`
    Scored {
        stage: &'a str,
        score: &'a str,
        bar: &'a str,
        rest: &'a str,
    },
    /// `stage <id> is blocked: <rest>`
    Blocked { stage: &'a str, rest: &'a str },
    /// `stage <id> needs a person to check: <rest>`
    Check { stage: &'a str, rest: &'a str },
}

fn needs_you_parts(reason: &str) -> Option<Parts<'_>> {
    let r = reason.strip_prefix("stage ")?;
    if let Some((id, rest)) = r.split_once(" is blocked: ") {
        return Some(Parts::Blocked { stage: id, rest });
    }
    if let Some((id, rest)) = r.split_once(" needs a person to check: ") {
        return Some(Parts::Check { stage: id, rest });
    }
    let (id, r) = r.split_once(": ")?;
    let (score, r) = r.split_once(", bar ")?;
    let (bar, rest) = r.split_once('.').unwrap_or((r, ""));
    (score.chars().all(|c| c.is_ascii_digit()) && bar.chars().all(|c| c.is_ascii_digit()))
        .then_some(Parts::Scored {
            stage: id,
            score,
            bar,
            rest: rest.trim_start(),
        })
}

/// Everything every pipeline did at or after `since` (a log time), newest first, from the logs
/// in `<HYDRA_HOME>/hydraui/runs`. A log last written before `since` isn't read.
pub fn read(hydra_root: &Path, since: &str, since_time: std::time::SystemTime) -> Vec<Item> {
    let dir = hydra_root.join("hydraui").join("runs");
    let mut out: Vec<Item> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= since_time)
        })
        .flat_map(|e| {
            let path = e.path();
            let run = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string);
            let text = (path.extension().is_some_and(|x| x == "jsonl"))
                .then(|| std::fs::read_to_string(&path).ok())
                .flatten();
            match (run, text) {
                (Some(run), Some(text)) => items(&run, &text),
                _ => Vec::new(),
            }
        })
        .filter(|i| i.at.as_str() >= since)
        .collect();
    // Newest first; of lines written in the same millisecond, the later one first.
    out.reverse();
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out
}

/// The log time `secs` seconds before `now` (a log time), for a window's start; `None` when
/// `now` is unreadable.
pub fn before(now: &str, secs: u64) -> Option<String> {
    let (date, time) = now.split_once(' ')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(['.', ':']).map(|p| p.parse::<i64>().ok());
    let (h, min, s) = (t.next()??, t.next()??, t.next()??);
    let total = days_from_civil(y, m, day) * 86_400 + h * 3_600 + min * 60 + s - secs as i64;
    let (days, rest) = (total.div_euclid(86_400), total.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.000",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    ))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The day heading for a log time: `Today`, `Yesterday`, else its date.
pub fn day_heading(at: &str, now: &str) -> String {
    let day = at.split(' ').next().unwrap_or(at);
    let today = now.split(' ').next().unwrap_or(now);
    if day == today {
        "Today".into()
    } else if before(now, 86_400).is_some_and(|y| y.starts_with(day)) {
        "Yesterday".into()
    } else {
        day.to_string()
    }
}

/// A log time's clock time, `09:05`.
pub fn clock(at: &str) -> &str {
    at.split(' ').nth(1).and_then(|t| t.get(..5)).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn log(lines: &[Value]) -> String {
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    fn started(at: &str) -> Value {
        json!({"at": at, "event": "run started", "ticket": "INTH-482",
            "summary": "Search can't match a full name", "pipeline": "inth",
            "link": "https://linear.app/x/issue/INTH-482",
            "pipeline_text": "name = \"INTH SLA tickets\"\n[[stage]]\nid = \"replicate\"\ntitle = \"Understand & replicate\"\n[[stage]]\nid = \"verify\"\ntitle = \"Verify\"\n"})
    }

    #[test]
    fn a_run_reads_as_what_happened_with_stage_titles_and_scores() {
        let text = log(&[
            started("2026-11-10 08:00:00.000"),
            json!({"at": "2026-11-10 08:00:01.000", "event": "claude session", "id": "x"}),
            json!({"at": "2026-11-10 08:00:02.000", "event": "stage started", "stage": "replicate"}),
            json!({"at": "2026-11-10 08:10:00.000", "event": "report", "stage": "replicate",
                "rating": {"score": 88}}),
            json!({"at": "2026-11-10 08:10:00.001", "event": "stage passed", "stage": "replicate"}),
            json!({"at": "2026-11-10 08:20:00.000", "event": "asked", "stage": "verify-2",
                "question": "Which row?\nmore"}),
            json!({"at": "2026-11-10 08:30:00.000", "event": "stage failed", "stage": "verify",
                "reason": "its session ended without reporting"}),
            json!({"at": "2026-11-10 08:31:00.000", "event": "needs you", "reason": "verify: 40, bar 80"}),
            json!({"at": "2026-11-10 08:40:00.000", "event": "person", "choice": "re-run", "note": "try again"}),
            json!({"at": "2026-11-10 09:00:00.000", "event": "run ended", "outcome": "done"}),
        ]);
        let all = items("r1", &text);
        let lines: Vec<(&str, Tone)> = all.iter().map(|i| (i.text.as_str(), i.tone)).collect();
        assert_eq!(
            lines,
            vec![
                ("Understand & replicate started", Tone::Neutral),
                ("Understand & replicate passed, 88", Tone::Good),
                ("Asked product: Which row?", Tone::Attention),
                (
                    "Verify failed: its session ended without reporting",
                    Tone::Bad
                ),
                ("Needs you: verify: 40, bar 80", Tone::Attention),
                ("You chose Re-run: try again", Tone::Neutral),
                ("Done", Tone::Good),
            ]
        );
        let first = &all[0];
        assert_eq!(
            (
                first.pipeline.as_str(),
                first.ticket.as_str(),
                first.link.as_deref()
            ),
            (
                "INTH SLA tickets",
                "INTH-482",
                Some("https://linear.app/x/issue/INTH-482")
            )
        );
        assert_eq!(first.title, "Search can't match a full name");
    }

    #[test]
    fn a_stopped_run_says_why_and_an_unknown_stage_reads_as_its_id() {
        let text = log(&[
            json!({"at": "2026-11-10 08:00:00.000", "event": "run started", "ticket": "T-1",
                "pipeline": "review"}),
            json!({"at": "2026-11-10 08:01:00.000", "event": "went back", "stage": "develop"}),
            json!({"at": "2026-11-10 08:02:00.000", "event": "run ended", "outcome": "stopped",
                "reason": "the orchestrator's terminal closed"}),
        ]);
        let all = items("r2", &text);
        assert_eq!(all[0].pipeline, "review");
        assert_eq!(all[0].text, "Went back to Develop");
        assert_eq!(
            (all[1].text.as_str(), all[1].tone),
            ("Stopped: the orchestrator's terminal closed", Tone::Bad)
        );
    }

    #[test]
    fn reading_keeps_only_what_happened_since_newest_first_across_runs() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("hydraui").join("runs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.jsonl"),
            log(&[
                started("2026-11-10 07:00:00.000"),
                json!({"at": "2026-11-10 09:30:00.000", "event": "run ended", "outcome": "done"}),
            ]),
        )
        .unwrap();
        std::fs::write(
            dir.join("b.jsonl"),
            log(&[
                started("2026-11-10 09:00:00.000"),
                json!({"at": "2026-11-10 09:00:01.000", "event": "stage started", "stage": "verify"}),
            ]),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let got = read(d.path(), "2026-11-10 08:00:00.000", std::time::UNIX_EPOCH);
        let seen: Vec<(&str, &str)> = got
            .iter()
            .map(|i| (i.run.as_str(), i.text.as_str()))
            .collect();
        assert_eq!(seen, vec![("a", "Done"), ("b", "Verify started")]);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3_600);
        assert!(read(d.path(), "2026-11-10 08:00:00.000", later).is_empty());
    }

    #[test]
    fn a_parked_run_reads_as_one_line_and_a_later_close_still_shows() {
        let text = log(&[
            started("2026-11-10 08:00:00.000"),
            json!({"at": "2026-11-10 12:00:00.000", "event": "parked", "idle_hours": 4}),
            json!({"at": "2026-11-10 12:00:00.001", "event": "orchestrator closed"}),
            json!({"at": "2026-11-10 13:00:00.000", "event": "resumed after restart"}),
            json!({"at": "2026-11-10 14:00:00.000", "event": "orchestrator closed"}),
        ]);
        let texts: Vec<String> = items("r", &text).into_iter().map(|i| i.text).collect();
        assert_eq!(
            texts,
            [
                "Parked: nothing changed for 4 hours, so its sessions closed. Resume it from \
                 History",
                "Resumed",
                "Interrupted: its orchestrator closed. Resume it from History",
            ]
        );
    }

    #[test]
    fn a_review_waiting_on_its_author_isnt_a_question_for_you() {
        let text = log(&[
            started("2026-11-10 08:00:00.000"),
            json!({"at": "2026-11-10 08:01:00.000", "event": "asked", "who": "author",
                "question": "Requested changes on #104"}),
            json!({"at": "2026-11-10 08:30:00.000", "event": "answered", "answer": "Fixed"}),
            json!({"at": "2026-11-10 08:40:00.000", "event": "asked", "who": "product",
                "question": "Which rule?"}),
        ]);
        let lines: Vec<(String, Tone)> = items("r", &text)
            .into_iter()
            .map(|i| (i.text, i.tone))
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    "Waiting on the author: Requested changes on #104".into(),
                    Tone::Neutral
                ),
                ("The author replied: Fixed".into(), Tone::Neutral),
                ("Asked product: Which rule?".into(), Tone::Attention),
            ]
        );
    }

    #[test]
    fn lines_written_in_the_same_millisecond_read_latest_first() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("hydraui").join("runs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.jsonl"),
            log(&[
                started("2026-11-10 08:00:00.000"),
                json!({"at": "2026-11-10 08:28:00.000", "event": "stage passed", "stage": "verify"}),
                json!({"at": "2026-11-10 08:28:00.000", "event": "run ended", "outcome": "done"}),
            ]),
        )
        .unwrap();
        let got = read(d.path(), "2026-11-10 08:00:00.000", std::time::UNIX_EPOCH);
        let texts: Vec<&str> = got.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["Done", "Verify passed"]);
    }

    #[test]
    fn a_window_start_is_counted_back_across_midnight_and_month_ends() {
        assert_eq!(
            before("2026-11-10 09:30:15.123", 2 * 3_600).as_deref(),
            Some("2026-11-10 07:30:15.000")
        );
        assert_eq!(
            before("2026-11-01 01:00:00.000", 2 * 3_600).as_deref(),
            Some("2026-10-31 23:00:00.000")
        );
        assert_eq!(
            before("2024-03-01 00:00:00.000", 86_400).as_deref(),
            Some("2024-02-29 00:00:00.000")
        );
        assert_eq!(before("nonsense", 1), None);
    }

    #[test]
    fn days_are_headed_today_yesterday_or_their_date() {
        let now = "2026-11-10 09:00:00.000";
        assert_eq!(day_heading("2026-11-10 01:00:00.000", now), "Today");
        assert_eq!(day_heading("2026-11-09 23:00:00.000", now), "Yesterday");
        assert_eq!(day_heading("2026-11-07 23:00:00.000", now), "2026-11-07");
        assert_eq!(clock("2026-11-10 09:05:33.000"), "09:05");
    }

    #[test]
    fn long_text_is_cut_to_its_first_line() {
        assert_eq!(short("abc def\nmore", 100), "abc def");
        assert_eq!(short("abcdefgh", 5), "abcde…");
    }

    #[test]
    fn needs_you_says_why_with_the_reports_concerns_in_full() {
        let text = log(&[
            started("2026-11-10 08:00:00.000"),
            json!({"at": "2026-11-10 08:01:00.000", "event": "stage started", "stage": "verify"}),
            json!({"at": "2026-11-10 08:20:00.000", "event": "report", "stage": "verify",
            "rating": {"score": 92, "summary": "Reviewed #1908, no code issues.",
                "reasons": [
                    {"side": "for", "text": "contract consistent"},
                    {"side": "against", "text": "This is a RED-zone auth change, left for a human to approve."}
                ]}}),
            json!({"at": "2026-11-10 08:20:00.001", "event": "needs you",
                "reason": "stage verify: 92, bar 95. Reviewed #1908, no code issues."}),
        ]);
        let all = items("r", &text);
        let last = all.last().unwrap();
        assert_eq!(
            last.text,
            "Needs you: Verify scored 92, under its bar of 95"
        );
        assert_eq!(
            last.details,
            vec![
                "Reviewed #1908, no code issues.",
                "This is a RED-zone auth change, left for a human to approve.",
            ]
        );
    }

    #[test]
    fn a_blocked_stage_and_a_passed_one_carry_their_concerns() {
        let text = log(&[
            started("2026-11-10 08:00:00.000"),
            json!({"at": "2026-11-10 08:10:00.000", "event": "report", "stage": "replicate",
                "rating": {"score": 95, "summary": "ok",
                    "reasons": [{"side": "against", "text": "Not run against live Dapr."}]}}),
            json!({"at": "2026-11-10 08:10:00.001", "event": "stage passed", "stage": "replicate"}),
            json!({"at": "2026-11-10 08:30:00.000", "event": "needs you",
                "reason": "stage verify is blocked: posting was refused"}),
        ]);
        let all = items("r", &text);
        assert_eq!(all[0].details, vec!["Not run against live Dapr."]);
        assert_eq!(all[1].text, "Needs you: Verify is blocked");
        assert_eq!(all[1].details, vec!["posting was refused"]);
    }

    #[test]
    fn a_reason_in_another_shape_still_reads() {
        assert_eq!(needs_you_parts("usage is low"), None);
        assert_eq!(
            needs_you_parts("stage first needs a person to check: review the auth keys"),
            Some(Parts::Check {
                stage: "first",
                rest: "review the auth keys"
            })
        );
        assert_eq!(needs_you_parts("stage a: high, bar 9. x"), None);
        assert_eq!(
            needs_you_parts("stage a: 40, bar 80"),
            Some(Parts::Scored {
                stage: "a",
                score: "40",
                bar: "80",
                rest: ""
            })
        );
    }
}
