//! One person per email (email triage spec §5): a thread is claimed with Gmail labels, which
//! everyone reading the mailbox sees at once and so decide, and a file in the team's swarm,
//! which shows hydraui who has what and covers the moments before a label lands. Two people
//! claiming at once both see both labels after the settle, and every machine picks the same
//! winner. Pure but for the swarm's files.

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

/// A claim in the swarm: who, and when (a log time, `2026-11-11 09:00:01.123`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub who: String,
    pub at: String,
}

/// `<swarm folder>/claims/<thread>--<who>.md`: one file per person, so two claims never make a
/// sync conflict, and `.md`, which every swarm syncs.
pub fn claim_file(swarm: &Path, thread: &str, who: &str) -> PathBuf {
    swarm.join("claims").join(format!("{thread}--{who}.md"))
}

/// Writes `who`'s claim on `thread`: who, when, the run's ticket and the subject. Never the
/// email itself.
pub fn write_claim(
    swarm: &Path,
    thread: &str,
    who: &str,
    at: &str,
    ticket: &str,
    subject: &str,
) -> std::io::Result<()> {
    let path = claim_file(swarm, thread, who);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        format!(
            "# Claimed: {subject}\n\nclaimed by: {who}\nat: {at}\nrun: {ticket}\nthread: {thread}\n"
        ),
    )
}

/// The claims on `thread` in the swarm's folder. Sync's conflict copies aren't claims.
pub fn read_claims(swarm: &Path, thread: &str) -> Vec<Claim> {
    let prefix = format!("{thread}--");
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
            let at = text
                .lines()
                .find_map(|l| l.strip_prefix("at:"))
                .map(|a| a.trim().to_string())?;
            Some(Claim { who, at })
        })
        .collect()
}

/// Removes `who`'s claim on `thread`, if there is one.
pub fn remove_claim(swarm: &Path, thread: &str, who: &str) -> std::io::Result<()> {
    match std::fs::remove_file(claim_file(swarm, thread, who)) {
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

/// Whether `me` may claim a thread with these label claimants and swarm claims, at `now`.
/// Gmail decides: anyone else's label is theirs. A swarm claim of someone else's holds it only
/// while recent, the moments before their label lands.
pub fn look(me: &str, labelled: &[String], swarm: &[Claim], now: &str) -> Found {
    if let Some(other) = labelled.iter().find(|w| *w != me) {
        return Found::Theirs(other.clone());
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
pub fn settle(me: &str, labelled: &[String], swarm: &[Claim]) -> Result<(), String> {
    if !labelled.iter().any(|w| w == me) {
        return Err(labelled
            .first()
            .cloned()
            .unwrap_or_else(|| "someone".into()));
    }
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
pub fn claim(
    ep: &Endpoints,
    access: &str,
    me: &str,
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
    let swarm_claims = |thread: &str| swarm.map(|s| read_claims(s, thread)).unwrap_or_default();
    let mut out = Claimed::default();
    let mut trying = Vec::new();
    for m in items {
        let on = names(gmail::thread_label_ids(ep, access, &m.thread)?, &known);
        match look(me, &claimants(&on), &swarm_claims(&m.thread), &now()) {
            Found::Mine => out.kept.push(m),
            Found::Theirs(who) => out.theirs.push((m, who)),
            Found::Free => {
                if let Some(s) = swarm {
                    write_claim(s, &m.thread, me, &now(), &m.ticket, &m.subject)
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
        match settle(me, &claimants(&on), &swarm_claims(&m.thread)) {
            Ok(()) => out.kept.push(m),
            Err(who) => {
                gmail::modify_thread(ep, access, &m.thread, &[], std::slice::from_ref(&mine))?;
                if let Some(s) = swarm {
                    remove_claim(s, &m.thread, me).map_err(|e| e.to_string())?;
                }
                out.lost.push((m, who));
            }
        }
    }
    Ok(out)
}

/// Ends `me`'s claim on `thread` once its run ends: `done` swaps the claim labels for
/// `hydra/done`; otherwise they come off, so the thread is free for someone else. Its claim
/// file goes either way.
pub fn release(
    ep: &Endpoints,
    access: &str,
    me: &str,
    swarm: Option<&Path>,
    thread: &str,
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
        remove_claim(s, thread, me).map_err(|e| e.to_string())?;
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
            )
            .unwrap();
            "2026-11-11 09:00:05.000".to_string()
        };
        let got = claim(
            &ep,
            "a",
            "jordan",
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
        )
        .unwrap();
        let (ep, seen) = fake(
            vec![
                ("/gmail/v1/users/me/labels", 200, LABELS.into()),
                ("/gmail/v1/users/me/threads/t1/modify", 200, "{}".into()),
            ],
            2,
        );
        release(&ep, "a", "jordan", Some(swarm.path()), "t1", true).unwrap();
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
        release(&ep, "a", "jordan", None, "t1", false).unwrap();
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
        }
    }

    const NOW: &str = "2026-11-11 09:30:00.000";

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
        )
        .unwrap();
        write_claim(
            s,
            "t1",
            "victoria",
            "2026-11-11 09:00:02.000",
            "MAIL-T1",
            "API 401",
        )
        .unwrap();
        write_claim(
            s,
            "t2",
            "jordan",
            "2026-11-11 09:00:03.000",
            "MAIL-T2",
            "Other",
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
        assert_eq!(look("jordan", &[], &[], NOW), Found::Free);
        assert_eq!(look("jordan", &names(&["jordan"]), &[], NOW), Found::Mine);
        assert_eq!(
            look("jordan", &names(&["victoria"]), &[], NOW),
            Found::Theirs("victoria".into())
        );
        // Their label hasn't landed yet, but their claim is a minute old.
        let fresh = [by("victoria", "2026-11-11 09:29:00.000")];
        assert_eq!(
            look("jordan", &[], &fresh, NOW),
            Found::Theirs("victoria".into())
        );
        // An hour-old claim with no label was given up: the thread is free.
        let old = [by("victoria", "2026-11-11 08:30:00.000")];
        assert_eq!(look("jordan", &[], &old, NOW), Found::Free);
        // My own claim file alone doesn't stop me.
        let mine = [by("jordan", "2026-11-11 09:29:00.000")];
        assert_eq!(look("jordan", &[], &mine, NOW), Found::Free);
    }

    #[test]
    fn rivals_settle_on_the_earliest_claim_then_the_lowest_name() {
        assert!(settle("jordan", &names(&["jordan"]), &[]).is_ok());
        let both = names(&["jordan", "victoria"]);
        let early_v = [
            by("jordan", "2026-11-11 09:00:02.000"),
            by("victoria", "2026-11-11 09:00:01.000"),
        ];
        assert_eq!(settle("jordan", &both, &early_v), Err("victoria".into()));
        assert!(settle("victoria", &both, &early_v).is_ok());
        // Same time, or neither seen: the lowest name, for everyone.
        assert!(settle("jordan", &both, &[]).is_ok());
        assert_eq!(settle("victoria", &both, &[]), Err("jordan".into()));
        // Only one claim seen: it's earlier than one not seen.
        let only_v = [by("victoria", "2026-11-11 09:00:05.000")];
        assert_eq!(settle("jordan", &both, &only_v), Err("victoria".into()));
        // My label is gone: lost to whoever has it.
        assert_eq!(
            settle("jordan", &names(&["victoria"]), &[]),
            Err("victoria".into())
        );
        assert_eq!(settle("jordan", &[], &[]), Err("someone".into()));
    }
}
