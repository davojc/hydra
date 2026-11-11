//! The folder layout decision: given the folders this machine last agreed on, the folders on disk
//! now and what the swarm's folder records say, work out what to publish, create, move and
//! remove. Pure: no I/O, no clock.
//!
//! Folder records are mapped through the moves in force the way file entries are (`Moves::map`),
//! so a rename is one record, never an edit of everything under it, and work done under an old
//! name before the rename reached its author lands in the renamed folder.
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::node::{FolderRecord, FolderState, FolderVersion, MoveRecord, MoveVersion};
use crate::paths::{RelPath, safe_rel_path};

/// `p` folded for comparing folder paths the way `folder.rs` compares file paths, so `Notes` and
/// `notes` are one folder.
fn fold(p: &str) -> String {
    p.to_lowercase()
}

/// What of `p` lies below `base`, comparing whole names case folded: `Some("")` when `p` is
/// `base`, None when it isn't in it. Name by name, so `notes` never matches `notesy`, and a
/// name whose folded form is a different length can't shift the split.
fn below<'p>(p: &'p str, base: &str) -> Option<&'p str> {
    let mut rest = p;
    for seg in base.split('/') {
        if rest.is_empty() {
            return None;
        }
        let (head, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if fold(head) != fold(seg) {
            return None;
        }
        rest = tail;
    }
    Some(rest)
}

/// Whether `p` is strictly inside `base`.
fn inside(p: &RelPath, base: &RelPath) -> bool {
    below(p.as_str(), base.as_str()).is_some_and(|rest| !rest.is_empty())
}

/// `p` with `from`, if `p` is it or is in it, swapped for `to`.
fn rebase(p: &RelPath, from: &RelPath, to: &RelPath) -> Option<RelPath> {
    match below(p.as_str(), from.as_str())? {
        "" => Some(to.clone()),
        rest => safe_rel_path(&format!("{}/{rest}", to.as_str())),
    }
}

fn parent(p: &RelPath) -> Option<RelPath> {
    p.as_str()
        .rsplit_once('/')
        .and_then(|(dir, _)| safe_rel_path(dir))
}

fn name(p: &RelPath) -> &str {
    p.as_str().rsplit_once('/').map_or(p.as_str(), |(_, n)| n)
}

fn depth(p: &RelPath) -> usize {
    p.as_str().matches('/').count()
}

/// The folders `p` is in, nearest first.
fn ancestors(p: &RelPath) -> impl Iterator<Item = RelPath> {
    std::iter::successors(parent(p), parent)
}

/// `p` after a sequence of folder renames, applied in order.
fn after(renames: &[(RelPath, RelPath)], p: &RelPath) -> RelPath {
    renames
        .iter()
        .fold(p.clone(), |p, (from, to)| rebase(&p, from, to).unwrap_or(p))
}

/// One move in force: the folder at `from`, and everything in it, goes to `to`.
#[derive(Debug, Clone)]
struct Move {
    from: RelPath,
    /// The source as its author wrote it.
    written_from: RelPath,
    to: RelPath,
    /// The target as its author wrote it, before older moves were applied to the folder it is
    /// in. The `Present` written beside a move, and its author's own copy of the folder, are
    /// found by it and follow the move to where it actually landed.
    written_to: RelPath,
    at: u64,
    author: String,
    machine: String,
}

/// A move as its author wrote it in the move log.
struct Written {
    from: RelPath,
    to: RelPath,
    at: u64,
    author: String,
    machine: String,
}

/// The moves in force, oldest first, after loops are dropped.
#[derive(Debug, Clone, Default)]
pub struct Moves {
    list: Vec<Move>,
}

impl Moves {
    /// Built from the move log, where no move is ever overwritten, so every move counts, not only
    /// the newest record at a path. A later rename of the same folder then moves on from where
    /// the earlier one put it, so both sides' work ends in the later name, and a folder made
    /// later under the old name doesn't undo the rename.
    pub fn from_moves(log: &[MoveVersion]) -> Moves {
        let mut written: Vec<Written> = log
            .iter()
            .filter_map(|v| {
                Some(Written {
                    from: safe_rel_path(&v.record.from)?,
                    to: safe_rel_path(&v.record.to)?,
                    at: v.record.at,
                    author: v.author.clone(),
                    machine: v.machine.clone(),
                })
            })
            .collect();
        written.sort_by(|a, b| {
            (a.at, &a.author, &a.machine, &a.from).cmp(&(b.at, &b.author, &b.machine, &b.from))
        });
        // Each round drops one more move, so this ends within `written.len()` rounds.
        let mut dropped: BTreeMap<usize, Option<usize>> = BTreeMap::new();
        loop {
            let i = match build(&written, &dropped) {
                Ok(list) => return Moves { list },
                Err(i) => i,
            };
            // Move `i` would put a folder inside itself because of an older move: the later move
            // wins, so the newest older move whose absence lets `i` stand is undone. If there is
            // none, `i` puts a folder inside itself on its own and is dropped.
            let older = (0..i)
                .rev()
                .filter(|j| !dropped.contains_key(j))
                .find(|&j| {
                    let mut trial = dropped.clone();
                    trial.insert(j, Some(i));
                    build(&written, &trial).map_or_else(|k| k > i, |_| true)
                });
            match older {
                Some(j) => dropped.insert(j, Some(i)),
                None => dropped.insert(i, None),
            };
        }
    }

    /// `p` after every move older than `at`, whatever the entry's time: where a folder its
    /// author named before `at` had got to by then.
    fn before(&self, at: u64, p: &RelPath) -> RelPath {
        self.list
            .iter()
            .filter(|m| m.at < at)
            .fold(p.clone(), |p, m| rebase(&p, &m.from, &m.to).unwrap_or(p))
    }

    /// Where an entry for `p` made at `at` belongs now.
    pub fn map(&self, p: &RelPath, at: u64) -> RelPath {
        self.place(p, at, false).0
    }

    /// Where something at `p` as of `at` is now, and the moves that took it there, in order. With
    /// `strict`, a move at exactly `at` counts as already seen, as for a folder this machine
    /// agreed at `at`.
    fn place(&self, p: &RelPath, at: u64, strict: bool) -> (RelPath, Vec<&Move>) {
        // The move's own folder, recorded at the move's time under the name its author wrote,
        // starts where the move landed, and only later moves act on it.
        let start = self
            .list
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.at == at && below(p.as_str(), m.written_to.as_str()).is_some());
        let (mut p, skip) = match start {
            Some((i, m)) => (
                rebase(p, &m.written_to, &m.to).unwrap_or_else(|| p.clone()),
                i + 1,
            ),
            None => (p.clone(), 0),
        };
        let mut chain = Vec::new();
        for m in self.list.iter().skip(skip) {
            let applies = if strict { m.at > at } else { m.at >= at };
            if !applies {
                continue;
            }
            if let Some(q) = rebase(&p, &m.from, &m.to) {
                p = q;
                chain.push(m);
            }
        }
        (p, chain)
    }
}

/// The moves in force with `dropped` left out (each mapped to the move whose turn undoes it), or
/// the index of the first move that would put a folder inside itself.
fn build(
    written: &[Written],
    dropped: &BTreeMap<usize, Option<usize>>,
) -> Result<Vec<Move>, usize> {
    let mut list: Vec<Move> = Vec::new();
    for (i, w) in written.iter().enumerate() {
        // A move this one beat is undone first, at this one's time, so its folder, and anything
        // put in it under its new name, go back where they were.
        for (&j, _) in dropped.iter().filter(|(_, by)| **by == Some(i)) {
            let d = &written[j];
            let from = apply_all(&list, &d.to);
            let to = target(&list, &d.from);
            if from != to && !inside(&to, &from) {
                list.push(Move {
                    from,
                    to,
                    written_from: d.to.clone(),
                    written_to: d.from.clone(),
                    at: w.at,
                    author: d.author.clone(),
                    machine: d.machine.clone(),
                });
            }
        }
        if dropped.contains_key(&i) {
            continue;
        }
        let from = apply_all(&list, &w.from);
        let to = target(&list, &w.to);
        // Once older moves are applied, a move onto itself moves nothing.
        if from == to {
            continue;
        }
        if inside(&to, &from) {
            return Err(i);
        }
        list.push(Move {
            from,
            to,
            written_from: w.from.clone(),
            written_to: w.to.clone(),
            at: w.at,
            author: w.author.clone(),
            machine: w.machine.clone(),
        });
    }
    Ok(list)
}

/// Where a folder named in a move record is now, after every older move whatever its time: its
/// author named it as it was then.
fn apply_all(list: &[Move], p: &RelPath) -> RelPath {
    list.iter()
        .fold(p.clone(), |p, m| rebase(&p, &m.from, &m.to).unwrap_or(p))
}

/// Where a move's target lands. The folder it is going into follows older moves, its own name
/// doesn't: `notes/bob → notes/robert` after `notes → docs` lands at `docs/robert`, while
/// `docs → notes` after `notes → docs` is a rename back, not a move onto itself.
fn target(list: &[Move], to: &RelPath) -> RelPath {
    match parent(to) {
        Some(dir) => safe_rel_path(&format!("{}/{}", apply_all(list, &dir).as_str(), name(to)))
            .unwrap_or_else(|| to.clone()),
        None => to.clone(),
    }
}

/// A folder in the swarm folder as scanned now.
pub struct DiskFolder {
    /// Nothing in it but empty folders.
    pub empty: bool,
    /// The names of the files directly in it that sync.
    pub synced_files: BTreeSet<String>,
    /// The names of the folders directly in it.
    pub subfolders: BTreeSet<String>,
}

/// A folder in the folder index: in the layout here, and the time of the newest record about
/// it this machine has acted on.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct KnownFolder {
    pub applied_at: u64,
    /// The newest record this machine has told its person about for this folder, so a notice
    /// fires once whatever order records arrive in. Older indexes have none.
    #[serde(default)]
    pub notified_at: u64,
}

pub struct LayoutInput<'a> {
    /// This machine's author id, to tell its own records from others'.
    pub me: &'a str,
    /// The folder index: what this machine and the swarm last agreed on.
    pub known: &'a BTreeMap<RelPath, KnownFolder>,
    /// Folders scanned now, those under a dot folder left out.
    pub disk: &'a BTreeMap<RelPath, DiskFolder>,
    pub records: &'a BTreeMap<RelPath, Vec<FolderVersion>>,
    /// The move log (`SwarmNode::moves`).
    pub moves: &'a [MoveVersion],
    /// Synced file paths after `Moves::map`, tombstones left out.
    pub live_files: &'a BTreeSet<RelPath>,
    pub writer: bool,
    pub now: u64,
    /// The newest file entry under a folder, for catch-up records (Ruling A).
    pub newest_file_at: &'a dyn Fn(&RelPath) -> Option<u64>,
    /// False when the dot rule or an owner's `.swarmignore` takes a folder out of the layout
    /// (`SyncRules::folder_may_sync`). Such a folder is never created, published or moved into.
    pub syncs_folder: &'a dyn Fn(&RelPath) -> bool,
    /// This machine's folder index has never been saved (an upgrade, or a lost index). Only then
    /// does an unknown folder on disk follow the swarm's moves and deletes; otherwise it is the
    /// person's own new folder.
    pub first_pass: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LayoutPlan {
    pub publish: Vec<(RelPath, FolderRecord)>,
    /// Moves to add to the move log, one per rename made here.
    pub publish_moves: Vec<MoveRecord>,
    /// Renames made here: the index and disk names follow.
    pub detected_moves: Vec<(RelPath, RelPath)>,
    /// Incoming renames to carry out on disk, in order; each source is the folder's path once
    /// the moves before it are done.
    pub disk_moves: Vec<(RelPath, RelPath)>,
    /// Parents first.
    pub create: Vec<RelPath>,
    /// Deepest first, after the file pass.
    pub remove_if_empty: Vec<RelPath>,
    /// Drop from the index.
    pub forget: Vec<RelPath>,
    /// Write to the index, at the folder's path once `disk_moves` are done.
    pub know: Vec<(RelPath, KnownFolder)>,
    pub notices: Vec<LayoutNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutNotice {
    RenameLost {
        mine: RelPath,
        kept: RelPath,
        by_machine: String,
    },
    DeleteKeptForWork {
        path: RelPath,
        deleted_by_machine: String,
    },
    MyDeleteKept {
        path: RelPath,
    },
    /// On the machine that deleted `path`: someone else's rename kept it, at `to`.
    MyDeleteLostToRename {
        path: RelPath,
        to: RelPath,
        by_machine: String,
    },
    /// On the machine that renamed a folder to `to`: someone else deleted it under its old name,
    /// after the rename, so the delete didn't reach it.
    MyRenameSurvivedDelete {
        to: RelPath,
        by_machine: String,
    },
}

/// The newest record at a folder path, once moves are applied.
struct Word<'r> {
    path: RelPath,
    state: &'r FolderState,
    at: u64,
    author: &'r str,
    machine: &'r str,
}

impl Word<'_> {
    /// Newest by `(at, author, machine)`, as file entries are. On a full tie a `Present` wins, so
    /// a folder an undone move put back isn't hidden by the move's own record.
    fn rank(&self) -> (u64, &str, &str, bool) {
        (self.at, self.author, self.machine, self.present())
    }

    fn present(&self) -> bool {
        matches!(self.state, FolderState::Present)
    }

    fn deleted(&self) -> bool {
        matches!(self.state, FolderState::Deleted)
    }
}

/// Per folded path, the newest record there.
fn newest_words<'r>(
    records: &'r BTreeMap<RelPath, Vec<FolderVersion>>,
    moves: &Moves,
) -> HashMap<String, Word<'r>> {
    let mut out: HashMap<String, Word<'r>> = HashMap::new();
    for (p, versions) in records {
        for v in versions {
            // `Present` and `Deleted` follow moves like file entries. A `MovedTo` says its own
            // path is gone.
            let path = match v.record.state {
                FolderState::MovedTo { .. } => p.clone(),
                _ => moves.map(p, v.record.at),
            };
            let w = Word {
                path,
                state: &v.record.state,
                at: v.record.at,
                author: &v.author,
                machine: &v.machine,
            };
            match out.entry(fold(w.path.as_str())) {
                Entry::Occupied(mut e) => {
                    if w.rank() > e.get().rank() {
                        e.insert(w);
                    }
                }
                Entry::Vacant(e) => {
                    e.insert(w);
                }
            }
        }
    }
    out
}

/// Folded paths with something live under them: a synced file, or a folder whose newest record
/// is `Present`.
fn live_below(words: &HashMap<String, Word>, live_files: &BTreeSet<RelPath>) -> HashSet<String> {
    let present = words.values().filter(|w| w.present()).map(|w| &w.path);
    live_files
        .iter()
        .chain(present)
        .flat_map(ancestors)
        .map(|a| fold(a.as_str()))
        .collect()
}

/// The order to carry out incoming renames in. A folder leaving the folder it is in goes first,
/// deepest first, before that folder moves and would take it along; then everything else,
/// parents first, from wherever the earlier renames left it. Crossed moves (A back out of B
/// while B goes into A) need the first; a parent and a child renamed together need the second.
fn order_disk_moves(placed: &BTreeMap<&RelPath, RelPath>) -> Vec<(RelPath, RelPath)> {
    let rides = |k: &RelPath, t: &RelPath| {
        ancestors(k)
            .find_map(|a| placed.get(&a))
            .is_some_and(|ta| inside(t, ta))
    };
    let mut leaving: Vec<(&RelPath, &RelPath)> = placed
        .iter()
        .filter(|(k, t)| !rides(k, t))
        .map(|(k, t)| (*k, t))
        .collect();
    leaving.sort_by_key(|(k, _)| std::cmp::Reverse(depth(k)));
    let mut all: Vec<(&RelPath, &RelPath)> = placed.iter().map(|(k, t)| (*k, t)).collect();
    all.sort_by_key(|(k, _)| depth(k));
    let mut ops: Vec<(RelPath, RelPath)> = Vec::new();
    for (k, t) in leaving.into_iter().chain(all) {
        let current = after(&ops, k);
        if current != *t {
            ops.push((current, t.clone()));
        }
    }
    ops
}

/// Whether an unknown folder on disk would join the layout as new: it may sync, it is empty or
/// holds a synced file at any depth, and no folder it is in that isn't in the layout fails
/// that too (so the empty folders in a `node_modules` stay out).
fn counts(
    d: &RelPath,
    disk: &BTreeMap<RelPath, DiskFolder>,
    in_layout: &dyn Fn(&RelPath) -> bool,
) -> bool {
    let Some(here) = disk.get(d) else {
        return true;
    };
    let holds_synced = || {
        disk.iter()
            .any(|(p, f)| !f.synced_files.is_empty() && below(p.as_str(), d.as_str()).is_some())
    };
    (here.empty || holds_synced())
        && ancestors(d).all(|a| in_layout(&a) || counts(&a, disk, in_layout))
}

/// What this machine's records and folders say should change. See the module doc and spec §4–5.
pub fn plan(input: &LayoutInput) -> LayoutPlan {
    let LayoutInput {
        me,
        known,
        disk,
        records,
        moves: log,
        live_files,
        writer,
        now,
        newest_file_at,
        syncs_folder,
        first_pass,
    } = *input;
    let moves = Moves::from_moves(log);
    let words = newest_words(records, &moves);
    let live = live_below(&words, live_files);
    let word = |p: &RelPath| words.get(&fold(p.as_str()));
    let live_under = |p: &RelPath| live.contains(&fold(p.as_str()));
    // A record written on someone else's behalf takes the time of the newest work that implied
    // it, never now, so it can't look newer than a real rename or delete.
    let beneath = |p: &RelPath| {
        let records = words
            .values()
            .filter(|w| w.present() && inside(&w.path, p))
            .map(|w| w.at);
        newest_file_at(p).into_iter().chain(records).max()
    };
    let is_ignored = |p: &RelPath| !syncs_folder(p) || ancestors(p).any(|a| !syncs_folder(&a));
    // My rename to `p` lost to `winner` only when both started from the same folder: a rename
    // of the folder after mine, by someone who had seen it, is an ordinary later rename.
    let lost_to = |p: &RelPath, winner: &Move| {
        log.iter().any(|v| {
            v.author == me
                && v.record.at < winner.at
                && fold(&v.record.to) == fold(p.as_str())
                && fold(moves.before(v.record.at, &winner.written_from).as_str())
                    == fold(&v.record.from)
        })
    };

    let mut publish: Vec<(RelPath, FolderRecord)> = Vec::new();
    let mut publish_moves: Vec<MoveRecord> = Vec::new();
    let mut detected_moves: Vec<(RelPath, RelPath)> = Vec::new();
    let mut create: BTreeSet<RelPath> = BTreeSet::new();
    let mut remove_if_empty: Vec<RelPath> = Vec::new();
    let mut forget: BTreeSet<RelPath> = BTreeSet::new();
    let mut know: BTreeMap<RelPath, u64> = BTreeMap::new();
    // `notified_at` for index entries written here, where it isn't the old entry's.
    let mut told: BTreeMap<RelPath, u64> = BTreeMap::new();
    let mut notices: Vec<LayoutNotice> = Vec::new();
    // Folded paths a known folder answers for, so records there don't also create it.
    let mut handled: HashSet<String> = known.keys().map(|k| fold(k.as_str())).collect();

    // Known folders still on disk: where each belongs now.
    let mut placed: BTreeMap<&RelPath, (RelPath, Vec<&Move>)> = BTreeMap::new();
    let mut missing: Vec<&RelPath> = Vec::new();
    for (k, kf) in known {
        if !disk.contains_key(k) {
            missing.push(k);
        } else if is_ignored(k) {
            // An owner's `.swarmignore` took it out of the layout: it leaves without a record,
            // and nothing is removed anywhere (spec §4.1).
            forget.insert(k.clone());
        } else {
            placed.insert(k, moves.place(k, kf.applied_at, true));
        }
    }
    let folded =
        |names: &BTreeSet<String>| -> BTreeSet<String> { names.iter().map(|n| fold(n)).collect() };
    // The folder's last-synced contents, by name: the synced files directly in it, and the
    // known folders directly in it.
    let last_files = |k: &RelPath| -> BTreeSet<String> {
        live_files
            .iter()
            .filter_map(|f| below(f.as_str(), k.as_str()))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .map(fold)
            .collect()
    };
    let last_subfolders = |k: &RelPath| -> BTreeSet<String> {
        known
            .keys()
            .filter_map(|c| below(c.as_str(), k.as_str()))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .map(fold)
            .collect()
    };
    let same = |k: &RelPath, d: &RelPath| {
        let now_there = &disk[d];
        folded(&now_there.synced_files) == last_files(k)
            && folded(&now_there.subfolders) == last_subfolders(k)
    };

    // On a first pass, a folder on disk the index doesn't have, at a path a move in force took
    // away, with no synced file there newer than the move, is the old folder: it follows the move
    // rather than bringing the old name back for everyone. One that is this writer's own rename,
    // found this scan, is left to the rename check. Once the index exists, such a folder is one
    // the person made, and §5.2 says a newer folder at the old name stands.
    let mut followers: BTreeMap<&RelPath, (RelPath, Vec<&Move>)> = BTreeMap::new();
    for d in disk.keys().filter(|_| first_pass) {
        if known.contains_key(d) || is_ignored(d) || (writer && missing.iter().any(|k| same(k, d)))
        {
            continue;
        }
        let (t, chain) = moves.place(d, newest_file_at(d).unwrap_or(0), false);
        if t != *d {
            followers.insert(d, (t, chain));
        }
    }
    // A known folder whose move would take it somewhere the rules keep out of the layout isn't
    // moved; it leaves the layout here, as an ignored folder does.
    placed.retain(|k, (t, _)| {
        let keep = !is_ignored(t);
        if !keep {
            forget.insert((*k).clone());
        }
        keep
    });
    followers.retain(|_, (t, _)| !is_ignored(t));
    let targets: BTreeMap<&RelPath, RelPath> = placed
        .iter()
        .chain(&followers)
        .map(|(k, (t, _))| (*k, t.clone()))
        .collect();
    let disk_moves = order_disk_moves(&targets);
    handled.extend(targets.values().map(|t| fold(t.as_str())));
    let incoming: HashSet<String> = targets
        .iter()
        .filter(|(k, t)| **k != *t)
        .map(|(_, t)| fold(t.as_str()))
        .collect();

    for (k, (t, chain)) in &placed {
        let applied = known[*k].applied_at;
        let notified = known[*k].notified_at;
        let mut applied_now = applied;
        if t != *k {
            forget.insert((*k).clone());
            told.insert(t.clone(), notified);
            applied_now = applied.max(chain.last().map_or(0, |m| m.at));
            // Any move along the way may be the one that beat mine; a later rename of the
            // winner's folder doesn't change who that was.
            if let Some(m) = chain.iter().find(|m| m.author != me && lost_to(k, m)) {
                notices.push(LayoutNotice::RenameLost {
                    mine: (*k).clone(),
                    kept: t.clone(),
                    by_machine: m.machine.clone(),
                });
            }
        }
        match word(t) {
            // Live only through its files, with no record: one is written for it (Ruling A).
            None if writer => {
                let at = beneath(t).unwrap_or(now);
                publish.push((t.clone(), present_at(at)));
                applied_now = applied_now.max(at);
            }
            Some(w) if w.at > applied && w.present() => applied_now = applied_now.max(w.at),
            Some(w) if w.at > applied && w.deleted() => {
                if !live_under(t) {
                    remove_if_empty.push(t.clone());
                    forget.insert((*k).clone());
                    continue;
                }
                // Something live under it that the delete didn't cover keeps it.
                applied_now = applied_now.max(w.at);
                notices.push(if w.author == me {
                    LayoutNotice::MyDeleteKept { path: t.clone() }
                } else {
                    LayoutNotice::DeleteKeptForWork {
                        path: t.clone(),
                        deleted_by_machine: w.machine.to_string(),
                    }
                });
            }
            _ => {}
        }
        // Someone else deleted the folder under its old name, before or after this machine renamed
        // it, and the renamed folder survived: the renamer is told, once.
        let late_delete = log
            .iter()
            .filter(|m| m.author == me)
            .filter(|m| {
                safe_rel_path(&m.record.to).is_some_and(|to| {
                    fold(moves.map(&to, m.record.at).as_str()) == fold(t.as_str())
                })
            })
            .flat_map(|m| {
                records
                    .iter()
                    .filter(move |(p, _)| fold(p.as_str()) == fold(&m.record.from))
                    .flat_map(|(_, versions)| versions)
                    .filter(move |v| {
                        matches!(v.record.state, FolderState::Deleted) && v.author != me
                    })
            })
            .filter(|v| v.record.at > notified)
            .max_by_key(|v| v.record.at);
        let mut notified_now = notified;
        if let Some(v) = late_delete {
            notices.push(LayoutNotice::MyRenameSurvivedDelete {
                to: t.clone(),
                by_machine: v.machine.clone(),
            });
            notified_now = v.record.at;
            told.insert(t.clone(), notified_now);
        }
        if t != *k || applied_now != applied || notified_now != notified {
            know.insert(t.clone(), applied_now);
        }
    }

    // Known folders gone from disk.
    missing.sort_by_key(|k| depth(k));
    let mut claimed: HashSet<RelPath> = HashSet::new();
    let mut renames: Vec<(RelPath, RelPath)> = Vec::new();
    if writer {
        let candidates: Vec<&RelPath> = disk
            .keys()
            .filter(|d| {
                !known.contains_key(*d)
                    && !followers.contains_key(*d)
                    && !is_ignored(d)
                    && !incoming.contains(&fold(d.as_str()))
            })
            .collect();
        let mut carried_along: HashSet<&RelPath> = HashSet::new();
        for k in &missing {
            forget.insert((*k).clone());
            // Carried along by a folder it is in being renamed.
            let carried = renames.iter().find_map(|(from, to)| rebase(k, from, to));
            if let Some(c) = carried.filter(|c| disk.contains_key(c)) {
                carried_along.insert(k);
                told.insert(c.clone(), known[*k].notified_at);
                know.insert(c.clone(), known[*k].applied_at);
                claimed.insert(c);
                continue;
            }
            let matching: Vec<&RelPath> = candidates
                .iter()
                .filter(|d| !claimed.contains(**d) && same(k, d))
                .copied()
                .collect();
            // A rename only when the match is one to one; otherwise a delete plus a create.
            let rename = match matching.as_slice() {
                [d] if !missing
                    .iter()
                    .any(|o| o != k && !carried_along.contains(o) && same(o, d)) =>
                {
                    Some(*d)
                }
                _ => None,
            };
            match rename {
                Some(d) => {
                    publish.push((
                        (*k).clone(),
                        FolderRecord {
                            state: FolderState::MovedTo {
                                to: d.as_str().to_string(),
                            },
                            at: now,
                        },
                    ));
                    publish.push((d.clone(), present_at(now)));
                    publish_moves.push(MoveRecord {
                        from: k.as_str().to_string(),
                        to: d.as_str().to_string(),
                        at: now,
                    });
                    detected_moves.push(((*k).clone(), d.clone()));
                    know.insert(d.clone(), now);
                    // What its person has been told stays with the folder under its new name.
                    told.insert(d.clone(), known[*k].notified_at);
                    claimed.insert(d.clone());
                    renames.push(((*k).clone(), d.clone()));
                }
                None => publish.push((
                    (*k).clone(),
                    FolderRecord {
                        state: FolderState::Deleted,
                        at: now,
                    },
                )),
            }
        }
    } else {
        // A reader's own delete isn't shared. The folder stays in the index, holding the time
        // it was agreed, so only a newer record brings it back.
        for k in &missing {
            let applied = known[*k].applied_at;
            let (t, _) = moves.place(k, applied, true);
            handled.insert(fold(t.as_str()));
            match word(&t) {
                Some(w) if w.at > applied && w.present() && !is_ignored(&t) => {
                    create.insert(t.clone());
                    if t != **k {
                        forget.insert((*k).clone());
                        told.insert(t.clone(), known[*k].notified_at);
                    }
                    know.insert(t, w.at);
                }
                Some(w) if w.at > applied && w.deleted() => {
                    forget.insert((*k).clone());
                }
                _ => {}
            }
        }
    }

    // Folders on disk the index doesn't have.
    let in_layout = |p: &RelPath| known.contains_key(p) || claimed.contains(p);
    let mut joined: HashSet<String> = HashSet::new();
    for d in disk.keys() {
        if in_layout(d) || is_ignored(d) {
            continue;
        }
        if let Some((t, chain)) = followers.get(d) {
            let at = word(t)
                .map_or(0, |w| w.at)
                .max(chain.last().map_or(0, |m| m.at));
            joined.insert(fold(t.as_str()));
            know.insert(t.clone(), at);
            continue;
        }
        let f = after(&disk_moves, d);
        if handled.contains(&fold(f.as_str())) || !joined.insert(fold(f.as_str())) {
            continue;
        }
        // On a first pass, a folder deleted elsewhere that an older hydra left here, emptied of
        // synced files, is that deleted folder: it goes, rather than coming back for everyone
        // with a record newer than the delete.
        if first_pass
            && let Some(w) = word(&f)
            && w.deleted()
            && !live_under(&f)
            && newest_file_at(&f).is_none_or(|at| at <= w.at)
        {
            remove_if_empty.push(f);
            continue;
        }
        match word(&f) {
            Some(w) if w.present() => {
                know.insert(f, w.at);
            }
            Some(w) if w.deleted() && live_under(&f) => {
                if w.author == me {
                    notices.push(LayoutNotice::MyDeleteKept { path: f.clone() });
                }
                know.insert(f, w.at);
            }
            // Made here as the path to synced files, or there before folders were recorded: a
            // catch-up record (Ruling A).
            w if live_under(&f) => {
                if writer {
                    let at = beneath(&f).unwrap_or(now);
                    publish.push((f.clone(), present_at(at)));
                    know.insert(f, at);
                } else {
                    know.insert(f, w.map_or(0, |w| w.at));
                }
            }
            _ if writer && counts(d, disk, &in_layout) => {
                publish.push((f.clone(), present_at(now)));
                know.insert(f, now);
            }
            _ => {}
        }
    }

    // Folders the swarm has that aren't here yet.
    let on_disk: HashSet<String> = disk
        .keys()
        .map(|p| fold(after(&disk_moves, p).as_str()))
        .collect();
    for w in words.values().filter(|w| w.present()) {
        // A rename found in this scan isn't among `moves` yet.
        let p = after(&renames, &w.path);
        let folded = fold(p.as_str());
        if on_disk.contains(&folded)
            || handled.contains(&folded)
            || joined.contains(&folded)
            || is_ignored(&p)
        {
            continue;
        }
        // This machine deleted the folder under an old name, and someone else's rename kept it.
        for m in log.iter().filter(|m| m.author != me) {
            let lands_here = safe_rel_path(&m.record.to)
                .is_some_and(|to| fold(moves.map(&to, m.record.at).as_str()) == folded);
            let deleted_by_me = records
                .iter()
                .filter(|(q, _)| fold(q.as_str()) == fold(&m.record.from))
                .flat_map(|(_, versions)| versions)
                .any(|v| v.author == me && matches!(v.record.state, FolderState::Deleted));
            if lands_here
                && deleted_by_me
                && let Some(from) = safe_rel_path(&m.record.from)
            {
                notices.push(LayoutNotice::MyDeleteLostToRename {
                    path: from,
                    to: p.clone(),
                    by_machine: m.machine.clone(),
                });
            }
        }
        know.insert(p.clone(), w.at);
        create.insert(p);
    }

    publish.sort_by(|a, b| a.0.cmp(&b.0));
    remove_if_empty.sort_by(|a, b| depth(b).cmp(&depth(a)).then_with(|| a.cmp(b)));
    LayoutPlan {
        publish,
        publish_moves,
        detected_moves,
        disk_moves,
        create: create.into_iter().collect(),
        remove_if_empty,
        forget: forget
            .into_iter()
            .filter(|p| !know.contains_key(p))
            .collect(),
        know: know
            .into_iter()
            .map(|(p, applied_at)| {
                let notified_at = told
                    .get(&p)
                    .copied()
                    .or_else(|| known.get(&p).map(|k| k.notified_at))
                    .unwrap_or(0);
                (
                    p,
                    KnownFolder {
                        applied_at,
                        notified_at,
                    },
                )
            })
            .collect(),
        notices,
    }
}

fn present_at(at: u64) -> FolderRecord {
    FolderRecord {
        state: FolderState::Present,
        at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{MoveRecord, MoveVersion};

    const NOW: u64 = 1_000_000;

    fn rel(s: &str) -> RelPath {
        safe_rel_path(s).unwrap()
    }

    fn machine(author: &str) -> String {
        format!("{author}'s laptop")
    }

    fn version(state: FolderState, at: u64, author: &str) -> FolderVersion {
        FolderVersion {
            record: FolderRecord { state, at },
            author: author.into(),
            machine: machine(author),
        }
    }

    fn present(at: u64, author: &str) -> FolderVersion {
        version(FolderState::Present, at, author)
    }

    fn deleted(at: u64, author: &str) -> FolderVersion {
        version(FolderState::Deleted, at, author)
    }

    fn moved(to: &str, at: u64, author: &str) -> FolderVersion {
        version(FolderState::MovedTo { to: to.into() }, at, author)
    }

    fn move_by(from: &str, to: &str, at: u64, author: &str) -> MoveVersion {
        MoveVersion {
            record: MoveRecord {
                from: from.into(),
                to: to.into(),
                at,
            },
            author: author.into(),
            machine: machine(author),
        }
    }

    fn rec(state: FolderState, at: u64) -> FolderRecord {
        FolderRecord { state, at }
    }

    fn names(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// A folder with nothing in it but empty folders.
    fn empty(subfolders: &[&str]) -> DiskFolder {
        DiskFolder {
            empty: true,
            synced_files: BTreeSet::new(),
            subfolders: names(subfolders),
        }
    }

    /// A folder holding files: `synced` are the ones that sync; it also holds others that don't
    /// when `synced` is empty.
    fn holding(synced: &[&str], subfolders: &[&str]) -> DiskFolder {
        DiskFolder {
            empty: false,
            synced_files: names(synced),
            subfolders: names(subfolders),
        }
    }

    struct World {
        me: String,
        writer: bool,
        known: BTreeMap<RelPath, KnownFolder>,
        disk: BTreeMap<RelPath, DiskFolder>,
        records: BTreeMap<RelPath, Vec<FolderVersion>>,
        moves: Vec<MoveVersion>,
        files: BTreeMap<RelPath, u64>,
        ignored: Vec<RelPath>,
        first_pass: bool,
    }

    impl World {
        fn writer(me: &str) -> World {
            World {
                me: me.into(),
                writer: true,
                known: BTreeMap::new(),
                disk: BTreeMap::new(),
                records: BTreeMap::new(),
                moves: Vec::new(),
                files: BTreeMap::new(),
                ignored: Vec::new(),
                first_pass: false,
            }
        }

        fn reader(me: &str) -> World {
            World {
                writer: false,
                ..World::writer(me)
            }
        }

        fn known(self, p: &str, applied_at: u64) -> World {
            self.known_told(p, applied_at, 0)
        }

        fn known_told(mut self, p: &str, applied_at: u64, notified_at: u64) -> World {
            self.known.insert(
                rel(p),
                KnownFolder {
                    applied_at,
                    notified_at,
                },
            );
            self
        }

        /// An owner's `.swarmignore` line (or the dot rule) takes `p` out of the layout.
        fn ignore(mut self, p: &str) -> World {
            self.ignored.push(rel(p));
            self
        }

        /// This machine's folder index has never been saved: an upgrade, or a lost index.
        fn first_pass(mut self) -> World {
            self.first_pass = true;
            self
        }

        fn disk(mut self, p: &str, d: DiskFolder) -> World {
            self.disk.insert(rel(p), d);
            self
        }

        fn record(mut self, p: &str, v: FolderVersion) -> World {
            let list = self.records.entry(rel(p)).or_default();
            list.push(v);
            list.sort_by(|a, b| (a.record.at, &a.author).cmp(&(b.record.at, &b.author)));
            self
        }

        /// A rename as its author writes it: `MovedTo` at the old path, and the move in the log.
        fn rename(self, from: &str, to: &str, at: u64, author: &str) -> World {
            let mut w = self.record(from, moved(to, at, author));
            w.moves.push(move_by(from, to, at, author));
            w
        }

        /// A live synced file, with the time of its newest entry.
        fn file(mut self, p: &str, at: u64) -> World {
            self.files.insert(rel(p), at);
            self
        }

        fn plan(&self) -> LayoutPlan {
            let live: BTreeSet<RelPath> = self.files.keys().cloned().collect();
            let newest = |folder: &RelPath| {
                let prefix = format!("{}/", folder.as_str());
                self.files
                    .iter()
                    .filter(|(p, _)| p.as_str().starts_with(&prefix))
                    .map(|(_, at)| *at)
                    .max()
            };
            let syncs = |folder: &RelPath| {
                !self.ignored.iter().any(|i| {
                    folder == i || folder.as_str().starts_with(&format!("{}/", i.as_str()))
                })
            };
            plan(&LayoutInput {
                me: &self.me,
                known: &self.known,
                disk: &self.disk,
                records: &self.records,
                moves: &self.moves,
                live_files: &live,
                writer: self.writer,
                now: NOW,
                newest_file_at: &newest,
                syncs_folder: &syncs,
                first_pass: self.first_pass,
            })
        }
    }

    fn known_paths(p: &LayoutPlan) -> Vec<&str> {
        p.know.iter().map(|(p, _)| p.as_str()).collect()
    }

    fn paths(v: &[RelPath]) -> Vec<&str> {
        v.iter().map(|p| p.as_str()).collect()
    }

    fn pairs(v: &[(RelPath, RelPath)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }

    fn published(p: &LayoutPlan) -> Vec<(&str, FolderRecord)> {
        p.publish
            .iter()
            .map(|(p, r)| (p.as_str(), r.clone()))
            .collect()
    }

    #[test]
    fn an_empty_new_folder_is_published() {
        let p = World::writer("me").disk("drafts", empty(&[])).plan();
        assert_eq!(
            published(&p),
            vec![("drafts", rec(FolderState::Present, NOW))]
        );
        assert_eq!(known_paths(&p), vec!["drafts"]);
    }

    #[test]
    fn a_folder_holding_only_files_that_dont_sync_never_joins() {
        let p = World::writer("me")
            .disk("node_modules", holding(&[], &["x"]))
            // Empty, but inside a folder that doesn't count, so it doesn't count either.
            .disk("node_modules/x", empty(&[]))
            .plan();
        assert_eq!(p, LayoutPlan::default());
    }

    #[test]
    fn a_nested_empty_layout_joins_whole() {
        let p = World::writer("me")
            .disk("a", empty(&["b"]))
            .disk("a/b", empty(&["c"]))
            .disk("a/b/c", empty(&[]))
            .plan();
        assert_eq!(
            published(&p),
            vec![
                ("a", rec(FolderState::Present, NOW)),
                ("a/b", rec(FolderState::Present, NOW)),
                ("a/b/c", rec(FolderState::Present, NOW)),
            ]
        );
    }

    #[test]
    fn a_known_folder_that_gains_only_unsynced_files_stays() {
        let p = World::writer("me")
            .known("notes", 10)
            .record("notes", present(10, "x"))
            .disk("notes", holding(&[], &[]))
            .plan();
        assert_eq!(p, LayoutPlan::default());
    }

    #[test]
    fn a_rename_is_detected_by_names_even_with_an_edit_in_the_same_scan() {
        // a.md was edited in the same scan: its content changed, but the layout only compares
        // names, so the edit doesn't stop the rename being recognised.
        let p = World::writer("me")
            .known("notes", 10)
            .known("notes/sub", 10)
            .record("notes", present(10, "me"))
            .record("notes/sub", present(10, "me"))
            .file("notes/a.md", 10)
            .file("notes/b.md", 10)
            .disk("docs", holding(&["a.md", "b.md"], &["sub"]))
            .disk("docs/sub", empty(&[]))
            .plan();
        assert_eq!(
            published(&p),
            vec![
                ("docs", rec(FolderState::Present, NOW)),
                (
                    "notes",
                    rec(FolderState::MovedTo { to: "docs".into() }, NOW)
                ),
            ]
        );
        assert_eq!(pairs(&p.detected_moves), vec![("notes", "docs")]);
        assert_eq!(
            p.publish_moves,
            vec![MoveRecord {
                from: "notes".into(),
                to: "docs".into(),
                at: NOW
            }]
        );
        assert_eq!(paths(&p.forget), vec!["notes", "notes/sub"]);
        assert_eq!(known_paths(&p), vec!["docs", "docs/sub"]);
        assert!(p.remove_if_empty.is_empty() && p.create.is_empty());
    }

    #[test]
    fn two_renamed_empty_folders_fall_back_to_delete_and_create() {
        let p = World::writer("me")
            .known("a", 10)
            .known("b", 10)
            .record("a", present(10, "me"))
            .record("b", present(10, "me"))
            .disk("c", empty(&[]))
            .disk("d", empty(&[]))
            .plan();
        assert_eq!(
            published(&p),
            vec![
                ("a", rec(FolderState::Deleted, NOW)),
                ("b", rec(FolderState::Deleted, NOW)),
                ("c", rec(FolderState::Present, NOW)),
                ("d", rec(FolderState::Present, NOW)),
            ]
        );
        assert!(p.detected_moves.is_empty());
        assert_eq!(paths(&p.forget), vec!["a", "b"]);
    }

    #[test]
    fn an_incoming_rename_moves_the_folder_on_disk() {
        let p = World::writer("z")
            .known("notes", 10)
            .rename("notes", "docs", 20, "x")
            .record("docs", present(20, "x"))
            .file("docs/a.md", 5)
            .disk("notes", holding(&["a.md"], &[]))
            .plan();
        assert_eq!(pairs(&p.disk_moves), vec![("notes", "docs")]);
        assert!(p.publish.is_empty() && p.create.is_empty());
        assert_eq!(paths(&p.forget), vec!["notes"]);
        assert_eq!(
            p.know,
            vec![(
                rel("docs"),
                KnownFolder {
                    applied_at: 20,
                    notified_at: 0
                }
            )]
        );
    }

    fn two_renames(me: &str) -> World {
        World::writer(me)
            .rename("notes", "b", 10, "y")
            .rename("notes", "a", 20, "x")
            .record("b", present(10, "y"))
            .record("a", present(20, "x"))
            .file("a/n.md", 5)
    }

    #[test]
    fn the_later_of_two_renames_wins_and_the_loser_is_told() {
        let loser = two_renames("y")
            .known("b", 10)
            .disk("b", holding(&["n.md"], &[]))
            .plan();
        assert_eq!(pairs(&loser.disk_moves), vec![("b", "a")]);
        assert_eq!(
            loser.notices,
            vec![LayoutNotice::RenameLost {
                mine: rel("b"),
                kept: rel("a"),
                by_machine: machine("x"),
            }]
        );
        assert!(loser.publish.is_empty() && loser.create.is_empty());

        let winner = two_renames("x")
            .known("a", 20)
            .disk("a", holding(&["n.md"], &[]))
            .plan();
        assert_eq!(winner, LayoutPlan::default());

        // A third machine that saw neither ends at the later name in one step.
        let other = two_renames("z")
            .known("notes", 5)
            .disk("notes", holding(&["n.md"], &[]))
            .plan();
        assert_eq!(pairs(&other.disk_moves), vec![("notes", "a")]);
        assert!(other.notices.is_empty());
    }

    #[test]
    fn a_parent_and_a_child_rename_both_apply_in_either_order() {
        let orders = [
            World::writer("z")
                .rename("notes", "docs", 10, "x")
                .record("docs", present(10, "x"))
                .rename("notes/bob", "notes/robert", 20, "y")
                .record("notes/robert", present(20, "y")),
            World::writer("z")
                .rename("notes/bob", "notes/robert", 10, "y")
                .record("notes/robert", present(10, "y"))
                .rename("notes", "docs", 20, "x")
                .record("docs", present(20, "x")),
        ];
        for w in orders {
            let moves = Moves::from_moves(&w.moves);
            assert_eq!(
                moves.map(&rel("notes/bob/x.md"), 5),
                rel("docs/robert/x.md")
            );
            let p = w
                .known("notes", 5)
                .known("notes/bob", 5)
                .disk("notes", empty(&["bob"]))
                .disk("notes/bob", empty(&[]))
                .plan();
            assert_eq!(
                pairs(&p.disk_moves),
                vec![("notes", "docs"), ("docs/bob", "docs/robert")]
            );
            assert!(p.create.is_empty(), "{:?}", p.create);
        }
    }

    #[test]
    fn crossed_moves_end_without_a_loop_and_the_later_wins() {
        // x moved A into B; y, not having seen that, later moved B into A.
        let w = World::writer("x")
            .rename("A", "B/A", 10, "x")
            .record("B/A", present(10, "x"))
            .rename("B", "A/B", 20, "y")
            .record("A/B", present(20, "y"));
        let moves = Moves::from_moves(&w.moves);
        // The later move stands: B is inside A.
        assert_eq!(moves.map(&rel("B/y.md"), 5), rel("A/B/y.md"));
        // The earlier one is undone: A is back where it was, and so is what was put in it.
        assert_eq!(moves.map(&rel("A/x.md"), 5), rel("A/x.md"));
        assert_eq!(moves.map(&rel("B/A/z.md"), 15), rel("A/z.md"));

        // On x, A comes back out of B before B moves into A.
        let p = w
            .known("B", 5)
            .known("B/A", 10)
            .disk("B", holding(&[], &["A"]))
            .disk("B/A", holding(&["x.md"], &[]))
            .file("A/x.md", 5)
            .plan();
        assert_eq!(pairs(&p.disk_moves), vec![("B/A", "A"), ("B", "A/B")]);
        assert!(p.create.is_empty(), "{:?}", p.create);
    }

    #[test]
    fn map_moves_an_older_entry_and_leaves_a_newer_one() {
        let w = World::writer("z").rename("notes", "docs", 20, "x");
        let moves = Moves::from_moves(&w.moves);
        assert_eq!(moves.map(&rel("notes/a.md"), 10), rel("docs/a.md"));
        assert_eq!(moves.map(&rel("notes/a.md"), 20), rel("docs/a.md"));
        assert_eq!(moves.map(&rel("notes/a.md"), 30), rel("notes/a.md"));
        assert_eq!(moves.map(&rel("notesy/a.md"), 10), rel("notesy/a.md"));
        // Folders match the way file paths do, case folded.
        assert_eq!(moves.map(&rel("Notes/a.md"), 10), rel("docs/a.md"));
    }

    #[test]
    fn a_rename_and_a_delete_of_the_same_folder_keep_the_renamed_folder() {
        // y deleted notes before x's rename reached it, and after.
        for delete_at in [10, 30] {
            let p = World::writer("z")
                .known("notes", 5)
                .record("notes", deleted(delete_at, "y"))
                .rename("notes", "docs", 20, "x")
                .record("docs", present(20, "x"))
                .disk("notes", empty(&[]))
                .plan();
            assert_eq!(pairs(&p.disk_moves), vec![("notes", "docs")], "{delete_at}");
            assert!(p.remove_if_empty.is_empty(), "{delete_at}");
            assert_eq!(known_paths(&p), vec!["docs"]);
        }
    }

    #[test]
    fn a_delete_removes_an_empty_folder_deepest_first() {
        let p = World::writer("z")
            .known("a", 5)
            .known("a/b", 5)
            .record("a", present(5, "x"))
            .record("a/b", present(5, "x"))
            .record("a", deleted(10, "y"))
            .record("a/b", deleted(10, "y"))
            .disk("a", empty(&["b"]))
            .disk("a/b", empty(&[]))
            .plan();
        assert_eq!(paths(&p.remove_if_empty), vec!["a/b", "a"]);
        assert_eq!(paths(&p.forget), vec!["a", "a/b"]);
        assert!(p.publish.is_empty() && p.notices.is_empty());
    }

    #[test]
    fn a_delete_keeps_a_folder_with_an_unseen_file_or_subfolder_and_tells_both() {
        let deleted_notes = |me: &str| {
            World::writer(me)
                .record("notes", present(5, "x"))
                .record("notes", deleted(10, "y"))
        };
        let kept = DeleteKeptForWork {
            path: rel("notes"),
            deleted_by_machine: machine("y"),
        };

        // An unseen file.
        let p = deleted_notes("z")
            .known("notes", 5)
            .file("notes/new.md", 12)
            .disk("notes", holding(&["new.md"], &[]))
            .plan();
        assert!(p.remove_if_empty.is_empty() && p.forget.is_empty());
        assert_eq!(p.notices, vec![kept.clone()]);

        // An unseen subfolder, not yet on this machine.
        let p = deleted_notes("z")
            .known("notes", 5)
            .record("notes/sub", present(12, "x"))
            .disk("notes", empty(&[]))
            .plan();
        assert!(p.remove_if_empty.is_empty() && p.forget.is_empty());
        assert_eq!(paths(&p.create), vec!["notes/sub"]);
        assert_eq!(p.notices, vec![kept]);

        // On the machine that deleted it, once the file has brought it back.
        let p = deleted_notes("y")
            .file("notes/new.md", 12)
            .disk("notes", holding(&["new.md"], &[]))
            .plan();
        assert_eq!(
            p.notices,
            vec![LayoutNotice::MyDeleteKept { path: rel("notes") }]
        );
        assert!(p.publish.is_empty());
        assert_eq!(known_paths(&p), vec!["notes"]);
    }

    use LayoutNotice::DeleteKeptForWork;

    #[test]
    fn a_catch_up_record_takes_the_newest_file_time() {
        let with_files = |w: World| {
            w.file("notes/a.md", 300)
                .file("notes/b.md", 500)
                .disk("notes", holding(&["a.md", "b.md"], &[]))
        };
        let catch_up = vec![("notes", rec(FolderState::Present, 500))];
        // A known folder with no record.
        let p = with_files(World::writer("me").known("notes", 0)).plan();
        assert_eq!(published(&p), catch_up);
        // A folder found on upgrade, before anything was known.
        let p = with_files(World::writer("me")).plan();
        assert_eq!(published(&p), catch_up);
        // A known folder with nothing under it has no file time to take.
        let p = World::writer("me")
            .known("empty", 0)
            .disk("empty", empty(&[]))
            .plan();
        assert_eq!(
            published(&p),
            vec![("empty", rec(FolderState::Present, NOW))]
        );
    }

    #[test]
    fn a_reader_publishes_nothing() {
        let w = World::reader("r")
            .known("gone", 5)
            .record("gone", present(5, "x"))
            .known("notes", 5)
            .record("notes", present(5, "x"))
            .file("notes/a.md", 5)
            .disk("renamed", holding(&["a.md"], &[]))
            .disk("mine", empty(&[]));
        let p = w.plan();
        assert!(p.publish.is_empty() && p.detected_moves.is_empty());
        // What the reader deleted isn't brought back by records it has already seen.
        assert!(p.create.is_empty(), "{:?}", p.create);
        assert!(p.forget.is_empty() && p.know.is_empty());

        // A record newer than what it agreed brings it back.
        let p = w.record("gone", present(20, "y")).plan();
        assert_eq!(paths(&p.create), vec!["gone"]);
        assert!(p.publish.is_empty());
    }

    #[test]
    fn folders_differing_only_by_case_are_one_folder() {
        let p = World::writer("z")
            .record("Drafts", present(10, "x"))
            .record("drafts", present(10, "y"))
            .plan();
        assert_eq!(paths(&p.create), vec!["drafts"]);
        assert!(p.disk_moves.is_empty() && p.detected_moves.is_empty());
        assert!(p.publish.is_empty());
    }

    #[test]
    fn a_rename_differing_only_by_case_is_a_rename() {
        let p = World::writer("me")
            .known("Notes", 5)
            .record("Notes", present(5, "me"))
            .file("Notes/a.md", 5)
            .disk("notes", holding(&["a.md"], &[]))
            .plan();
        assert_eq!(
            published(&p),
            vec![
                (
                    "Notes",
                    rec(FolderState::MovedTo { to: "notes".into() }, NOW)
                ),
                ("notes", rec(FolderState::Present, NOW)),
            ]
        );
        assert_eq!(pairs(&p.detected_moves), vec![("Notes", "notes")]);
    }

    #[test]
    fn a_folder_an_owner_ignores_is_forgotten_without_a_record() {
        let p = World::writer("me")
            .known("build", 5)
            .record("build", present(5, "x"))
            .ignore("build")
            .disk("build", empty(&[]))
            .plan();
        assert_eq!(paths(&p.forget), vec!["build"]);
        assert!(p.publish.is_empty() && p.remove_if_empty.is_empty() && p.create.is_empty());
    }
    #[test]
    fn a_rename_back_to_the_old_name_is_a_move_not_a_move_onto_itself() {
        let w = World::writer("z")
            .rename("notes", "docs", 10, "x")
            .rename("docs", "notes", 20, "y");
        let moves = Moves::from_moves(&w.moves);
        assert_eq!(moves.map(&rel("notes/a.md"), 5), rel("notes/a.md"));
        assert_eq!(moves.map(&rel("docs/b.md"), 15), rel("notes/b.md"));
    }

    #[test]
    fn one_new_folder_matching_two_gone_ones_is_not_a_rename() {
        let p = World::writer("me")
            .known("a", 10)
            .known("b", 10)
            .disk("c", empty(&[]))
            .plan();
        assert!(p.detected_moves.is_empty());
        assert_eq!(
            published(&p),
            vec![
                ("a", rec(FolderState::Deleted, NOW)),
                ("b", rec(FolderState::Deleted, NOW)),
                ("c", rec(FolderState::Present, NOW)),
            ]
        );
    }
    #[test]
    fn a_move_outlives_its_author_making_a_new_folder_at_the_old_name() {
        // x renamed A to B, then made a new A: x's Present at A replaced its MovedTo there.
        let mut w = World::writer("z")
            .record("A", present(30, "x"))
            .record("B", present(20, "x"));
        w.moves.push(move_by("A", "B", 20, "x"));
        let moves = Moves::from_moves(&w.moves);
        assert_eq!(moves.map(&rel("A/old.md"), 10), rel("B/old.md"));
        assert_eq!(moves.map(&rel("A/new.md"), 40), rel("A/new.md"));
    }

    #[test]
    fn a_folder_found_at_a_moved_away_name_follows_the_move() {
        // On upgrade: no index, A still on disk with its old files, and A has moved to B.
        let p = World::writer("z")
            .first_pass()
            .rename("A", "B", 20, "x")
            .record("B", present(20, "x"))
            .file("B/old.md", 10)
            .disk("A", holding(&["old.md"], &[]))
            .plan();
        assert_eq!(pairs(&p.disk_moves), vec![("A", "B")]);
        assert!(p.publish.is_empty() && p.create.is_empty());
        assert_eq!(known_paths(&p), vec!["B"]);

        // A file there newer than the move means A was made again after it: it stays.
        let p = World::writer("z")
            .first_pass()
            .rename("A", "B", 20, "x")
            .record("B", present(20, "x"))
            .file("A/new.md", 30)
            .disk("A", holding(&["new.md"], &[]))
            .disk("B", empty(&[]))
            .plan();
        assert!(p.disk_moves.is_empty());
    }

    #[test]
    fn a_delete_that_loses_to_a_rename_tells_the_deleter() {
        // y deleted notes; x renamed it to docs, before the delete or after.
        for delete_at in [10, 30] {
            let p = World::writer("y")
                .record("notes", deleted(delete_at, "y"))
                .rename("notes", "docs", 20, "x")
                .record("docs", present(20, "x"))
                .plan();
            assert_eq!(paths(&p.create), vec!["docs"], "{delete_at}");
            assert_eq!(
                p.notices,
                vec![LayoutNotice::MyDeleteLostToRename {
                    path: rel("notes"),
                    to: rel("docs"),
                    by_machine: machine("x"),
                }],
                "{delete_at}"
            );
        }
    }

    #[test]
    fn a_rename_that_survives_a_delete_tells_the_renamer_once() {
        // y deleted notes before x's rename and after it: x is told either way, once.
        for delete_at in [10, 30] {
            let w = || {
                World::writer("x")
                    .rename("notes", "docs", 20, "x")
                    .record("docs", present(20, "x"))
                    .record("notes", deleted(delete_at, "y"))
                    .disk("docs", empty(&[]))
            };
            let p = w().known("docs", 20).plan();
            assert_eq!(
                p.notices,
                vec![LayoutNotice::MyRenameSurvivedDelete {
                    to: rel("docs"),
                    by_machine: machine("y"),
                }],
                "{delete_at}"
            );
            assert_eq!(
                p.know,
                vec![(
                    rel("docs"),
                    KnownFolder {
                        applied_at: 20,
                        notified_at: delete_at
                    }
                )]
            );
            assert!(p.remove_if_empty.is_empty());
            assert_eq!(
                w().known_told("docs", 20, delete_at).plan(),
                LayoutPlan::default(),
                "{delete_at}"
            );
        }
    }

    #[test]
    fn a_later_rename_of_my_renamed_folder_is_not_a_lost_rename() {
        // y renamed notes to b; x, having seen that, later renamed b to c.
        let p = World::writer("y")
            .rename("notes", "b", 10, "y")
            .record("b", present(10, "y"))
            .rename("b", "c", 30, "x")
            .record("c", present(30, "x"))
            .known("b", 10)
            .disk("b", empty(&[]))
            .plan();
        assert_eq!(pairs(&p.disk_moves), vec![("b", "c")]);
        assert!(p.notices.is_empty(), "{:?}", p.notices);
    }

    #[test]
    fn a_new_folder_at_a_moved_away_name_is_a_create_once_the_index_exists() {
        let p = World::writer("z")
            .rename("A", "B", 20, "x")
            .record("B", present(20, "x"))
            .known("B", 20)
            .disk("B", empty(&[]))
            .disk("A", empty(&[]))
            .plan();
        assert!(p.disk_moves.is_empty(), "{:?}", p.disk_moves);
        assert_eq!(published(&p), vec![("A", rec(FolderState::Present, NOW))]);
    }

    #[test]
    fn on_upgrade_a_folder_deleted_elsewhere_is_removed_not_brought_back() {
        let p = World::writer("z")
            .first_pass()
            .record("notes", deleted(10, "x"))
            .disk("notes", empty(&[]))
            .plan();
        assert!(p.publish.is_empty(), "{:?}", p.publish);
        assert_eq!(paths(&p.remove_if_empty), vec!["notes"]);
        assert!(p.know.is_empty());

        // Once the index exists, a folder made at that name is the user's create.
        let p = World::writer("z")
            .record("notes", deleted(10, "x"))
            .disk("notes", empty(&[]))
            .plan();
        assert_eq!(
            published(&p),
            vec![("notes", rec(FolderState::Present, NOW))]
        );
    }

    #[test]
    fn a_folder_an_owner_ignores_is_never_created() {
        let p = World::reader("r")
            .ignore("build")
            .record("build", present(10, "x"))
            .record("build/out", present(10, "x"))
            .plan();
        assert!(p.create.is_empty(), "{:?}", p.create);
        assert!(p.know.is_empty());
    }
    #[test]
    fn a_lost_rename_is_told_even_when_the_winner_was_renamed_again() {
        // y and x renamed notes at once and x won; later w renamed x's folder on.
        let p = World::writer("y")
            .rename("notes", "b", 10, "y")
            .record("b", present(10, "y"))
            .rename("notes", "a", 20, "x")
            .record("a", present(20, "x"))
            .rename("a", "c", 30, "w")
            .record("c", present(30, "w"))
            .known("b", 10)
            .disk("b", empty(&[]))
            .plan();
        assert_eq!(pairs(&p.disk_moves), vec![("b", "c")]);
        assert_eq!(
            p.notices,
            vec![LayoutNotice::RenameLost {
                mine: rel("b"),
                kept: rel("c"),
                by_machine: machine("x"),
            }]
        );
    }

    #[test]
    fn renaming_a_folder_here_keeps_what_its_person_was_told() {
        // x renamed notes to docs, y deleted notes after, and x was told. Now x renames docs to e.
        let w = || {
            World::writer("x")
                .rename("notes", "docs", 20, "x")
                .record("notes", deleted(30, "y"))
        };
        let p = w()
            .record("docs", present(20, "x"))
            .known_told("docs", 20, 30)
            .disk("e", empty(&[]))
            .plan();
        assert_eq!(pairs(&p.detected_moves), vec![("docs", "e")]);
        assert_eq!(
            p.know,
            vec![(
                rel("e"),
                KnownFolder {
                    applied_at: NOW,
                    notified_at: 30
                }
            )]
        );

        // The next pass, with the rename in the swarm: nothing is told again.
        let p = w()
            .rename("docs", "e", NOW, "x")
            .record("e", present(NOW, "x"))
            .known_told("e", NOW, 30)
            .disk("e", empty(&[]))
            .plan();
        assert!(p.notices.is_empty(), "{:?}", p.notices);
    }
}
