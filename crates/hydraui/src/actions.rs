//! Actions (core spec §80): work a stage drafts for a person to approve, such as a reply to a
//! customer's email. A stage proposes one with `action_propose` and waits; the Actions page
//! shows it; Approve, Reject or Edit tells the stage what happened. Each action is a file,
//! `<hydra home>/hydraui/actions/<id>.json`, so a restart keeps them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How long an approved or rejected action stays listed, in days.
pub const KEEP_DAYS: u64 = 7;

/// Where an action has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// Waiting for a person.
    Waiting,
    /// A person asked for a change; the stage is revising it.
    Changing { ask: String },
    /// Approved; `draft` is the Gmail draft it was saved as, for an email's reply.
    Approved {
        #[serde(default)]
        draft: Option<String>,
    },
    Rejected {
        #[serde(default)]
        why: String,
    },
}

impl State {
    pub fn open(&self) -> bool {
        matches!(self, State::Waiting | State::Changing { .. })
    }

    /// As the page's row says it.
    pub fn word(&self) -> &'static str {
        match self {
            State::Waiting => "Waiting for you",
            State::Changing { .. } => "Changing…",
            State::Approved { draft: Some(_) } => "Approved · saved as a Gmail draft",
            State::Approved { draft: None } => "Approved",
            State::Rejected { .. } => "Rejected",
        }
    }
}

/// The email a reply answers, when its run came from one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Email {
    /// The Gmail thread the draft is saved in.
    pub thread: String,
    pub subject: String,
    pub from: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub run: String,
    pub ticket: String,
    /// The pipeline's file name and its name.
    pub pipeline: String,
    pub pipeline_name: String,
    pub stage: usize,
    pub stage_title: String,
    pub title: String,
    /// What it answers, as text: an email's `email.md`, or the stage's `context`.
    pub context: String,
    pub proposal: String,
    #[serde(default)]
    pub email: Option<Email>,
    #[serde(flatten)]
    pub state: State,
    /// 1 for the first draft; up one each time the stage revises it after Edit.
    pub revision: u32,
    /// Log times (`2026-11-12 19:26:13.372`).
    pub created: String,
    pub updated: String,
}

/// An action's id: its run and stage, so a revision lands on the same one.
pub fn id_for(run: &str, stage: usize) -> String {
    let safe: String = run
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{safe}-s{stage}")
}

/// What a stage proposes.
pub struct Proposal<'a> {
    pub run: &'a str,
    pub ticket: &'a str,
    pub pipeline: &'a str,
    pub pipeline_name: &'a str,
    pub stage: usize,
    pub stage_title: &'a str,
    pub title: &'a str,
    pub context: &'a str,
    pub proposal: &'a str,
    pub email: Option<Email>,
    pub now: &'a str,
}

/// The action `p` makes: a new one, or `existing` revised when a person asked for a change.
/// An action already waiting for a person, or already decided, isn't proposed again.
pub fn propose(existing: Option<Action>, p: Proposal) -> Result<Action, String> {
    let (title, proposal) = (p.title.trim(), p.proposal.trim());
    if title.is_empty() {
        return Err("give the action a title: what the person approves, in a line".into());
    }
    if proposal.is_empty() {
        return Err("the proposal is empty: put the draft itself in proposal".into());
    }
    let revision = match &existing {
        None => 1,
        Some(a) => match &a.state {
            State::Changing { .. } => a.revision + 1,
            State::Waiting => {
                return Err("your action already waits for a person: stop until they act".into());
            }
            _ => return Err("a person already decided this action: report your stage".into()),
        },
    };
    let created = existing
        .as_ref()
        .map_or_else(|| p.now.to_string(), |a| a.created.clone());
    Ok(Action {
        id: id_for(p.run, p.stage),
        run: p.run.into(),
        ticket: p.ticket.into(),
        pipeline: p.pipeline.into(),
        pipeline_name: p.pipeline_name.into(),
        stage: p.stage,
        stage_title: p.stage_title.into(),
        title: title.into(),
        context: p.context.trim().into(),
        proposal: proposal.into(),
        email: p.email.or_else(|| existing.and_then(|a| a.email)),
        state: State::Waiting,
        revision,
        created,
        updated: p.now.into(),
    })
}

/// An email's subject and its latest sender, from its `email.md` (`mail_watch::email_md`).
pub fn email_heading(md: &str) -> (String, String) {
    let subject = md
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .unwrap_or("(no subject)")
        .trim()
        .to_string();
    let from = md
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("From: "))
        .unwrap_or("")
        .trim()
        .to_string();
    (subject, from)
}

/// A reply's subject: `Re: <subject>`, once.
pub fn reply_subject(subject: &str) -> String {
    let s = subject.trim();
    if s.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("re:")) {
        s.to_string()
    } else {
        format!("Re: {s}")
    }
}

/// A header value as RFC 2047 needs it: as is in ASCII, else a UTF-8 encoded word.
fn header_text(v: &str) -> String {
    use base64::Engine;
    if v.is_ascii() {
        v.to_string()
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(v.as_bytes())
        )
    }
}

/// The reply as a whole message for Gmail's drafts: to `to`, threaded under `in_reply_to`
/// (a `Message-ID`, when known), plain text.
pub fn reply_mime(to: &str, subject: &str, in_reply_to: &str, body: &str) -> String {
    let mut m = format!(
        "To: {to}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\n\
         Content-Transfer-Encoding: 8bit\r\n",
        header_text(&reply_subject(subject))
    );
    let id = in_reply_to.trim();
    if !id.is_empty() {
        m.push_str(&format!("In-Reply-To: {id}\r\nReferences: {id}\r\n"));
    }
    m.push_str("\r\n");
    m.push_str(&body.replace("\r\n", "\n").replace('\n', "\r\n"));
    m
}

/// Typed to the stage when a person approves. `draft`: saved as a Gmail draft.
pub fn approved_prompt(text: &str, draft: bool) -> String {
    let saved = if draft {
        "hydraui saved it as a Gmail draft reply in the email's thread; a person sends it from \
         Gmail. "
    } else {
        ""
    };
    format!(
        "Your action was approved, as this text (the person may have edited it):\n\n{text}\n\n\
         {saved}Don't send or post it yourself. Report your stage done, saying it was approved."
    )
}

/// Typed to the stage when a person rejects it.
pub fn rejected_prompt(why: &str) -> String {
    let why = match why.trim() {
        "" => "no reason given".to_string(),
        w => w.to_string(),
    };
    format!(
        "Your action was rejected ({why}). Nothing was saved. Don't draft it again: report your \
         stage done, saying it was rejected and why."
    )
}

/// Typed to the stage when a person asks for a change.
pub fn edit_prompt(ask: &str, current: &str) -> String {
    format!(
        "A person asked for a change to your action: {ask}\n\nThe draft as it stands (they may \
         have edited it):\n\n{current}\n\nRevise it as they ask, then call action_propose again \
         with the new version, and stop until they act."
    )
}

fn dir(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("actions")
}

pub fn save(hydra_root: &Path, a: &Action) -> std::io::Result<()> {
    let d = dir(hydra_root);
    std::fs::create_dir_all(&d)?;
    std::fs::write(
        d.join(format!("{}.json", a.id)),
        serde_json::to_string_pretty(a).map_err(std::io::Error::other)?,
    )
}

pub fn load(hydra_root: &Path, id: &str) -> Option<Action> {
    let t = std::fs::read_to_string(dir(hydra_root).join(format!("{id}.json"))).ok()?;
    serde_json::from_str(&t).ok()
}

/// Every action worth listing at log time `now`, newest first: the open ones, and the decided
/// ones from the last `KEEP_DAYS`.
pub fn list(hydra_root: &Path, now: &str) -> Vec<Action> {
    let mut all: Vec<Action> = std::fs::read_dir(dir(hydra_root))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .filter(|a: &Action| {
            a.state.open()
                || crate::run_history::ago(&a.updated, now)
                    .is_none_or(|d| d.as_secs() < KEEP_DAYS * 86_400)
        })
        .collect();
    all.sort_by(|a, b| b.updated.cmp(&a.updated));
    all
}

/// How many wait for a person.
pub fn waiting(actions: &[Action]) -> usize {
    actions.iter().filter(|a| a.state == State::Waiting).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal<'a>(text: &'a str, now: &'a str) -> Proposal<'a> {
        Proposal {
            run: "MAIL-18C2-1",
            ticket: "MAIL-18C2",
            pipeline: "integrations-emails",
            pipeline_name: "Integrations Emails",
            stage: 1,
            stage_title: "Handle the email",
            title: "Reply to Ann about sharing",
            context: "# Sharing\n\nFrom: Ann <ann@acme.com>\n\nHow do I share?",
            proposal: text,
            email: Some(Email {
                thread: "t1".into(),
                subject: "Sharing".into(),
                from: "Ann <ann@acme.com>".into(),
            }),
            now,
        }
    }

    #[test]
    fn a_proposal_waits_and_a_revision_lands_on_the_same_action() {
        let a = propose(None, proposal("Hi Ann,", "2026-11-12 10:00:00.000")).unwrap();
        assert_eq!((a.id.as_str(), a.revision), ("MAIL-18C2-1-s1", 1));
        assert_eq!(a.state, State::Waiting);
        // Waiting already: not again.
        assert!(
            propose(Some(a.clone()), proposal("x", "2026-11-12 10:01:00.000"))
                .unwrap_err()
                .contains("waits")
        );
        let asked = Action {
            state: State::Changing {
                ask: "shorter".into(),
            },
            ..a.clone()
        };
        let b = propose(Some(asked), proposal("Hi Ann.", "2026-11-12 10:05:00.000")).unwrap();
        assert_eq!((&b.id, b.revision, &b.state), (&a.id, 2, &State::Waiting));
        assert_eq!(b.created, "2026-11-12 10:00:00.000");
        assert_eq!(b.updated, "2026-11-12 10:05:00.000");
        assert_eq!(b.proposal, "Hi Ann.");
        // Decided: not again.
        let done = Action {
            state: State::Rejected { why: "no".into() },
            ..b
        };
        assert!(propose(Some(done), proposal("x", "now")).is_err());
        assert!(propose(None, proposal("  ", "now")).is_err());
    }

    #[test]
    fn an_email_reply_is_threaded_and_says_re_once() {
        assert_eq!(reply_subject("Sharing"), "Re: Sharing");
        assert_eq!(reply_subject("RE: Sharing"), "RE: Sharing");
        let m = reply_mime("ann@acme.com", "Sharing", "<m1@acme.com>", "Hi Ann,\nYes.");
        assert!(
            m.starts_with("To: ann@acme.com\r\nSubject: Re: Sharing\r\n"),
            "{m}"
        );
        assert!(m.contains("In-Reply-To: <m1@acme.com>\r\nReferences: <m1@acme.com>\r\n"));
        assert!(m.ends_with("\r\n\r\nHi Ann,\r\nYes."), "{m}");
        // A subject beyond ASCII is an encoded word; no Message-ID, no threading headers.
        let m = reply_mime("a@b.c", "Café", "", "x");
        assert!(m.contains("Subject: =?UTF-8?B?"), "{m}");
        assert!(!m.contains("In-Reply-To"));
    }

    #[test]
    fn an_emails_heading_is_its_subject_and_latest_sender() {
        let md = "# Sharing\n\n## Message 1 of 2\n\nFrom: Ann <ann@acme.com>\nTo: x\n\nHi\n\n\
                  ## Message 2 of 2\n\nFrom: Bob <bob@acme.com>\nTo: x\n\nAlso";
        assert_eq!(
            email_heading(md),
            ("Sharing".to_string(), "Bob <bob@acme.com>".to_string())
        );
        assert_eq!(email_heading("").0, "(no subject)");
    }

    #[test]
    fn the_prompts_tell_the_stage_what_happened() {
        let p = approved_prompt("Hi Ann", true);
        assert!(p.contains("Hi Ann") && p.contains("Gmail draft") && p.contains("Don't send"));
        assert!(!approved_prompt("Hi", false).contains("Gmail"));
        assert!(rejected_prompt("").contains("no reason given"));
        assert!(rejected_prompt("wrong product").contains("wrong product"));
        let e = edit_prompt("make it shorter", "Hi Ann");
        assert!(e.contains("make it shorter") && e.contains("action_propose"));
    }

    #[test]
    fn actions_are_kept_listed_newest_first_and_old_decided_ones_drop_off() {
        let home = tempfile::tempdir().unwrap();
        let a = propose(None, proposal("Hi", "2026-11-01 10:00:00.000")).unwrap();
        let b = Action {
            id: "other".into(),
            updated: "2026-11-12 09:00:00.000".into(),
            ..a.clone()
        };
        let old = Action {
            id: "old".into(),
            state: State::Approved { draft: None },
            updated: "2026-11-01 09:00:00.000".into(),
            ..a.clone()
        };
        for x in [&a, &b, &old] {
            save(home.path(), x).unwrap();
        }
        let now = "2026-11-12 10:00:00.000";
        let ids: Vec<String> = list(home.path(), now).into_iter().map(|a| a.id).collect();
        // The old open one stays; the old decided one is gone.
        assert_eq!(ids, vec!["other".to_string(), a.id.clone()]);
        assert_eq!(load(home.path(), &a.id), Some(a.clone()));
        assert_eq!(waiting(&list(home.path(), now)), 2);
    }
}
