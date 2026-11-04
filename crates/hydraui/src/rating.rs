//! A stage's confidence rating (orchestrator spec §4): what `stage_report` carries, checked
//! before it counts, and whether it clears a bar.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The stage did its job.
    Done,
    /// It couldn't (not enough information, no access, a service down): stops the run whatever
    /// the score.
    Blocked,
    /// The work looks done, but proving it needs a person to try something by hand that no
    /// tool here can (real Chrome's own translate, a phone, a printed page). `check` says what.
    /// Stops the run until a person says it works or sends the stage back.
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    Test,
    Command,
    File,
    Link,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub detail: String,
}

/// Which way a reason pulls the score (the design's `+` and `−`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// What makes the score this high.
    For,
    /// What keeps it from higher.
    Against,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub side: Side,
    pub text: String,
}

/// What a stage session reported at its end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rating {
    pub outcome: Outcome,
    /// The session's confidence that the stage is done well, 0 to 100.
    pub score: u8,
    pub summary: String,
    pub evidence: Vec<Evidence>,
    /// Why this score: at least one reason for or against.
    pub reasons: Vec<Reason>,
    /// What the next stage needs to know; may be empty.
    pub handoff: String,
    /// For `Check`: what a person should try and what they should see. Empty otherwise.
    pub check: String,
    /// Pictures of what to look at (absolute paths to images the session captured), shown
    /// where the person decides; hydraui keeps its own copies under the run's folder.
    pub screenshots: Vec<String>,
}

/// Most evidence items (and reasons) in one report, and the longest detail or reason
/// (characters).
pub const MAX_EVIDENCE: usize = 20;
pub const MAX_DETAIL: usize = 500;
/// The most screenshots in one report.
pub const MAX_SHOTS: usize = 6;

/// Whether `path` names a picture hydraui shows: absolute, and PNG, JPEG, GIF or WebP.
pub fn is_picture(path: &str) -> bool {
    let p = std::path::Path::new(path);
    p.is_absolute()
        && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["png", "jpg", "jpeg", "gif", "webp"].contains(&e.to_ascii_lowercase().as_str())
        })
}

/// The longest `check` (characters): room for a few steps.
pub const MAX_CHECK: usize = 1500;

fn text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn evidence_kind(s: &str) -> Option<EvidenceKind> {
    Some(match s {
        "test" => EvidenceKind::Test,
        "command" => EvidenceKind::Command,
        "file" => EvidenceKind::File,
        "link" => EvidenceKind::Link,
        "note" => EvidenceKind::Note,
        _ => return None,
    })
}

/// Reads `reasons`, adding what's wrong with them to `problems`.
fn reasons(args: &Value, problems: &mut Vec<String>) -> Vec<Reason> {
    let items = args
        .get("reasons")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if items.is_empty() {
        problems
            .push("reasons is empty: give at least one, each {side: for or against, text}".into());
    } else if items.len() > MAX_EVIDENCE {
        problems.push(format!(
            "reasons has {} items; at most {MAX_EVIDENCE}",
            items.len()
        ));
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().take(MAX_EVIDENCE).enumerate() {
        let side_text = text(item, "side");
        let side = match side_text.as_str() {
            "for" => Some(Side::For),
            "against" => Some(Side::Against),
            _ => {
                problems.push(format!(
                    "reason {}: side {side_text:?} must be for or against",
                    i + 1
                ));
                None
            }
        };
        let text = text(item, "text");
        if text.is_empty() {
            problems.push(format!("reason {}: text is empty", i + 1));
        } else if text.chars().count() > MAX_DETAIL {
            problems.push(format!(
                "reason {}: text is over {MAX_DETAIL} characters",
                i + 1
            ));
        }
        if let Some(side) = side {
            out.push(Reason { side, text });
        }
    }
    out
}

/// Reads `stage_report`'s arguments, naming every problem at once so the session can fix them
/// in one go.
pub fn from_args(args: &Value) -> Result<Rating, String> {
    let mut problems = Vec::new();
    let outcome = match args.get("outcome").and_then(Value::as_str) {
        Some("done") => Some(Outcome::Done),
        Some("blocked") => Some(Outcome::Blocked),
        Some("check") => Some(Outcome::Check),
        other => {
            problems.push(format!(
                "outcome must be done, blocked or check, not {}",
                other.map_or("nothing".into(), |o| format!("{o:?}"))
            ));
            None
        }
    };
    let score = match args.get("score").and_then(Value::as_u64) {
        Some(s) if s <= 100 => Some(s as u8),
        _ => {
            problems.push("score must be a whole number from 0 to 100".into());
            None
        }
    };
    let summary = text(args, "summary");
    if summary.is_empty() {
        problems.push("summary is empty".into());
    }
    let reasons = reasons(args, &mut problems);
    let check = text(args, "check");
    if outcome == Some(Outcome::Check) && check.is_empty() {
        problems
            .push("check is empty: say what a person should try and what they should see".into());
    } else if check.chars().count() > MAX_CHECK {
        problems.push(format!("check is over {MAX_CHECK} characters"));
    }
    let screenshots: Vec<String> = args
        .get("screenshots")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(|p| p.trim().to_string())
        .collect();
    if screenshots.len() > MAX_SHOTS {
        problems.push(format!(
            "screenshots has {}; at most {MAX_SHOTS}",
            screenshots.len()
        ));
    }
    for (i, shot) in screenshots.iter().enumerate() {
        if !is_picture(shot) {
            problems.push(format!(
                "screenshot {}: {shot:?} must be an absolute path to a .png, .jpg, .gif or .webp",
                i + 1
            ));
        }
    }
    let items = args
        .get("evidence")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if items.is_empty() {
        problems.push("evidence is empty: give at least one item".into());
    } else if items.len() > MAX_EVIDENCE {
        problems.push(format!(
            "evidence has {} items; at most {MAX_EVIDENCE}",
            items.len()
        ));
    }
    let mut evidence = Vec::new();
    for (i, item) in items.iter().take(MAX_EVIDENCE).enumerate() {
        let kind_text = text(item, "kind");
        let kind = evidence_kind(&kind_text);
        if kind.is_none() {
            problems.push(format!(
                "evidence {}: kind {kind_text:?} must be test, command, file, link or note",
                i + 1
            ));
        }
        let detail = text(item, "detail");
        if detail.is_empty() {
            problems.push(format!("evidence {}: detail is empty", i + 1));
        } else if detail.chars().count() > MAX_DETAIL {
            problems.push(format!(
                "evidence {}: detail is over {MAX_DETAIL} characters",
                i + 1
            ));
        }
        if let Some(kind) = kind {
            evidence.push(Evidence { kind, detail });
        }
    }
    match (outcome, score) {
        (Some(outcome), Some(score)) if problems.is_empty() => Ok(Rating {
            outcome,
            score,
            summary,
            evidence,
            reasons,
            handoff: text(args, "handoff"),
            check,
            screenshots,
        }),
        _ => Err(problems.join("; ")),
    }
}

/// Whether `rating` clears `bar`: done, and scored at or above it.
pub fn passes(rating: &Rating, bar: u8) -> bool {
    rating.outcome == Outcome::Done && rating.score >= bar
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn good() -> Value {
        json!({"outcome": "done", "score": 86, "summary": "Reproduced.",
               "evidence": [{"kind": "test", "detail": "e2e: 1 failing"}],
               "reasons": [{"side": "for", "text": "Fails three runs out of three."}],
               "handoff": "Fix save()."})
    }

    #[test]
    fn a_full_report_reads_and_passes_at_or_above_the_bar() {
        let r = from_args(&good()).unwrap();
        assert_eq!(r.outcome, Outcome::Done);
        assert_eq!(r.score, 86);
        assert_eq!(r.evidence[0].kind, EvidenceKind::Test);
        assert_eq!(r.handoff, "Fix save().");
        assert!(passes(&r, 86) && !passes(&r, 87));
    }

    #[test]
    fn blocked_never_passes() {
        let mut a = good();
        a["outcome"] = json!("blocked");
        a["score"] = json!(100);
        assert!(!passes(&from_args(&a).unwrap(), 0));
    }

    #[test]
    fn a_check_says_what_to_try_and_never_passes_by_itself() {
        let mut a = good();
        a["outcome"] = json!("check");
        a["score"] = json!(95);
        let e = from_args(&a).unwrap_err();
        assert!(e.contains("check is empty"), "{e}");
        a["check"] = json!("Open the nl page in Chrome; Always translate stays off.");
        let r = from_args(&a).unwrap();
        assert_eq!(r.outcome, Outcome::Check);
        assert!(r.check.starts_with("Open the nl page"));
        assert!(!passes(&r, 0));
        // Absolute on every OS: "/tmp/x.png" has no drive on Windows.
        let at = |name: &str| std::env::temp_dir().join(name).display().to_string();
        let shots = [at("nl.png"), at("en.JPG")];
        a["screenshots"] = json!(shots);
        assert_eq!(from_args(&a).unwrap().screenshots, shots);
        a["screenshots"] = json!(["shot.png", at("notes.txt")]);
        let e = from_args(&a).unwrap_err();
        assert!(
            e.contains("screenshot 1") && e.contains("screenshot 2"),
            "{e}"
        );
        a["screenshots"] = json!(vec![at("a.png"); 7]);
        assert!(from_args(&a).unwrap_err().contains("at most 6"));
        a.as_object_mut().unwrap().remove("screenshots");
        a["check"] = json!("x".repeat(MAX_CHECK + 1));
        assert!(from_args(&a).unwrap_err().contains("check is over"));
    }

    #[test]
    fn the_handoff_may_be_left_out() {
        let mut a = good();
        a.as_object_mut().unwrap().remove("handoff");
        assert_eq!(from_args(&a).unwrap().handoff, "");
    }

    #[test]
    fn every_missing_piece_is_named_at_once() {
        let e = from_args(&json!({"outcome": "maybe", "score": 101, "summary": " ",
                                  "evidence": [], "reasons": ""}))
        .unwrap_err();
        for says in ["outcome", "score", "summary", "evidence", "reasons"] {
            assert!(e.contains(says), "{says}: {e}");
        }
        assert!(from_args(&json!({})).unwrap_err().contains("score"));
        let mut a = good();
        a["score"] = json!(85.5);
        assert!(from_args(&a).unwrap_err().contains("score"));
    }

    #[test]
    fn reasons_need_a_side_and_text() {
        let mut a = good();
        a["reasons"] = json!([{"side": "for", "text": "Three runs out of three."},
                              {"side": "against", "text": "Not tried on mobile."}]);
        let r = from_args(&a).unwrap();
        assert_eq!(
            r.reasons[1],
            Reason {
                side: Side::Against,
                text: "Not tried on mobile.".into()
            }
        );
        assert_eq!(
            serde_json::to_value(&r).unwrap()["reasons"][1]["side"],
            "against"
        );
        for (reasons, says) in [
            (json!([]), "reasons"),
            (json!("Because."), "reasons"),
            (json!([{"side": "maybe", "text": "x"}]), "maybe"),
            (json!([{"side": "for", "text": " "}]), "reason 1: text"),
            (json!([{"side": "for", "text": "x".repeat(501)}]), "500"),
            (
                json!(
                    (0..21)
                        .map(|_| json!({"side": "for", "text": "x"}))
                        .collect::<Vec<_>>()
                ),
                "20",
            ),
        ] {
            a["reasons"] = reasons;
            let e = from_args(&a).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn evidence_is_bounded() {
        let mut a = good();
        a["evidence"] = json!(
            (0..21)
                .map(|_| json!({"kind": "note", "detail": "x"}))
                .collect::<Vec<_>>()
        );
        assert!(from_args(&a).unwrap_err().contains("20"));
        a["evidence"] = json!([{"kind": "note", "detail": "x".repeat(501)}]);
        assert!(from_args(&a).unwrap_err().contains("500"));
        a["evidence"] = json!([{"kind": "rumour", "detail": "x"}]);
        assert!(from_args(&a).unwrap_err().contains("rumour"));
        a["evidence"] = json!([{"kind": "note", "detail": "  "}]);
        assert!(from_args(&a).unwrap_err().contains("detail"));
    }
}
