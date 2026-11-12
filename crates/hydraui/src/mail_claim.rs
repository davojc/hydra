//! One person per email (email triage spec §5): a thread is claimed with Gmail labels, which
//! everyone reading the mailbox sees at once and so decide, and a file in the team's swarm,
//! which shows hydraui who has what and covers the moments before a label lands. Two people
//! claiming at once both see both labels after the settle, and every machine picks the same
//! winner. Pure but for the swarm's files.
//!
//! A Google group has no mailbox of its own: each member reads their own copy of an email, with
//! its own thread id, and a label on one copy is invisible on another (§5.1). So a swarm claim
//! is keyed by the email's `Message-ID` (the same in every copy, `mail_watch::claim_key`) and
//! says which mailbox made it. A claim from another mailbox is the only sign of it this one will
//! ever see: it holds while its file is there, and counts in the settle as a label would.

use std::path::{Path, PathBuf};

/// The label every claimed thread carries, so `-label:hydra-claimed` leaves it out of searches.
pub const CLAIMED: &str = crate::mail_watch::CLAIMED;
/// The label a finished thread carries instead.
pub const DONE: &str = crate::mail_watch::DONE;

/// How long after claiming the labels are read again for a rival claim.
pub const SETTLE: std::time::Duration = std::time::Duration::from_secs(20);

/// How long a swarm claim without its label still holds a thread: long enough for its label
/// to land, not so long that a claim given up on blocks the thread.
const RECENT_SECS: u64 = 10 * 60;

/// `name` as it goes in a label and a file name: lowercase letters, digits and `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Person `who`'s label: `hydra/claimed/<who>`.
pub fn label_of(who: &str) -> String {
    format!("{CLAIMED}/{who}")
}

/// Who claims a thread, from its label names: each `hydra/claimed/<who>`.
pub fn claimants(label_names: &[String]) -> Vec<String> {
    let prefix = format!("{CLAIMED}/");
    label_names
        .iter()
        .filter_map(|n| n.strip_prefix(&prefix))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// A claim in the swarm: who, when (a log time, `2026-11-11 09:00:01.123`), and the mailbox
/// that made it; empty for a claim written before claims said (read as this mailbox's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub who: String,
    pub at: String,
    pub mailbox: String,
}

impl Claim {
    /// Made in a mailbox other than `mine`: its label can never show here (a group's copies).
    fn elsewhere(&self, mine: &str) -> bool {
        !self.mailbox.is_empty() && !self.mailbox.eq_ignore_ascii_case(mine)
    }
}

/// `<swarm folder>/claims/<key>--<who>.md`: one file per person, so two claims never make a
/// sync conflict, and `.md`, which every swarm syncs. `key` is the email's
/// (`mail_watch::claim_key`).
pub fn claim_file(swarm: &Path, key: &str, who: &str) -> PathBuf {
    swarm.join("claims").join(format!("{key}--{who}.md"))
}

/// Writes `who`'s claim, made in `mailbox`, on the email `key`: who, when, which mailbox, the
/// run's ticket and the subject. Never the email itself.
#[allow(clippy::too_many_arguments)]
pub fn write_claim(
    swarm: &Path,
    key: &str,
    who: &str,
    at: &str,
    ticket: &str,
    subject: &str,
    mailbox: &str,
) -> std::io::Result<()> {
    let path = claim_file(swarm, key, who);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        format!(
            "# Claimed: {subject}\n\nclaimed by: {who}\nat: {at}\nmailbox: {mailbox}\n\
             run: {ticket}\nemail: {key}\n"
        ),
    )
}

/// The claims on the email `key` in the swarm's folder. Sync's conflict copies aren't claims.
pub fn read_claims(swarm: &Path, key: &str) -> Vec<Claim> {
    let prefix = format!("{key}--");
    let Ok(entries) = std::fs::read_dir(swarm.join("claims")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let who = name.strip_prefix(&prefix)?.strip_suffix(".md")?.to_string();
            // A conflict copy (`… 2.md`) or anything else that isn't a plain name isn't a claim.
            if who.is_empty() || slug(&who) != who {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).ok()?;
            let field = |name: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(name))
                    .map(|a| a.trim().to_string())
            };
            let at = field("at:")?;
            let mailbox = field("mailbox:").unwrap_or_default();
            Some(Claim { who, at, mailbox })
        })
        .collect()
}

/// Removes `who`'s claim on the email `key`, if there is one.
pub fn remove_claim(swarm: &Path, key: &str, who: &str) -> std::io::Result<()> {
    match std::fs::remove_file(claim_file(swarm, key, who)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What a look finds about a thread before claiming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Nobody has it: claim it.
    Free,
    /// `me` has it already (a look after a restart).
    Mine,
    /// Someone else has it.
    Theirs(String),
}

/// Whether `me`, reading `mailbox`, may claim a thread with these label claimants and swarm
/// claims, at `now`. Gmail decides for this mailbox: anyone else's label is theirs. Someone
/// else's swarm claim made in this mailbox holds only while recent, the moments before their
/// label lands; one made in another mailbox (a group's copy) holds while it's there.
pub fn look(me: &str, mailbox: &str, labelled: &[String], swarm: &[Claim], now: &str) -> Found {
    if let Some(other) = labelled.iter().find(|w| *w != me) {
        return Found::Theirs(other.clone());
    }
    if let Some(c) = swarm.iter().find(|c| c.who != me && c.elsewhere(mailbox)) {
        return Found::Theirs(c.who.clone());
    }
    if labelled.iter().any(|w| w == me) {
        return Found::Mine;
    }
    let cut = crate::run_feed::before(now, RECENT_SECS);
    swarm
        .iter()
        .filter(|c| c.who != me)
        .find(|c| cut.as_deref().is_none_or(|cut| c.at.as_str() > cut))
        .map_or(Found::Free, |c| Found::Theirs(c.who.clone()))
}

/// After the settle: whether `me` keeps the thread. Alone, yes. With rivals, the earliest
/// swarm claim wins (one not seen counts as latest), then the lowest name, so every machine
/// picks the same one. Without `me`'s label at all, it's lost to whoever has it.
/// Someone claiming from another mailbox (a group's copy) is a rival as a label would be.
pub fn settle(me: &str, mailbox: &str, labelled: &[String], swarm: &[Claim]) -> Result<(), String> {
    if !labelled.iter().any(|w| w == me) {
        return Err(labelled
            .first()
            .cloned()
            .unwrap_or_else(|| "someone".into()));
    }
    let mut rivals: Vec<String> = labelled.to_vec();
    for c in swarm.iter().filter(|c| c.elsewhere(mailbox)) {
        if !rivals.contains(&c.who) {
            rivals.push(c.who.clone());
        }
    }
    let labelled = &rivals;
    let when = |who: &str| {
        swarm
            .iter()
            .filter(|c| c.who == who)
            .map(|c| c.at.as_str())
            .min()
            // Not seen: later than any time written.
            .unwrap_or("~")
    };
    let winner = labelled
        .iter()
        .min_by(|a, b| when(a).cmp(when(b)).then(a.cmp(b)))
        .expect("me is among them");
    if winner == me {
        Ok(())
    } else {
        Err(winner.clone())
    }
}

use crate::gmail::{self, Endpoints};
use crate::mail_watch::Mailed;

/// What claiming a look's threads came to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Claimed {
    /// Threads `me` holds now: run them.
    pub kept: Vec<Mailed>,
    /// Threads someone else held before `me` tried, with who.
    pub theirs: Vec<(Mailed, String)>,
    /// Threads `me` claimed but lost to a rival claiming at the same time, with who.
    pub lost: Vec<(Mailed, String)>,
}

/// Claims `items` for `me` (spec §5): those nobody has get `me`'s claim file in `swarm` and
/// both labels; after `settle`, the labels are read again and a rival claim settled, the loser
/// taking its claim back. `now` is the log time, for claim files.
#[allow(clippy::too_many_arguments)]
pub fn claim(
    ep: &Endpoints,
    access: &str,
    me: &str,
    mailbox: &str,
    swarm: Option<&Path>,
    items: Vec<Mailed>,
    now: &dyn Fn() -> String,
    settle_for: std::time::Duration,
) -> Result<Claimed, String> {
    let mut known = gmail::labels(ep, access)?;
    let claimed = gmail::label_id(ep, access, CLAIMED, &mut known)?;
    let mine = gmail::label_id(ep, access, &label_of(me), &mut known)?;
    let names = |ids: Vec<String>, known: &[(String, String)]| -> Vec<String> {
        ids.iter()
            .filter_map(|id| known.iter().find(|(k, _)| k == id).map(|(_, n)| n.clone()))
            .collect()
    };
    let swarm_claims = |m: &Mailed| swarm.map(|s| read_claims(s, m.key())).unwrap_or_default();
    let mut out = Claimed::default();
    let mut trying = Vec::new();
    for m in items {
        let on = names(gmail::thread_label_ids(ep, access, &m.thread)?, &known);
        match look(me, mailbox, &claimants(&on), &swarm_claims(&m), &now()) {
            Found::Mine => out.kept.push(m),
            Found::Theirs(who) => out.theirs.push((m, who)),
            Found::Free => {
                if let Some(s) = swarm {
                    write_claim(s, m.key(), me, &now(), &m.ticket, &m.subject, mailbox)
                        .map_err(|e| format!("can't write the claim to the swarm: {e}"))?;
                }
                gmail::modify_thread(ep, access, &m.thread, &[claimed.clone(), mine.clone()], &[])?;
                trying.push(m);
            }
        }
    }
    if trying.is_empty() {
        return Ok(out);
    }
    std::thread::sleep(settle_for);
    // A rival's label may be new to this machine.
    let known = gmail::labels(ep, access)?;
    for m in trying {
        let on = names(gmail::thread_label_ids(ep, access, &m.thread)?, &known);
        match settle(me, mailbox, &claimants(&on), &swarm_claims(&m)) {
            Ok(()) => out.kept.push(m),
            Err(who) => {
                gmail::modify_thread(ep, access, &m.thread, &[], std::slice::from_ref(&mine))?;
                if let Some(s) = swarm {
                    remove_claim(s, m.key(), me).map_err(|e| e.to_string())?;
                }
                out.lost.push((m, who));
            }
        }
    }
    Ok(out)
}

/// Ends `me`'s claim on `thread` (the email `key`) once its run ends: `done` swaps the claim
/// labels for `hydra/done`; otherwise they come off, so the thread is free for someone else.
/// Its claim file goes either way.
pub fn release(
    ep: &Endpoints,
    access: &str,
    me: &str,
    swarm: Option<&Path>,
    thread: &str,
    key: &str,
    done: bool,
) -> Result<(), String> {
    let mut known = gmail::labels(ep, access)?;
    let claimed = gmail::label_id(ep, access, CLAIMED, &mut known)?;
    let mine = gmail::label_id(ep, access, &label_of(me), &mut known)?;
    let add = if done {
        vec![gmail::label_id(ep, access, DONE, &mut known)?]
    } else {
        Vec::new()
    };
    gmail::modify_thread(ep, access, thread, &add, &[claimed, mine])?;
    if let Some(s) = swarm {
        remove_claim(s, key, me).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail::testing::fake;

    fn mailed(id: &str) -> Mailed {
        Mailed {
            thread: id.into(),
            ticket: crate::mail_watch::ticket(id),
            subject: format!("About {id}"),
            from: "Ann <ann@acme.com>".into(),
            key: String::new(),
        }
    }

    const LABELS: &str = r#"{"labels":[{"id":"L1","name":"hydra/claimed"},
        {"id":"L2","name":"hydra/claimed/jordan"},{"id":"L3","name":"hydra/claimed/victoria"},
        {"id":"L4","name":"hydra/done"}]}"#;

    fn labelled(ids: &[&str]) -> String {
        serde_json::json!({"id": "t", "messages": [{"labelIds": ids}]}).to_string()
    }

    fn at() -> String {
        "2026-11-11 09:00:05.000".into()
    }

    #[test]
    fn alone_a_claim_holds_and_leaves_its_file() {
        let swarm = tempfile::tempdir().unwrap();
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                ("/gmail/v1/users/me/threads/t1?", 200, labelled(&["INBOX"])),
                ("/gmail/v1/users/me/threads/t1/modify", 200, "{}".into()),
                (
                    "/gmail/v1/users/me/threads/t1?",
                    200,
                    labelled(&["INBOX", "L1", "L2"]),
                ),
            ],
            5,
        );
        let got = claim(
            &ep,
            "a",
            "jordan",
            "team@x.com",
            Some(swarm.path()),
            vec![mailed("t1")],
            &at,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(got.kept, vec![mailed("t1")]);
        assert!(got.lost.is_empty() && got.theirs.is_empty());
        assert_eq!(read_claims(swarm.path(), "t1").len(), 1);
        let calls: Vec<Seen> = seen.try_iter().collect();
        let modify = calls.iter().find(|c| c.url.ends_with("/modify")).unwrap();
        let body: serde_json::Value = serde_json::from_str(&modify.body).unwrap();
        assert_eq!(body["addLabelIds"], serde_json::json!(["L1", "L2"]));
    }

    use crate::gmail::testing::Seen;

    #[test]
    fn a_rival_claiming_first_keeps_it_and_mine_comes_off() {
        let swarm = tempfile::tempdir().unwrap();
        write_claim(
            swarm.path(),
            "t2",
            "victoria",
            "2026-11-11 09:00:01.000",
            "MAIL-T2",
            "S",
            "team@x.com",
        )
        .unwrap();
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                // Before: her label hasn't landed, but her claim is seconds old: hers.
                ("/gmail/v1/users/me/threads/t2?", 200, labelled(&["INBOX"])),
            ],
            2,
        );
        let now = || "2026-11-11 09:00:03.000".to_string();
        let got = claim(
            &ep,
            "a",
            "jordan",
            "team@x.com",
            Some(swarm.path()),
            vec![mailed("t2")],
            &now,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(got.theirs, vec![(mailed("t2"), "victoria".to_string())]);
        assert_eq!(
            seen.try_iter()
                .filter(|c| c.url.ends_with("/modify"))
                .count(),
            0
        );

        // At the same moment: both labels land; her claim was first, so she keeps it.
        let swarm = tempfile::tempdir().unwrap();
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                ("/gmail/v1/users/me/threads/t3?", 200, labelled(&["INBOX"])),
                ("/gmail/v1/users/me/threads/t3/modify", 200, "{}".into()),
                ("/gmail/v1/users/me/threads/t3/modify", 200, "{}".into()),
                (
                    "/gmail/v1/users/me/threads/t3?",
                    200,
                    labelled(&["L1", "L2", "L3"]),
                ),
            ],
            6,
        );
        let sw = swarm.path().to_path_buf();
        let now = move || {
            // Her claim file syncs in during the settle, a second before mine.
            write_claim(
                &sw,
                "t3",
                "victoria",
                "2026-11-11 09:00:04.000",
                "MAIL-T3",
                "S",
                "team@x.com",
            )
            .unwrap();
            "2026-11-11 09:00:05.000".to_string()
        };
        let got = claim(
            &ep,
            "a",
            "jordan",
            "team@x.com",
            Some(swarm.path()),
            vec![mailed("t3")],
            &now,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(got.lost, vec![(mailed("t3"), "victoria".to_string())]);
        assert!(got.kept.is_empty());
        let modifies: Vec<serde_json::Value> = seen
            .try_iter()
            .filter(|c| c.url.ends_with("/modify"))
            .map(|c| serde_json::from_str(&c.body).unwrap())
            .collect();
        assert_eq!(modifies.len(), 2);
        assert_eq!(modifies[1]["removeLabelIds"], serde_json::json!(["L2"]));
        // Only her claim is left.
        let left = read_claims(swarm.path(), "t3");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].who, "victoria");
    }

    #[test]
    fn a_thread_already_mine_is_kept_without_claiming_again() {
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                (
                    "/gmail/v1/users/me/threads/t4?",
                    200,
                    labelled(&["L1", "L2"]),
                ),
            ],
            2,
        );
        let got = claim(
            &ep,
            "a",
            "jordan",
            "team@x.com",
            None,
            vec![mailed("t4")],
            &at,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(got.kept, vec![mailed("t4")]);
        assert_eq!(
            seen.try_iter()
                .filter(|c| c.url.ends_with("/modify"))
                .count(),
            0
        );
    }

    #[test]
    fn a_finished_thread_is_done_and_a_stopped_one_freed() {
        let swarm = tempfile::tempdir().unwrap();
        write_claim(
            swarm.path(),
            "t1",
            "jordan",
            "2026-11-11 09:00:01.000",
            "MAIL-T1",
            "S",
            "team@x.com",
        )
        .unwrap();
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                ("/gmail/v1/users/me/threads/t1/modify", 200, "{}".into()),
            ],
            2,
        );
        release(&ep, "a", "jordan", Some(swarm.path()), "t1", "t1", true).unwrap();
        let body: serde_json::Value = serde_json::from_str(
            &seen
                .try_iter()
                .find(|c| c.url.ends_with("/modify"))
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(body["addLabelIds"], serde_json::json!(["L4"]));
        assert_eq!(body["removeLabelIds"], serde_json::json!(["L1", "L2"]));
        assert!(read_claims(swarm.path(), "t1").is_empty());

        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                ("/gmail/v1/users/me/threads/t1/modify", 200, "{}".into()),
            ],
            2,
        );
        release(&ep, "a", "jordan", None, "t1", "t1", false).unwrap();
        let body: serde_json::Value = serde_json::from_str(
            &seen
                .try_iter()
                .find(|c| c.url.ends_with("/modify"))
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(body["addLabelIds"], serde_json::json!([]));
        assert_eq!(body["removeLabelIds"], serde_json::json!(["L1", "L2"]));
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn by(who: &str, at: &str) -> Claim {
        Claim {
            who: who.into(),
            at: at.into(),
            mailbox: "team@x.com".into(),
        }
    }

    const NOW: &str = "2026-11-11 09:30:00.000";

    fn from_box(who: &str, at: &str, mailbox: &str) -> Claim {
        Claim {
            who: who.into(),
            at: at.into(),
            mailbox: mailbox.into(),
        }
    }

    #[test]
    fn a_claim_from_another_mailbox_holds_while_it_is_there() {
        // A group: David reads his own copy, so his label never shows in Jordan's mailbox.
        let old = [from_box(
            "david",
            "2026-11-11 08:00:00.000",
            "david@iov42.com",
        )];
        assert_eq!(
            look("jordan", "jordan@iov42.com", &[], &old, NOW),
            Found::Theirs("david".into()),
            "an hour on, still his: nothing else here will ever say so"
        );
        // The same claim made in this very mailbox, with no label after an hour: given up.
        let same = [from_box(
            "david",
            "2026-11-11 08:00:00.000",
            "jordan@iov42.com",
        )];
        assert_eq!(
            look("jordan", "jordan@iov42.com", &[], &same, NOW),
            Found::Free
        );
        // A claim written before claims named their mailbox reads as this mailbox's.
        let unnamed = [from_box("david", "2026-11-11 08:00:00.000", "")];
        assert_eq!(
            look("jordan", "jordan@iov42.com", &[], &unnamed, NOW),
            Found::Free
        );
    }

    #[test]
    fn rivals_in_other_mailboxes_settle_like_labels() {
        let me = names(&["jordan"]);
        // Only my label shows; David claimed his copy first: his.
        let first = [
            from_box("jordan", "2026-11-11 09:00:02.000", "jordan@iov42.com"),
            from_box("david", "2026-11-11 09:00:01.000", "david@iov42.com"),
        ];
        assert_eq!(
            settle("jordan", "jordan@iov42.com", &me, &first),
            Err("david".into())
        );
        // On his machine the same files decide the same way.
        assert!(settle("david", "david@iov42.com", &names(&["david"]), &first).is_ok());
        // Mine was first: mine.
        let second = [
            from_box("jordan", "2026-11-11 09:00:01.000", "jordan@iov42.com"),
            from_box("david", "2026-11-11 09:00:02.000", "david@iov42.com"),
        ];
        assert!(settle("jordan", "jordan@iov42.com", &me, &second).is_ok());
        assert_eq!(
            settle("david", "david@iov42.com", &names(&["david"]), &second),
            Err("jordan".into())
        );
    }

    #[test]
    fn two_inboxes_racing_for_one_group_email_end_with_one_owner() {
        // Jordan's copy is thread j1, David's is d1: the same email, the same claim key.
        let key = "m-00aa11bb22cc33dd";
        let swarm = tempfile::tempdir().unwrap();
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                ("/gmail/v1/users/me/threads/j1?", 200, labelled(&["INBOX"])),
                ("/gmail/v1/users/me/threads/j1/modify", 200, "{}".into()),
                ("/gmail/v1/users/me/threads/j1/modify", 200, "{}".into()),
                // After the settle only Jordan's own label is here: David's is on his copy.
                (
                    "/gmail/v1/users/me/threads/j1?",
                    200,
                    labelled(&["INBOX", "L1", "L2"]),
                ),
            ],
            6,
        );
        let sw = swarm.path().to_path_buf();
        let now = move || {
            // David's claim on his copy syncs in during the settle, a second before Jordan's.
            write_claim(
                &sw,
                key,
                "david",
                "2026-11-11 09:00:04.000",
                "MAIL-D1",
                "API 401",
                "david@iov42.com",
            )
            .unwrap();
            "2026-11-11 09:00:05.000".to_string()
        };
        let mut email = mailed("j1");
        email.key = key.into();
        let got = claim(
            &ep,
            "a",
            "jordan",
            "jordan@iov42.com",
            Some(swarm.path()),
            vec![email.clone()],
            &now,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(got.lost, vec![(email, "david".to_string())]);
        assert!(got.kept.is_empty(), "only one of them runs it");
        // Jordan's label comes off his copy, and only David's claim is left in the swarm.
        let modifies: Vec<serde_json::Value> = seen
            .try_iter()
            .filter(|c| c.url.ends_with("/modify"))
            .map(|c| serde_json::from_str(&c.body).unwrap())
            .collect();
        assert_eq!(modifies[1]["removeLabelIds"], serde_json::json!(["L2"]));
        let left = read_claims(swarm.path(), key);
        assert_eq!(left.len(), 1);
        assert_eq!(
            (left[0].who.as_str(), left[0].mailbox.as_str()),
            ("david", "david@iov42.com")
        );
    }

    #[test]
    fn people_become_plain_slugs_and_labels() {
        assert_eq!(slug("Jordan Miller"), "jordan-miller");
        assert_eq!(slug("  Victoria  "), "victoria");
        assert_eq!(slug("O'Neil.QA"), "o-neil-qa");
        assert_eq!(label_of("jordan"), "hydra/claimed/jordan");
        assert_eq!(
            claimants(&names(&[
                "INBOX",
                "hydra/claimed",
                "hydra/claimed/jordan",
                "hydra/done"
            ])),
            ["jordan"]
        );
    }

    #[test]
    fn claims_are_written_read_and_removed_per_person() {
        let swarm = tempfile::tempdir().unwrap();
        let s = swarm.path();
        assert!(read_claims(s, "t1").is_empty());
        write_claim(
            s,
            "t1",
            "jordan",
            "2026-11-11 09:00:01.000",
            "MAIL-T1",
            "API 401",
            "team@x.com",
        )
        .unwrap();
        write_claim(
            s,
            "t1",
            "victoria",
            "2026-11-11 09:00:02.000",
            "MAIL-T1",
            "API 401",
            "team@x.com",
        )
        .unwrap();
        write_claim(
            s,
            "t2",
            "jordan",
            "2026-11-11 09:00:03.000",
            "MAIL-T2",
            "Other",
            "team@x.com",
        )
        .unwrap();
        // A sync conflict copy is not a claim.
        std::fs::write(
            s.join("claims").join("t1--jordan 2.md"),
            "at: 2026-11-11 08:00:00.000\n",
        )
        .unwrap();
        let mut got = read_claims(s, "t1");
        got.sort_by(|a, b| a.who.cmp(&b.who));
        assert_eq!(
            got,
            [
                by("jordan", "2026-11-11 09:00:01.000"),
                by("victoria", "2026-11-11 09:00:02.000")
            ]
        );
        let text = std::fs::read_to_string(claim_file(s, "t1", "jordan")).unwrap();
        assert!(
            text.contains("MAIL-T1") && text.contains("API 401"),
            "{text}"
        );
        remove_claim(s, "t1", "jordan").unwrap();
        remove_claim(s, "t1", "jordan").unwrap();
        assert_eq!(read_claims(s, "t1").len(), 1);
    }

    #[test]
    fn a_look_leaves_others_threads_alone() {
        assert_eq!(look("jordan", "team@x.com", &[], &[], NOW), Found::Free);
        assert_eq!(
            look("jordan", "team@x.com", &names(&["jordan"]), &[], NOW),
            Found::Mine
        );
        assert_eq!(
            look("jordan", "team@x.com", &names(&["victoria"]), &[], NOW),
            Found::Theirs("victoria".into())
        );
        // Their label hasn't landed yet, but their claim is a minute old.
        let fresh = [by("victoria", "2026-11-11 09:29:00.000")];
        assert_eq!(
            look("jordan", "team@x.com", &[], &fresh, NOW),
            Found::Theirs("victoria".into())
        );
        // An hour-old claim with no label was given up: the thread is free.
        let old = [by("victoria", "2026-11-11 08:30:00.000")];
        assert_eq!(look("jordan", "team@x.com", &[], &old, NOW), Found::Free);
        // My own claim file alone doesn't stop me.
        let mine = [by("jordan", "2026-11-11 09:29:00.000")];
        assert_eq!(look("jordan", "team@x.com", &[], &mine, NOW), Found::Free);
    }

    #[test]
    fn rivals_settle_on_the_earliest_claim_then_the_lowest_name() {
        assert!(settle("jordan", "team@x.com", &names(&["jordan"]), &[]).is_ok());
        let both = names(&["jordan", "victoria"]);
        let early_v = [
            by("jordan", "2026-11-11 09:00:02.000"),
            by("victoria", "2026-11-11 09:00:01.000"),
        ];
        assert_eq!(
            settle("jordan", "team@x.com", &both, &early_v),
            Err("victoria".into())
        );
        assert!(settle("victoria", "team@x.com", &both, &early_v).is_ok());
        // Same time, or neither seen: the lowest name, for everyone.
        assert!(settle("jordan", "team@x.com", &both, &[]).is_ok());
        assert_eq!(
            settle("victoria", "team@x.com", &both, &[]),
            Err("jordan".into())
        );
        // Only one claim seen: it's earlier than one not seen.
        let only_v = [by("victoria", "2026-11-11 09:00:05.000")];
        assert_eq!(
            settle("jordan", "team@x.com", &both, &only_v),
            Err("victoria".into())
        );
        // My label is gone: lost to whoever has it.
        assert_eq!(
            settle("jordan", "team@x.com", &names(&["victoria"]), &[]),
            Err("victoria".into())
        );
        assert_eq!(
            settle("jordan", "team@x.com", &[], &[]),
            Err("someone".into())
        );
    }
}
