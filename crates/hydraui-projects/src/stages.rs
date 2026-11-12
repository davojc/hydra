//! A project's stages (spec §14.8): how its tickets are worked, set in `project.toml` as
//! `[[stage]]` like a pipeline's. A **work** stage makes the change and opens pull requests; a
//! **check** stage passes or fails them, a failure going back to a work stage. Statuses stay
//! the six buckets (`TicketStatus`): a ticket in progress or testing also names its stage.
//! Pure: no I/O.

use serde::{Deserialize, Serialize};

use crate::{TicketMeta, TicketStatus};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageKind {
    #[default]
    Work,
    Check,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub kind: StageKind,
    /// What the stage's session is told, with `{placeholders}`; empty is the default for its
    /// kind. hydraui adds how to finish (the report tools), which can't be changed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// A work stage's prompt after a check sent the ticket back; empty is the default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix_prompt: String,
    /// A skill every session of this stage loads, by name. Empty leaves it to each engineer's
    /// choice on their Mac, then the repo's own and the skill router's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skill: String,
    /// A check stage's: the work stage a failure goes back to. Empty is the nearest work stage
    /// before it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub on_fail: String,
}

/// The placeholders a prompt may use, and what each becomes.
pub const PLACEHOLDERS: [(&str, &str); 9] = [
    ("{id}", "the ticket's id, e.g. PG-12"),
    ("{title}", "the ticket's title"),
    ("{project}", "the project's name"),
    ("{stage}", "this stage's title"),
    ("{branch}", "the work branch"),
    ("{place}", "where the code is checked out"),
    ("{prs}", "the ticket's pull requests"),
    ("{what_failed}", "what the failed check said (fix prompts)"),
    ("{skills}", "which skills to load"),
];

pub const DEFAULT_WORK_PROMPT: &str = "You're doing ticket {id} \"{title}\" of the project \
\"{project}\", {place} {skills} Then call ticket_show: the ticket, its milestone and where the \
project's spec is.";

pub const DEFAULT_FIX_PROMPT: &str = "Ticket {id} \"{title}\" of the project \"{project}\" \
failed a check. Its pull request(s) {prs} are checked out {place} on branch {branch}. {skills} \
Call ticket_show for what the ticket asks; its Check section has the whole report.\n\
What failed: {what_failed}\n\
Fix that within the ticket's scope.";

pub const DEFAULT_CHECK_PROMPT: &str = "You're checking ticket {id} \"{title}\" of the project \
\"{project}\" ({stage}): its pull request(s) {prs}, checked out {place}. {skills} Call ticket_show for \
what the ticket asks and how to tell it's done.\n\
Check that the work does what the ticket says and passes the checks its skills call for. You \
may fix small problems within the ticket's scope: commit and push them to this branch.";

/// The stages a project has when `project.toml` names none: development, then testing.
pub fn defaults() -> Vec<Stage> {
    vec![
        Stage {
            id: "dev".into(),
            title: "Development".into(),
            kind: StageKind::Work,
            ..Default::default()
        },
        Stage {
            id: "testing".into(),
            title: "Testing".into(),
            kind: StageKind::Check,
            ..Default::default()
        },
    ]
}

impl Stage {
    /// Its prompt, or the default for its kind.
    pub fn prompt_text(&self) -> &str {
        if !self.prompt.trim().is_empty() {
            &self.prompt
        } else if self.kind == StageKind::Check {
            DEFAULT_CHECK_PROMPT
        } else {
            DEFAULT_WORK_PROMPT
        }
    }

    /// Its fix prompt, or the default.
    pub fn fix_text(&self) -> &str {
        if self.fix_prompt.trim().is_empty() {
            DEFAULT_FIX_PROMPT
        } else {
            &self.fix_prompt
        }
    }
}

/// `{placeholder}`s in `template` replaced from `values`; unknown ones are left as written.
pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    values
        .iter()
        .fold(template.to_string(), |out, (k, v)| out.replace(k, v))
}

/// What's wrong with `stages`, if anything: none, a blank or repeated id or title, an id that
/// isn't plain, no work stage first, or a check failing back to a stage that isn't an earlier
/// work stage.
pub fn problem(stages: &[Stage]) -> Option<String> {
    if stages.is_empty() {
        return Some("A project needs at least one stage.".into());
    }
    if stages[0].kind != StageKind::Work {
        return Some("The first stage must be a work stage: checks need work to check.".into());
    }
    for (i, st) in stages.iter().enumerate() {
        let plain = !st.id.is_empty()
            && st
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !plain {
            return Some(format!(
                "Stage id {:?} must be lowercase letters, digits and dashes.",
                st.id
            ));
        }
        if st.title.trim().is_empty() {
            return Some(format!("Stage {} needs a title.", st.id));
        }
        if stages[..i].iter().any(|o| o.id == st.id) {
            return Some(format!("Stage id {} is used twice.", st.id));
        }
        if stages[..i]
            .iter()
            .any(|o| o.title.trim().eq_ignore_ascii_case(st.title.trim()))
        {
            return Some(format!("Two stages are called {}.", st.title.trim()));
        }
        if !st.on_fail.is_empty()
            && (st.kind != StageKind::Check
                || !stages[..i]
                    .iter()
                    .any(|o| o.id == st.on_fail && o.kind == StageKind::Work))
        {
            return Some(format!(
                "{}: a failure can only go back to an earlier work stage, not {:?}.",
                st.title, st.on_fail
            ));
        }
    }
    None
}

/// The stage a ticket is at: its own when it names one of `stages` of the right kind for its
/// status, else the first of that kind. None outside in progress and testing, or when the
/// project has no stage of that kind.
pub fn current<'a>(stages: &'a [Stage], t: &TicketMeta) -> Option<&'a Stage> {
    let kind = match t.status {
        TicketStatus::Todo | TicketStatus::InProgress => StageKind::Work,
        TicketStatus::Testing => StageKind::Check,
        _ => return None,
    };
    stages
        .iter()
        .find(|s| s.id == t.stage && s.kind == kind)
        .or_else(|| stages.iter().find(|s| s.kind == kind))
}

/// The status a ticket at `stage` has.
pub fn status_of(stage: &Stage) -> TicketStatus {
    match stage.kind {
        StageKind::Work => TicketStatus::InProgress,
        StageKind::Check => TicketStatus::Testing,
    }
}

/// Whether the ticket has passed every check stage. A ticket from before stages that passed
/// testing counts for the default `testing` stage.
pub fn checks_passed(stages: &[Stage], t: &TicketMeta) -> bool {
    stages
        .iter()
        .filter(|s| s.kind == StageKind::Check)
        .all(|s| {
            t.passed.contains(&s.id)
                || (t.passed.is_empty() && t.check == "passed" && s.id == "testing")
        })
}

/// Whether a done ticket with pull requests skipped a check stage, as tickets do when a stage
/// is added after they finished.
pub fn skipped_checks(stages: &[Stage], t: &TicketMeta) -> bool {
    t.status == TicketStatus::Done && !t.prs.is_empty() && !checks_passed(stages, t)
}

/// Puts ticket `t` at `stage`, as a person moving it does: its status follows, and what it
/// passed from that stage on is forgotten, so those checks run again.
pub fn move_to(stages: &[Stage], t: &mut TicketMeta, stage: &str) -> Result<(), String> {
    let i = stages
        .iter()
        .position(|s| s.id == stage)
        .ok_or_else(|| format!("There's no stage {stage}."))?;
    let later: Vec<&str> = stages[i..].iter().map(|s| s.id.as_str()).collect();
    t.passed.retain(|p| !later.contains(&p.as_str()));
    t.status = status_of(&stages[i]);
    t.stage = stage.to_string();
    t.check.clear();
    t.check_note.clear();
    t.check_failures = 0;
    Ok(())
}

/// A work session reported: on to the next stage, with `prs`. No pull requests means nothing
/// to check: it's done, the summary its result.
pub fn after_report(stages: &[Stage], t: &mut TicketMeta, prs: Vec<String>) {
    let at = current(stages, t).map(|s| s.id.clone());
    t.prs = prs;
    t.check.clear();
    if t.prs.is_empty() {
        t.status = TicketStatus::Done;
        t.stage.clear();
        return;
    }
    let i = at
        .and_then(|id| stages.iter().position(|s| s.id == id))
        .unwrap_or(0);
    // New work: every check after this stage runs again.
    let later: Vec<&str> = stages[i..].iter().map(|s| s.id.as_str()).collect();
    t.passed.retain(|p| !later.contains(&p.as_str()));
    enter(stages.get(i + 1), t);
}

/// A check session finished. Passed: on to the next stage, or in review after the last.
/// Failed: back to its `on_fail` work stage to fix, unless it has now failed `limit` times,
/// when it stays for a person.
pub fn after_check(stages: &[Stage], t: &mut TicketMeta, passed: bool, limit: u32) {
    let Some(at) = current(stages, t).cloned() else {
        return;
    };
    let i = stages.iter().position(|s| s.id == at.id).unwrap_or(0);
    if passed {
        if !t.passed.contains(&at.id) {
            t.passed.push(at.id.clone());
        }
        t.check_failures = 0;
        enter(stages.get(i + 1), t);
        if t.status == TicketStatus::InReview {
            t.check = "passed".into();
        }
        return;
    }
    t.check = "failed".into();
    t.check_failures += 1;
    if t.check_failures >= limit {
        // Failed again: the run stops here for a person.
        t.stage = at.id;
        return;
    }
    let back = stages[..i]
        .iter()
        .rev()
        .find(|s| s.kind == StageKind::Work && (at.on_fail.is_empty() || s.id == at.on_fail))
        .or_else(|| stages.iter().find(|s| s.kind == StageKind::Work));
    if let Some(w) = back {
        t.status = TicketStatus::InProgress;
        t.stage = w.id.clone();
    }
}

/// Puts `t` at `next`, fresh for its session; past the last stage, in review.
fn enter(next: Option<&Stage>, t: &mut TicketMeta) {
    match next {
        Some(n) => {
            t.status = status_of(n);
            t.stage = n.id.clone();
            t.check.clear();
        }
        None => {
            t.status = TicketStatus::InReview;
            t.stage.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(id: &str, kind: StageKind) -> Stage {
        Stage {
            id: id.into(),
            title: id.to_uppercase(),
            kind,
            ..Default::default()
        }
    }

    /// dev → testing → docs (work) → security (check, failing back to dev).
    fn four() -> Vec<Stage> {
        let mut security = stage("security", StageKind::Check);
        security.on_fail = "dev".into();
        vec![
            stage("dev", StageKind::Work),
            stage("testing", StageKind::Check),
            stage("docs", StageKind::Work),
            security,
        ]
    }

    fn at(status: TicketStatus, stage: &str) -> TicketMeta {
        TicketMeta {
            title: "T".into(),
            status,
            stage: stage.into(),
            prs: vec!["https://github.com/o/r/pull/1".into()],
            ..Default::default()
        }
    }

    #[test]
    fn the_defaults_are_development_then_testing() {
        let d = defaults();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].id.as_str(), d[0].kind), ("dev", StageKind::Work));
        assert_eq!((d[1].id.as_str(), d[1].kind), ("testing", StageKind::Check));
        assert_eq!(d[0].title, "Development");
        assert_eq!(d[1].title, "Testing");
        assert_eq!(problem(&d), None);
        assert_eq!(d[0].prompt_text(), DEFAULT_WORK_PROMPT);
        assert_eq!(d[0].fix_text(), DEFAULT_FIX_PROMPT);
        assert_eq!(d[1].prompt_text(), DEFAULT_CHECK_PROMPT);
        let mut own = d[1].clone();
        own.prompt = "Mine {id}".into();
        assert_eq!(own.prompt_text(), "Mine {id}");
    }

    #[test]
    fn placeholders_are_filled_and_unknown_ones_left() {
        assert_eq!(
            render(
                "{id}: {title} {nope} {id}",
                &[("{id}", "PG-1"), ("{title}", "Share")]
            ),
            "PG-1: Share {nope} PG-1"
        );
    }

    #[test]
    fn bad_stages_say_why() {
        assert!(problem(&[]).unwrap().contains("at least one"));
        let first_check = vec![stage("testing", StageKind::Check)];
        assert!(problem(&first_check).unwrap().contains("work stage"));
        let twice = vec![
            stage("dev", StageKind::Work),
            stage("dev", StageKind::Check),
        ];
        assert!(problem(&twice).unwrap().contains("twice"));
        let spaced = vec![stage("Dev work", StageKind::Work)];
        assert!(problem(&spaced).unwrap().contains("lowercase"));
        let mut untitled = stage("dev", StageKind::Work);
        untitled.title = " ".into();
        assert!(problem(&[untitled]).unwrap().contains("title"));
        let mut bad = four();
        bad[3].on_fail = "testing".into();
        assert!(problem(&bad).unwrap().contains("earlier work stage"));
        bad[3].on_fail = "docs".into();
        assert_eq!(problem(&bad), None);
        bad[1].on_fail = "docs".into();
        assert!(problem(&bad).unwrap().contains("earlier work stage"));
    }

    #[test]
    fn a_ticket_is_at_its_stage_or_the_first_of_its_kind() {
        let s = four();
        assert_eq!(
            current(&s, &at(TicketStatus::InProgress, "docs"))
                .unwrap()
                .id,
            "docs"
        );
        assert_eq!(
            current(&s, &at(TicketStatus::InProgress, "")).unwrap().id,
            "dev"
        );
        assert_eq!(current(&s, &at(TicketStatus::Todo, "")).unwrap().id, "dev");
        // Its stage was taken out, or is of the other kind: the first of its kind.
        assert_eq!(
            current(&s, &at(TicketStatus::InProgress, "gone"))
                .unwrap()
                .id,
            "dev"
        );
        assert_eq!(
            current(&s, &at(TicketStatus::Testing, "docs")).unwrap().id,
            "testing"
        );
        assert_eq!(
            current(&s, &at(TicketStatus::Testing, "security"))
                .unwrap()
                .id,
            "security"
        );
        assert!(current(&s, &at(TicketStatus::InReview, "")).is_none());
        assert!(current(&s[..1], &at(TicketStatus::Testing, "")).is_none());
    }

    #[test]
    fn a_report_moves_on_to_the_next_stage() {
        let s = four();
        let mut t = at(TicketStatus::InProgress, "dev");
        t.check = "failed".into();
        t.passed = vec!["testing".into(), "security".into()];
        after_report(&s, &mut t, vec!["https://github.com/o/r/pull/2".into()]);
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::Testing, "testing")
        );
        assert_eq!(t.prs, vec!["https://github.com/o/r/pull/2".to_string()]);
        assert_eq!(t.check, "");
        // New work: every later check runs again.
        assert!(t.passed.is_empty());

        let mut t = at(TicketStatus::InProgress, "docs");
        t.passed = vec!["testing".into()];
        let prs = t.prs.clone();
        after_report(&s, &mut t, prs);
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::Testing, "security")
        );
        assert_eq!(t.passed, vec!["testing".to_string()]);
    }

    #[test]
    fn a_report_after_the_last_stage_is_in_review_and_none_without_prs_is_done() {
        let s = vec![stage("dev", StageKind::Work)];
        let mut t = at(TicketStatus::InProgress, "dev");
        let prs = t.prs.clone();
        after_report(&s, &mut t, prs);
        assert_eq!((t.status, t.stage.as_str()), (TicketStatus::InReview, ""));
        let mut t = at(TicketStatus::InProgress, "dev");
        after_report(&four(), &mut t, vec![]);
        assert_eq!((t.status, t.stage.as_str()), (TicketStatus::Done, ""));
    }

    #[test]
    fn a_passed_check_moves_on_and_the_last_one_is_in_review() {
        let s = four();
        let mut t = at(TicketStatus::Testing, "testing");
        after_check(&s, &mut t, true, 2);
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::InProgress, "docs")
        );
        assert_eq!(t.passed, vec!["testing".to_string()]);
        assert_eq!(t.check, "");

        let mut t = at(TicketStatus::Testing, "security");
        t.passed = vec!["testing".into()];
        after_check(&s, &mut t, true, 2);
        assert_eq!((t.status, t.stage.as_str()), (TicketStatus::InReview, ""));
        assert_eq!(t.check, "passed");
        assert!(checks_passed(&s, &t));
    }

    #[test]
    fn a_failed_check_goes_back_to_its_work_stage_then_to_a_person() {
        let s = four();
        let mut t = at(TicketStatus::Testing, "security");
        after_check(&s, &mut t, false, 2);
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::InProgress, "dev")
        );
        assert_eq!((t.check.as_str(), t.check_failures), ("failed", 1));

        // With no on_fail: the nearest work stage before it.
        let mut t = at(TicketStatus::Testing, "testing");
        after_check(&s, &mut t, false, 2);
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::InProgress, "dev")
        );

        let mut t = at(TicketStatus::Testing, "testing");
        t.check_failures = 1;
        after_check(&s, &mut t, false, 2);
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::Testing, "testing")
        );
        assert_eq!((t.check.as_str(), t.check_failures), ("failed", 2));
    }

    #[test]
    fn checks_passed_counts_every_check_stage_and_old_passes() {
        let s = four();
        let mut t = at(TicketStatus::InReview, "");
        assert!(!checks_passed(&s, &t));
        t.passed = vec!["testing".into()];
        assert!(!checks_passed(&s, &t));
        t.passed.push("security".into());
        assert!(checks_passed(&s, &t));
        // From before stages: check = passed meant testing passed.
        let mut old = at(TicketStatus::InReview, "");
        old.check = "passed".into();
        assert!(checks_passed(&defaults(), &old));
        assert!(!checks_passed(&s, &old));
        // No check stages: nothing to pass.
        assert!(checks_passed(&s[..1], &at(TicketStatus::Done, "")));
    }

    #[test]
    fn a_done_ticket_that_skipped_a_check_is_found() {
        let d = defaults();
        let mut t = at(TicketStatus::Done, "");
        assert!(skipped_checks(&d, &t));
        t.passed = vec!["testing".into()];
        assert!(!skipped_checks(&d, &t));
        // No pull requests: a write-up, nothing to check.
        let mut t = at(TicketStatus::Done, "");
        t.prs.clear();
        assert!(!skipped_checks(&d, &t));
        assert!(!skipped_checks(&d, &at(TicketStatus::InReview, "")));
    }

    #[test]
    fn moving_a_ticket_to_a_stage_reruns_it_and_what_follows() {
        let s = four();
        let mut t = at(TicketStatus::Done, "");
        t.passed = vec!["testing".into(), "security".into()];
        (t.check, t.check_note, t.check_failures) = ("passed".into(), "ok".into(), 1);
        move_to(&s, &mut t, "testing").unwrap();
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::Testing, "testing")
        );
        assert!(t.passed.is_empty());
        assert_eq!((t.check.as_str(), t.check_failures), ("", 0));

        let mut t = at(TicketStatus::InReview, "");
        t.passed = vec!["testing".into(), "security".into()];
        move_to(&s, &mut t, "docs").unwrap();
        assert_eq!(
            (t.status, t.stage.as_str()),
            (TicketStatus::InProgress, "docs")
        );
        assert_eq!(t.passed, vec!["testing".to_string()]);
        assert!(move_to(&s, &mut t, "nope").is_err());
    }

    #[test]
    fn stages_read_and_write_as_toml() {
        let text = r#"
[[stage]]
id = "dev"
title = "Development"
skill = "angular"

[[stage]]
id = "testing"
title = "Testing"
kind = "check"
on_fail = "dev"
prompt = "Check {id}"
"#;
        #[derive(Deserialize, Serialize)]
        struct F {
            stage: Vec<Stage>,
        }
        let f: F = toml::from_str(text).unwrap();
        assert_eq!(f.stage[0].kind, StageKind::Work);
        assert_eq!(f.stage[0].skill, "angular");
        assert_eq!(f.stage[1].kind, StageKind::Check);
        assert_eq!(f.stage[1].prompt_text(), "Check {id}");
        let back = toml::to_string(&f).unwrap();
        assert!(!back.contains("fix_prompt"), "{back}");
        assert!(back.contains("kind = \"check\""), "{back}");
    }
}
