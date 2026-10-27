//! The per-file sync decision: given what this machine last agreed on (`base`), what is on disk
//! now and what the swarm says, work out what to do with one path. Pure: no I/O.
use crate::paths::{RelPath, conflict_name};
use iroh_blobs::Hash;
use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize};

/// The file on this machine. `hash` is None when the file is absent.
pub struct Local {
    pub hash: Option<Hash>,
    /// Unix microseconds.
    pub modified: u64,
}

/// The swarm's latest word on a path. `hash` is None for a tombstone (a delete).
pub struct Remote {
    pub hash: Option<Hash>,
    /// Unix microseconds.
    pub at: u64,
    pub author: String,
    pub machine: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Held {
    ReaderEdit,
    EditedAfterDelete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Put the remote content at the path.
    Write(Hash),
    /// Delete the local file.
    RemoveFile,
    /// Writer: publish the local content (or a tombstone if the file is absent).
    Publish,
    /// Stop syncing this path here and raise attention.
    Hold(Held),
    /// The remote takes the name; the local content goes to this copy, which is then published.
    ConflictKeepRemote {
        copy_from_local: RelPath,
    },
    /// Local keeps the name and is published; the remote content goes to this copy, which is
    /// published too.
    ConflictKeepLocal {
        copy_from_remote: RelPath,
    },
    /// A local delete met a remote edit: the edit comes back.
    Restore(Hash),
}

fn timestamp(micros: u64) -> Timestamp {
    Timestamp::from_microsecond(i64::try_from(micros).unwrap_or(i64::MAX)).unwrap_or(Timestamp::MAX)
}

#[allow(clippy::too_many_arguments)]
pub fn decide(
    path: &RelPath,
    base: Option<Hash>,
    local: &Local,
    remote: Option<&Remote>,
    me: (&str, &str),
    writer: bool,
    held: Option<Held>,
    tz: &TimeZone,
) -> Action {
    if held.is_some() {
        return Action::Nothing;
    }
    let changed = local.hash != base;
    let incoming = remote.filter(|r| r.hash != base);
    let Some(remote) = incoming else {
        return match (changed, writer) {
            (false, _) => Action::Nothing,
            (true, true) => Action::Publish,
            (true, false) => Action::Hold(Held::ReaderEdit),
        };
    };
    if !changed {
        return match remote.hash {
            Some(hash) => Action::Write(hash),
            None => Action::RemoveFile,
        };
    }
    // Both sides moved on from the base.
    if local.hash == remote.hash {
        return Action::Nothing;
    }
    if !writer {
        return Action::Hold(Held::ReaderEdit);
    }
    match (local.hash, remote.hash) {
        (Some(_), None) => Action::Hold(Held::EditedAfterDelete),
        (None, Some(hash)) => Action::Restore(hash),
        // Both hold content (equal hashes and both-absent were settled above). The newer side
        // keeps the name; ties go to the higher author, then the higher machine, so every
        // machine reaches the same answer.
        _ => {
            let mine = (local.modified, me.0, me.1);
            let theirs = (remote.at, remote.author.as_str(), remote.machine.as_str());
            if theirs > mine {
                Action::ConflictKeepRemote {
                    copy_from_local: conflict_name(path, me.1, timestamp(local.modified), tz),
                }
            } else {
                Action::ConflictKeepLocal {
                    copy_from_remote: conflict_name(
                        path,
                        &remote.machine,
                        timestamp(remote.at),
                        tz,
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::safe_rel_path;

    const YEAR_US: u64 = 365 * 24 * 3600 * 1_000_000;
    const NOW: u64 = 1_761_570_300_000_000; // 27 Oct 2025, 13:05 UTC

    fn h(n: u8) -> Hash {
        Hash::new([n])
    }

    fn path() -> RelPath {
        safe_rel_path("SKILL.md").unwrap()
    }

    fn local(hash: Option<Hash>, modified: u64) -> Local {
        Local { hash, modified }
    }

    fn remote(hash: Option<Hash>, at: u64, author: &str) -> Remote {
        Remote {
            hash,
            at,
            author: author.into(),
            machine: "Laptop".into(),
        }
    }

    const ME: (&str, &str) = ("davo", "Desktop");

    fn run(
        base: Option<Hash>,
        l: &Local,
        r: Option<&Remote>,
        writer: bool,
        held: Option<Held>,
    ) -> Action {
        decide(&path(), base, l, r, ME, writer, held, &TimeZone::UTC)
    }

    #[test]
    fn an_untouched_file_takes_the_incoming_edit() {
        let r = remote(Some(h(2)), NOW, "bob");
        let a = run(Some(h(1)), &local(Some(h(1)), NOW), Some(&r), false, None);
        assert_eq!(a, Action::Write(h(2)));
    }

    #[test]
    fn an_untouched_file_follows_a_delete() {
        let r = remote(None, NOW, "bob");
        let a = run(Some(h(1)), &local(Some(h(1)), NOW), Some(&r), false, None);
        assert_eq!(a, Action::RemoveFile);
    }

    #[test]
    fn a_writers_local_edit_is_published() {
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), None, true, None);
        assert_eq!(a, Action::Publish);
    }

    #[test]
    fn a_readers_local_edit_is_held() {
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), None, false, None);
        assert_eq!(a, Action::Hold(Held::ReaderEdit));
    }

    #[test]
    fn a_readers_local_delete_is_held() {
        let a = run(Some(h(1)), &local(None, NOW), None, false, None);
        assert_eq!(a, Action::Hold(Held::ReaderEdit));
    }

    #[test]
    fn both_changed_the_newer_remote_wins_and_local_is_copied() {
        let r = remote(Some(h(3)), NOW + 5_000_000, "bob");
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), Some(&r), true, None);
        let copy = conflict_name(&path(), "Desktop", timestamp(NOW), &TimeZone::UTC);
        assert_eq!(
            a,
            Action::ConflictKeepRemote {
                copy_from_local: copy
            }
        );
        assert_eq!(
            match a {
                Action::ConflictKeepRemote { copy_from_local } =>
                    copy_from_local.as_str().to_string(),
                _ => unreachable!(),
            },
            "SKILL (conflict, Desktop, 27 Oct 13.05).md"
        );
    }

    #[test]
    fn both_changed_the_newer_local_wins_and_remote_is_copied() {
        let at = NOW - 3_600_000_000;
        let r = remote(Some(h(3)), at, "bob");
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), Some(&r), true, None);
        let copy = conflict_name(&path(), "Laptop", timestamp(at), &TimeZone::UTC);
        assert_eq!(
            a,
            Action::ConflictKeepLocal {
                copy_from_remote: copy
            }
        );
    }

    #[test]
    fn a_tie_goes_to_the_higher_author() {
        let higher = remote(Some(h(3)), NOW, "zed");
        let a = run(
            Some(h(1)),
            &local(Some(h(2)), NOW),
            Some(&higher),
            true,
            None,
        );
        assert!(matches!(a, Action::ConflictKeepRemote { .. }));
        let lower = remote(Some(h(3)), NOW, "alice");
        let a = run(
            Some(h(1)),
            &local(Some(h(2)), NOW),
            Some(&lower),
            true,
            None,
        );
        assert!(matches!(a, Action::ConflictKeepLocal { .. }));
    }

    #[test]
    fn delete_meeting_an_edit_keeps_the_edit() {
        // Remote deleted, local edited: held for the person to decide.
        let tomb = remote(None, NOW + 1, "bob");
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), Some(&tomb), true, None);
        assert_eq!(a, Action::Hold(Held::EditedAfterDelete));
        // Local deleted, remote edited: the edit comes back.
        let edit = remote(Some(h(3)), NOW + 1, "bob");
        let a = run(Some(h(1)), &local(None, NOW), Some(&edit), true, None);
        assert_eq!(a, Action::Restore(h(3)));
    }

    #[test]
    fn the_same_change_on_both_sides_is_not_a_conflict() {
        let r = remote(Some(h(2)), NOW + 1, "bob");
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), Some(&r), true, None);
        assert_eq!(a, Action::Nothing);
        // Both deleted.
        let tomb = remote(None, NOW, "bob");
        let a = run(Some(h(1)), &local(None, NOW), Some(&tomb), true, None);
        assert_eq!(a, Action::Nothing);
    }

    #[test]
    fn a_held_path_is_left_alone() {
        let r = remote(Some(h(2)), NOW, "bob");
        let a = run(
            Some(h(1)),
            &local(Some(h(3)), NOW),
            Some(&r),
            true,
            Some(Held::ReaderEdit),
        );
        assert_eq!(a, Action::Nothing);
    }

    #[test]
    fn nothing_changed_means_nothing_to_do() {
        let a = run(Some(h(1)), &local(Some(h(1)), NOW), None, true, None);
        assert_eq!(a, Action::Nothing);
        // An absent file with no base is not a change.
        let a = run(None, &local(None, NOW), None, true, None);
        assert_eq!(a, Action::Nothing);
    }

    #[test]
    fn a_clock_far_ahead_wins_the_name_but_the_other_edit_is_kept() {
        let r = remote(Some(h(3)), NOW + 10 * YEAR_US, "bob");
        let a = run(Some(h(1)), &local(Some(h(2)), NOW), Some(&r), true, None);
        let copy = conflict_name(&path(), "Desktop", timestamp(NOW), &TimeZone::UTC);
        assert_eq!(
            a,
            Action::ConflictKeepRemote {
                copy_from_local: copy
            }
        );
    }
}
