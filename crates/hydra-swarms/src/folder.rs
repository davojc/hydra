//! A swarm's folder on this machine, kept in step with the swarm document: a scan of the folder,
//! an index of the version each path last synced as, `decide`'s actions applied to disk and
//! document, conflict copies, held files, the attention list, and a debounced watcher.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use bytes::Bytes;
use iroh_blobs::Hash;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::decide::{Action, Held, Local, Remote, decide};
use crate::ids::to_hex;
use crate::layout::{self, DiskFolder, KnownFolder, LayoutInput, LayoutNotice, Moves};
use crate::node::{FileHistory, MoveVersion, ParentRecord, SwarmNode, now_micros};
use crate::paths::{
    MAX_FILE_BYTES, RelPath, allowed, conflict_name, conflict_original, is_conflict_copy,
    numbered_copy, rel_from_disk, safe_rel_path,
};
use crate::roster::{MAX_IGNORE_BYTES, Member, Role, Roster};
use crate::rules::{SWARMIGNORE, SyncRules};

/// Ends the name of every file hydra writes before renaming it into place. Never a synced type,
/// and skipped by the scan whatever the type list says.
const TMP_SUFFIX: &str = ".hydra-tmp";
/// Quiet time after the last change before the watcher reports.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// The gap between the two size reads that show a changed file has stopped growing.
const SETTLE: Duration = Duration::from_millis(100);
/// How often an idle watcher checks that someone is still listening.
const IDLE: Duration = Duration::from_secs(1);
const INDEX_FILE: &str = "index.json";
/// The key the folder index is kept under in `index.json`, beside the file index's paths. No
/// path can start with `/`, so it never meets a file's key, and an older hydra reading the file
/// skips it as it skips any key that isn't a safe path.
const FOLDERS_KEY: &str = "/folders";

/// The folder index as kept under `FOLDERS_KEY`. An older hydra reads every value in the file as
/// a file entry before it skips keys that aren't paths, so the folders sit one level down, under
/// a name that is no field of a file entry. At the top, a folder named `base` or `seen` would be
/// read as that field, fail, and have the older hydra call the whole index damaged.
#[derive(Serialize, Deserialize)]
struct FolderIndex {
    known: BTreeMap<String, KnownFolder>,
}
/// Ends the name a folder is given for a moment while its name changes only by case, on a disk
/// that won't take that change directly. The scan treats it as any folder: if hydra stops part
/// way, the folder is seen as renamed to this name, never as its files deleted.
const RECASE_SUFFIX: &str = ".hydra-recase";
/// How many earlier versions a parent record lists.
const MAX_ANCESTORS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// A conflict copy is in the folder, whichever machine made it.
    Conflict {
        path: RelPath,
        copy: RelPath,
    },
    EditedAfterDelete {
        path: RelPath,
    },
    /// The swarm no longer counts the only version it had; the file stays here.
    NoLongerShared {
        path: RelPath,
    },
    ReaderEdit {
        path: RelPath,
    },
    /// An allowed type over `MAX_FILE_BYTES`, which stays on this machine.
    TooBig {
        path: RelPath,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderEvent {
    /// The attention list changed; this is all of it.
    Attention(Vec<Attention>),
    NewConflict {
        path: RelPath,
        copy: RelPath,
    },
    Held(Attention),
    /// A local delete met a remote edit, so the file is back.
    Restored(RelPath),
    /// The newest change applied in this pass. `at` is unix microseconds.
    LastChange {
        machine: String,
        at: u64,
    },
    /// One path couldn't be brought in step; the others carried on.
    Error(String),
    /// The text an owner saved in `.swarmignore` on this machine (empty when they emptied or
    /// deleted it), for the service to sign into the roster as `Change::SetIgnore`, with
    /// `replaces`, the hex hash of the text it was made on top of. It travels through the roster
    /// rather than as a file entry so that only an owner's version counts.
    PublishIgnore {
        text: String,
        replaces: Option<String>,
    },
    /// This machine renamed a folder to `mine`, and `machine`'s rename of it at about the same
    /// time won, so it is at `kept`, with everything from both sides in it.
    RenameLost {
        mine: RelPath,
        kept: RelPath,
        machine: String,
    },
    /// `machine` deleted `path`, but it holds work added here that the delete didn't cover, so
    /// it was kept.
    DeleteKeptForWork {
        path: RelPath,
        machine: String,
    },
    /// This machine deleted `path`, but it holds work someone else added, so it was kept.
    MyDeleteKept {
        path: RelPath,
    },
    /// This machine deleted `path`, but `machine` had renamed it to `to`, so it was kept there.
    MyDeleteLostToRename {
        path: RelPath,
        to: RelPath,
        machine: String,
    },
    /// This machine renamed a folder to `to`, and `machine` deleted it under its old name; the
    /// delete didn't reach the renamed folder.
    MyRenameSurvivedDelete {
        to: RelPath,
        machine: String,
    },
}

/// A file's size and modification time when it was last hashed, so an unchanged file isn't read
/// again.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
struct Seen {
    size: u64,
    /// Unix microseconds.
    modified: u64,
    hash: Hash,
}

/// Whose entry a version taken in from the swarm was, and when it was made.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
struct Source {
    author: String,
    machine: String,
    /// Unix microseconds.
    at: u64,
}

impl Source {
    fn of(r: &Remote) -> Source {
        Source {
            author: r.author.clone(),
            machine: r.machine.clone(),
            at: r.at,
        }
    }
}

/// What this machine knows of one path.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
struct Entry {
    /// The version this folder and the swarm last agreed on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<Hash>,
    /// Whose entry `base` was, when it was taken in from the swarm rather than made here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    held: Option<Held>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen: Option<Seen>,
    /// `.swarmignore` only: the text this machine last handed out for the roster, until the
    /// roster moves on to another, so a newer owner's change made without having seen it can be
    /// told from one made on top of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published: Option<Published>,
}

impl Entry {
    /// Whether anything about the path is worth keeping in the index.
    fn worth_keeping(&self) -> bool {
        self.base.is_some() || self.held.is_some() || self.published.is_some()
    }
}

/// A `.swarmignore` text handed out for the roster, by hash: None when it was emptied.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
struct Published {
    hash: Option<Hash>,
}

/// An allowed file in the folder. `hash` is None for an empty file.
struct OnDisk {
    modified: u64,
    hash: Option<Hash>,
}

/// The root `.swarmignore` as the scan found it.
#[derive(Default)]
enum IgnoreFile {
    #[default]
    Absent,
    Text {
        bytes: Bytes,
        /// Unix microseconds.
        modified: u64,
    },
    /// There but not readable this time; left alone until it is.
    Unreadable,
}

#[derive(Default)]
struct Scan {
    /// Every file that syncs as a file entry. `.swarmignore` is never one of them.
    files: BTreeMap<RelPath, OnDisk>,
    ignore: IgnoreFile,
    /// Files hashed in this scan.
    seen: BTreeMap<RelPath, Seen>,
    too_big: BTreeSet<RelPath>,
    /// Allowed files that couldn't be read this time; left alone until they can.
    unreadable: BTreeSet<RelPath>,
    /// Every folder the scan went into, as the disk spells it. The swarm folder itself isn't one.
    folders: BTreeMap<RelPath, DiskFolder>,
}

impl Scan {
    /// Every file path in the scan at or in `from` moved to `to`, as a folder rename on disk
    /// leaves them, except those in `keep`.
    fn rebase(&mut self, from: &RelPath, to: &RelPath, keep: &BTreeSet<RelPath>) {
        let moved = |p: &RelPath| {
            if keep.contains(p) {
                None
            } else {
                layout::rebase(p, from, to)
            }
        };
        self.files = rekey(std::mem::take(&mut self.files), moved);
        self.seen = rekey(std::mem::take(&mut self.seen), moved);
        let set = |s: &mut BTreeSet<RelPath>| {
            *s = std::mem::take(s)
                .into_iter()
                .map(|p| moved(&p).unwrap_or(p))
                .collect();
        };
        set(&mut self.too_big);
        set(&mut self.unreadable);
    }
}

/// `map` with each key `moved` gives a new place for moved there. A moved key never replaces
/// one already at its new place.
fn rekey<V>(
    map: BTreeMap<RelPath, V>,
    moved: impl Fn(&RelPath) -> Option<RelPath>,
) -> BTreeMap<RelPath, V> {
    let (go, stay): (Vec<_>, Vec<_>) = map.into_iter().partition(|(p, _)| moved(p).is_some());
    let mut out: BTreeMap<RelPath, V> = stay.into_iter().collect();
    for (p, v) in go {
        if let Some(n) = moved(&p) {
            out.entry(n).or_insert(v);
        }
    }
    out
}

/// What the folder layout's part of a pass leaves for after the file pass.
struct LayoutPass {
    remove_if_empty: Vec<RelPath>,
    forget: Vec<RelPath>,
    know: Vec<(RelPath, KnownFolder)>,
    notices: Vec<LayoutNotice>,
    /// Folder renames carried out on disk this pass.
    done: Vec<(RelPath, RelPath)>,
    /// Folder renames that couldn't be carried out this pass (a file in the folder is open, say),
    /// and every one after them. They are tried again next pass; until then nothing at or in
    /// either of their folders is touched, so nothing is half done.
    waiting: Vec<(RelPath, RelPath)>,
}

impl LayoutPass {
    /// Whether `p` is at or in a folder whose rename is waiting.
    fn blocked(&self, p: &RelPath) -> bool {
        self.waiting.iter().any(|(from, to)| {
            layout::rebase(p, from, from).is_some() || layout::rebase(p, to, to).is_some()
        })
    }
}

/// What one pass knows of the swarm, for carrying out decisions.
struct Pass<'a> {
    node: &'a SwarmNode,
    history: &'a BTreeMap<RelPath, FileHistory>,
    /// The newest entry per path.
    remote: &'a BTreeMap<RelPath, Remote>,
    machine: &'a str,
}

type ApplyHook = Box<dyn FnMut(&RelPath) + Send + Sync>;

pub struct Folder {
    root: PathBuf,
    /// Where the index is kept: this machine's state for the swarm, outside the folder.
    state: PathBuf,
    index: BTreeMap<RelPath, Entry>,
    /// The folder index: the folders this machine and the swarm last agreed on. None until it
    /// has first been saved, which tells an upgrade (or a lost index) from a layout with no
    /// folders in it.
    folders: Option<BTreeMap<RelPath, KnownFolder>>,
    /// Conflict copies in the folder as of the last pass.
    copies: BTreeSet<RelPath>,
    too_big: BTreeSet<RelPath>,
    /// The attention list as last reported, so only changes are reported.
    reported: Vec<Attention>,
    /// Where a path is on disk when its spelling there differs from the swarm's only by case.
    disk_names: HashMap<RelPath, RelPath>,
    /// Runs just before each path's decision is carried out, so tests can save in between.
    before_apply: Option<ApplyHook>,
}

impl Folder {
    /// The folder at `root`, made if it is missing, with its index kept in `state`. Joining
    /// (`join`) needs `root` to be empty or new.
    pub fn new(root: PathBuf, state: PathBuf, join: bool) -> anyhow::Result<Folder> {
        if join && fs::read_dir(&root).is_ok_and(|mut d| d.next().is_some()) {
            bail!("A swarm can only be joined into an empty or new folder.");
        }
        fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
        let (index, folders) = load_index(&state.join(INDEX_FILE))?;
        Ok(Folder {
            root,
            state,
            index,
            folders,
            copies: BTreeSet::new(),
            too_big: BTreeSet::new(),
            reported: Vec::new(),
            disk_names: HashMap::new(),
            before_apply: None,
        })
    }

    /// Scans disk and doc, decides per path, applies; run at start, on doc changes, on
    /// (debounced) watcher events, and every 60 s.
    pub async fn reconcile(
        &mut self,
        node: &SwarmNode,
        roster: &Roster,
        me: &Member,
    ) -> anyhow::Result<Vec<FolderEvent>> {
        let rules = std::sync::Arc::new(SyncRules::new(
            &roster.types,
            roster.ignore.as_ref().map(|i| i.text.as_str()),
        ));
        let mut scan = {
            let root = self.root.clone();
            let seen: HashMap<RelPath, Seen> = self
                .index
                .iter()
                .filter_map(|(p, e)| Some((self.disk_names.get(p).unwrap_or(p).clone(), e.seen?)))
                .collect();
            let rules = rules.clone();
            tokio::task::spawn_blocking(move || scan(&root, &rules, &seen)).await??
        };
        // Ruling P1: a `files/.swarmignore` entry is never read, since any writer can put one
        // there and the file must count only as an owner set it in the roster.
        let syncs = |p: &RelPath| p.as_str() != SWARMIGNORE && rules.file_syncs(p);
        let raw = node.file_entries(roster).await?;
        let writer = roster
            .role_of(&me.person)
            .is_some_and(|r| r >= Role::Writer)
            && node.can_write();
        let author = node.author();
        let machine = machine_name(roster, me, &node.endpoint_id());
        let tz = TimeZone::system();
        let mut events = Vec::new();
        // The layout first (spec §5.2): a folder renamed elsewhere is renamed here before any file
        // change from the same rename is applied, and file entries are read at the paths the
        // folder moves put them.
        let (layout, history) = self
            .layout_before_files(
                node,
                roster,
                &rules,
                &mut scan,
                &raw,
                &syncs,
                (writer, &author, &machine),
                &tz,
                &mut events,
            )
            .await?;
        // A path that has stopped syncing forgets the version it last agreed on (and stays as it
        // is on disk), so that if it syncs again it comes back as a new file would. Otherwise a
        // delete or edit made meanwhile would be taken as a change to the swarm's copy.
        for (p, e) in self.index.iter_mut() {
            if p.as_str() != SWARMIGNORE && !syncs(p) {
                e.base = None;
                e.from = None;
            }
        }
        // Keys that differ only by case are one file on Windows and macOS. The key whose newest
        // entry is newest owns it; the content of the others is kept as conflict copies of it.
        let (mut names, shadowed) = case_owners(&history);
        let remote: BTreeMap<RelPath, Remote> = history
            .iter()
            .filter(|(p, _)| names.get(&fold(p)) == Some(p))
            .filter_map(|(p, h)| Some((p.clone(), newest(&h.entries)?.clone())))
            .collect();
        self.adopt_spellings(&mut names);
        let scan = self.respell(scan, &names);
        for (p, seen) in &scan.seen {
            self.index.entry(p.clone()).or_default().seen = Some(*seen);
        }
        let pass = Pass {
            node,
            history: &history,
            remote: &remote,
            machine: &machine,
        };

        let mut last: Option<(String, u64)> = None;
        match self
            .sync_ignore(
                node,
                roster,
                me,
                &scan.ignore,
                (&author, &machine),
                &tz,
                &mut events,
            )
            .await
        {
            Ok(change) => last = change,
            Err(e) => events.push(FolderEvent::Error(format!("{SWARMIGNORE}: {e:#}"))),
        }
        let mut paths: BTreeSet<RelPath> =
            scan.files.keys().chain(remote.keys()).cloned().collect();
        // Synced paths that no longer sync (a type taken off the list, a folder starting with a
        // dot, a match in `.swarmignore`) are left as they are on every machine, not deleted.
        paths.extend(
            self.index
                .iter()
                .filter(|(p, e)| (e.base.is_some() || e.held.is_some()) && syncs(p))
                .map(|(p, _)| p.clone()),
        );
        for p in paths {
            if scan.too_big.contains(&p) || scan.unreadable.contains(&p) || layout.blocked(&p) {
                continue;
            }
            let mut entry = self.index.get(&p).cloned().unwrap_or_default();
            // A reader promoted since its edit was held: the edit goes through like any other.
            if writer && entry.held == Some(Held::ReaderEdit) {
                entry.held = None;
                self.entry(&p).held = None;
            }
            let file = scan.files.get(&p);
            // An empty file is never published, since iroh-docs can't tell it from a delete: it
            // counts as untouched.
            let local = Local {
                hash: file.and_then(|f| f.hash.or(entry.base)),
                modified: file.map_or(0, |f| f.modified),
            };
            let r = remote.get(&p);
            // The swarm has a version of a path it had stopped having: it syncs again.
            if entry.held == Some(Held::NoLongerShared) && r.is_some() {
                entry.held = None;
                self.entry(&p).held = None;
            }
            // Ruling 20: a version taken in whose entry no longer counts (its author was taken
            // out without it being kept) is kept as a conflict copy before anything replaces it,
            // and when nothing replaces it, the file is flagged as on this machine only.
            let orphan = match (entry.base, entry.from.as_ref()) {
                (Some(base), Some(from)) if local.hash == Some(base) && entry.held.is_none() => {
                    (!counts(roster, history.get(&p), &p, from, base)).then_some((base, from))
                }
                _ => None,
            };
            if orphan.is_some() && r.is_none() {
                self.entry(&p).held = Some(Held::NoLongerShared);
                events.push(FolderEvent::Held(held_attention(&p, Held::NoLongerShared)));
                continue;
            }
            if let Some((base, from)) = orphan
                && r.is_some_and(|r| r.hash != Some(base))
            {
                let gone = Remote {
                    hash: Some(base),
                    at: from.at,
                    author: from.author.clone(),
                    machine: from.machine.clone(),
                };
                match self
                    .capture(&pass, &p, &gone, &tz, &scan.files, writer)
                    .await
                {
                    Ok(Some(copy)) => events.push(FolderEvent::NewConflict {
                        path: p.clone(),
                        copy,
                    }),
                    Ok(None) => {}
                    Err(e) => {
                        // Not replaced until it is safe in its copy.
                        events.push(FolderEvent::Error(format!("{}: {e:#}", p.as_str())));
                        continue;
                    }
                }
            }
            let scanned = file.map(|f| f.hash.unwrap_or_else(|| Hash::new([])));
            let action = decide(
                &p,
                entry.base,
                &local,
                r,
                (&author, &machine),
                writer,
                entry.held,
                &tz,
            );
            if let Some(hook) = self.before_apply.as_mut() {
                hook(&p);
            }
            match self
                .apply(&pass, &p, action, &local, entry.base, scanned, &mut events)
                .await
            {
                Ok(Some(change)) => {
                    if last.as_ref().is_none_or(|l| change.1 > l.1) {
                        last = Some(change);
                    }
                }
                Ok(None) => {}
                Err(e) => events.push(FolderEvent::Error(format!("{}: {e:#}", p.as_str()))),
            }
        }
        // Edits made at the same time as the newest version, which it would otherwise hide, and
        // the content of keys owned by a key differing only by case.
        if writer {
            let unseen = history.iter().flat_map(|(p, h)| {
                unseen_edits(h)
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |e| (p, e))
            });
            let cased = shadowed
                .iter()
                .filter_map(|(owner, key)| Some((owner, newest(&history[key].entries)?)));
            let all: Vec<_> = unseen
                .chain(cased)
                .filter(|(p, _)| !layout.blocked(p))
                .collect();
            for (p, edit) in all {
                match self.capture(&pass, p, edit, &tz, &scan.files, true).await {
                    Ok(Some(copy)) => events.push(FolderEvent::NewConflict {
                        path: p.clone(),
                        copy,
                    }),
                    Ok(None) => {}
                    Err(e) => events.push(FolderEvent::Error(format!("{}: {e:#}", p.as_str()))),
                }
            }
        }
        self.layout_after_files(layout, &mut events);
        self.index.retain(|_, e| e.worth_keeping());
        self.too_big = scan.too_big;
        self.copies = scan
            .files
            .keys()
            .chain(self.index.keys())
            .filter(|p| is_conflict_copy(p) && self.is_file(p))
            .cloned()
            .collect();
        self.save()?;
        if let Some((machine, at)) = last {
            events.push(FolderEvent::LastChange { machine, at });
        }
        let attention = self.attention();
        if attention != self.reported {
            events.push(FolderEvent::Attention(attention.clone()));
            self.reported = attention;
        }
        Ok(events)
    }

    /// The folder layout's part of a pass before the file pass: works out the plan, publishes its
    /// records, carries out renames found here and incoming ones, and makes the folders the swarm
    /// has. Gives what is left for after the file pass, and the swarm's file entries at the paths
    /// the folder moves put them, this machine's own new renames included.
    #[allow(clippy::too_many_arguments)]
    async fn layout_before_files(
        &mut self,
        node: &SwarmNode,
        roster: &Roster,
        rules: &SyncRules,
        scan: &mut Scan,
        raw: &BTreeMap<RelPath, FileHistory>,
        syncs: &(dyn Fn(&RelPath) -> bool + Sync),
        (writer, author, machine): (bool, &str, &str),
        tz: &TimeZone,
        events: &mut Vec<FolderEvent>,
    ) -> anyhow::Result<(LayoutPass, BTreeMap<RelPath, FileHistory>)> {
        let records = node.folder_records(roster).await?;
        let mut log = node.moves(roster).await?;
        let moves = Moves::from_moves(&log);
        self.take_up_moves_done(&moves, &scan.folders);
        let mut history = mapped(raw, &moves, syncs);
        let live_files: BTreeSet<RelPath> = history
            .iter()
            .filter(|(_, h)| newest(&h.entries).is_some_and(|e| e.hash.is_some()))
            .map(|(p, _)| p.clone())
            .collect();
        let newest_file_at = |f: &RelPath| {
            history
                .iter()
                .filter(|(p, _)| layout::inside(p, f))
                .flat_map(|(_, h)| h.entries.iter().map(|e| e.at))
                .max()
        };
        let syncs_folder = |p: &RelPath| rules.folder_may_sync(p);
        let known = self.folders.clone().unwrap_or_default();
        let plan = layout::plan(&LayoutInput {
            me: author,
            known: &known,
            disk: &scan.folders,
            records: &records,
            moves: &log,
            live_files: &live_files,
            writer,
            now: now_micros(),
            newest_file_at: &newest_file_at,
            syncs_folder: &syncs_folder,
            first_pass: self.folders.is_none(),
        });

        // Published before anything here changes. If the swarm can't take them, the pass stops
        // and the next one finds the same changes again, rather than this machine moving on with a
        // rename the swarm never heard of.
        for m in &plan.publish_moves {
            node.put_move(m).await?;
        }
        for (p, r) in &plan.publish {
            node.put_folder(p, r).await?;
        }
        // This machine's own renames are in force for the file pass at once, so its entries under
        // the old name are read under the new one rather than written back there.
        if !plan.publish_moves.is_empty() {
            log.extend(plan.publish_moves.iter().map(|m| MoveVersion {
                record: m.clone(),
                author: author.to_string(),
                machine: machine.to_string(),
            }));
            history = mapped(raw, &Moves::from_moves(&log), syncs);
        }

        // Paths from here on are the disk's own spellings, as the scan found them.
        self.disk_names.clear();
        // Renamed here: the files are already at the new path on disk, so only the index follows.
        for (from, to) in &plan.detected_moves {
            self.rebase_index(from, to, &BTreeSet::new());
        }
        let mut pass = LayoutPass {
            remove_if_empty: plan.remove_if_empty,
            forget: plan.forget,
            know: plan.know,
            notices: plan.notices,
            done: Vec::new(),
            waiting: Vec::new(),
        };
        for (from, to) in plan.disk_moves {
            // Each rename starts from where the ones before it left things.
            if !pass.waiting.is_empty() {
                pass.waiting.push((from, to));
                continue;
            }
            match self.move_folder(&from, &to, rules, (machine, tz)) {
                Ok(clashes) => {
                    self.settle_move(scan, &history, &from, &to, clashes);
                    pass.done.push((from, to));
                }
                Err(e) => {
                    events.push(FolderEvent::Error(format!(
                        "{} couldn't be renamed to {} yet, so it will be tried again: {e:#}",
                        from.as_str(),
                        to.as_str()
                    )));
                    pass.waiting.push((from, to));
                }
            }
        }
        for p in &plan.create {
            if pass.blocked(p) {
                continue;
            }
            if let Err(e) = self.make_folder(p) {
                events.push(FolderEvent::Error(format!("{}: {e:#}", p.as_str())));
            }
        }
        Ok((pass, history))
    }

    /// Takes up renames already carried out on disk that the folder index doesn't show: a pass
    /// that renamed a folder and stopped before saving leaves the index with the old name and the
    /// disk with the new one. Read as they stand, the plan would take that as a rename made here
    /// and publish it again, dated now, pulling in work someone made at the old name since, or as
    /// a delete of the old name and its files. A known folder gone from disk whose move in force
    /// lands on a folder that is on disk is that move, done.
    fn take_up_moves_done(&mut self, moves: &Moves, disk: &BTreeMap<RelPath, DiskFolder>) {
        let Some(folders) = self.folders.as_mut() else {
            return;
        };
        let on_disk: HashMap<String, &RelPath> = disk.keys().map(|p| (fold(p), p)).collect();
        let mut done: Vec<(RelPath, RelPath)> = folders
            .iter()
            .filter(|(k, _)| !disk.contains_key(*k))
            .filter_map(|(k, kf)| {
                // Only the moves newer than what this machine last agreed on for it.
                let t = moves.map(k, kf.applied_at.saturating_add(1));
                let d = on_disk.get(&fold(&t))?;
                (fold(&t) != fold(k)).then(|| (k.clone(), (*d).clone()))
            })
            .collect();
        // Deepest first, so a subfolder renamed on its own isn't carried along by its parent.
        done.sort_by_key(|(k, _)| std::cmp::Reverse(k.as_str().matches('/').count()));
        for (k, d) in done {
            if let Some(folders) = self.folders.as_mut()
                && let Some(v) = folders.remove(&k)
            {
                folders.entry(d.clone()).or_insert(v);
            }
            self.rebase_index(&k, &d, &BTreeSet::new());
        }
    }

    /// The folder layout's part of a pass after the file pass: removes the folders the swarm
    /// deleted once the file pass has emptied them, updates the folder index and tells the
    /// person what they need to know.
    fn layout_after_files(&mut self, mut pass: LayoutPass, events: &mut Vec<FolderEvent>) {
        // Deepest first, and only a folder with nothing left in it: one still holding anything,
        // synced or not, stays (spec §6), so a failure here is expected and not an error.
        for p in &pass.remove_if_empty {
            if pass.blocked(p) {
                continue;
            }
            if let Ok(Some(path)) = self.inside(p, false)
                && fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.is_symlink())
            {
                let _ = fs::remove_dir(&path);
            }
        }
        let mut folders = self.folders.take().unwrap_or_default();
        for k in &pass.forget {
            let now_at = layout::after(&pass.done, k);
            // Still waiting to be renamed: it stays known where it is now, so the rename is
            // planned again next pass.
            if pass
                .waiting
                .iter()
                .any(|(from, _)| layout::rebase(&now_at, from, from).is_some())
            {
                if let Some(v) = folders.remove(k) {
                    folders.insert(now_at, v);
                }
                continue;
            }
            folders.remove(k);
        }
        for (p, k) in &pass.know {
            if !pass.blocked(p) {
                folders.insert(p.clone(), k.clone());
            }
        }
        self.folders = Some(folders);
        for notice in std::mem::take(&mut pass.notices) {
            let (at, event) = match notice {
                LayoutNotice::RenameLost {
                    mine,
                    kept,
                    by_machine,
                } => (
                    kept.clone(),
                    FolderEvent::RenameLost {
                        mine,
                        kept,
                        machine: by_machine,
                    },
                ),
                LayoutNotice::DeleteKeptForWork {
                    path,
                    deleted_by_machine,
                } => (
                    path.clone(),
                    FolderEvent::DeleteKeptForWork {
                        path,
                        machine: deleted_by_machine,
                    },
                ),
                LayoutNotice::MyDeleteKept { path } => {
                    (path.clone(), FolderEvent::MyDeleteKept { path })
                }
                LayoutNotice::MyDeleteLostToRename {
                    path,
                    to,
                    by_machine,
                } => (
                    to.clone(),
                    FolderEvent::MyDeleteLostToRename {
                        path,
                        to,
                        machine: by_machine,
                    },
                ),
                LayoutNotice::MyRenameSurvivedDelete { to, by_machine } => (
                    to.clone(),
                    FolderEvent::MyRenameSurvivedDelete {
                        to,
                        machine: by_machine,
                    },
                ),
            };
            // A waiting rename plans the same notice again next pass, once it is done.
            if !pass.blocked(&at) {
                events.push(event);
            }
        }
    }

    /// Index keys at or in `from` moved to `to`, except those in `keep`. An entry already at the
    /// new key stays.
    fn rebase_index(&mut self, from: &RelPath, to: &RelPath, keep: &BTreeSet<RelPath>) {
        self.index = rekey(std::mem::take(&mut self.index), |p| {
            if keep.contains(p) {
                None
            } else {
                layout::rebase(p, from, to)
            }
        });
    }

    /// After a folder moved on disk from `from` to `to`: the index and the scan follow it, so the
    /// file pass sees the same files, unchanged, under the new name, and never a delete of the
    /// old paths.
    fn settle_move(
        &mut self,
        scan: &mut Scan,
        history: &BTreeMap<RelPath, FileHistory>,
        from: &RelPath,
        to: &RelPath,
        clashes: Vec<(RelPath, RelPath)>,
    ) {
        // Met a file of the same name and went in under a copy name: new files there, made from
        // nothing the swarm has at that name.
        for (old, copy) in clashes {
            self.index.remove(&old);
            scan.seen.remove(&old);
            if let Some(f) = scan.files.remove(&old) {
                scan.files.insert(copy, f);
            }
        }
        // A file whose newest entry is newer than the move stays at the old name in the swarm: a
        // new folder made there by someone who hadn't seen the rename (spec §5.2). It goes back
        // there, so this machine ends with what every other one has.
        let mut stay = BTreeSet::new();
        let under: Vec<RelPath> = scan
            .files
            .keys()
            .filter(|p| layout::inside(p, from))
            .cloned()
            .collect();
        for p in under {
            let Some(q) = layout::rebase(&p, from, to) else {
                continue;
            };
            let stands = history
                .get(&p)
                .and_then(|h| newest(&h.entries))
                .is_some_and(|e| e.hash.is_some())
                && !history.contains_key(&q);
            if stands && self.move_file(&q, &p).is_ok() {
                stay.insert(p);
            }
        }
        self.rebase_index(from, to, &stay);
        scan.rebase(from, to, &stay);
    }

    /// `p` on disk when it is a folder there, not a link.
    fn folder_at(&self, p: &RelPath) -> anyhow::Result<Option<PathBuf>> {
        let Some(path) = self.inside(p, false)? else {
            return Ok(None);
        };
        Ok(fs::symlink_metadata(&path)
            .is_ok_and(|m| m.is_dir() && !m.is_symlink())
            .then_some(path))
    }

    /// Makes the folder `p`, and any folder on the way to it.
    fn make_folder(&self, p: &RelPath) -> anyhow::Result<()> {
        let path = self
            .inside(p, true)?
            .context("a folder on the way is missing")?;
        match fs::create_dir(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
        }
    }

    /// Moves the file at `from` to `to`, which must be free.
    fn move_file(&self, from: &RelPath, to: &RelPath) -> anyhow::Result<()> {
        let src = self.inside(from, false)?.context("it isn't there")?;
        let dst = self
            .inside(to, true)?
            .context("a folder on the way is missing")?;
        if fs::symlink_metadata(&dst).is_ok() {
            bail!("{} is taken", to.as_str());
        }
        fs::rename(&src, &dst).with_context(|| format!("moving {}", src.display()))
    }

    /// Renames the folder `from` to `to` on disk, with everything in it. When another folder is
    /// already at `to`, the two merge (spec §5.2): each thing in `from` moves in, and a synced file
    /// meeting a file of the same name goes in under a conflict-copy name, so nothing is
    /// overwritten or deleted. Gives those files, as (path before, copy). On an error `from` is
    /// as it was, or, part way through a merge, holds what hadn't moved yet; the next pass
    /// carries on from there.
    fn move_folder(
        &self,
        from: &RelPath,
        to: &RelPath,
        rules: &SyncRules,
        (machine, tz): (&str, &TimeZone),
    ) -> anyhow::Result<Vec<(RelPath, RelPath)>> {
        let src = self
            .folder_at(from)?
            .with_context(|| format!("{} isn't there", from.as_str()))?;
        let dst = self
            .inside(to, true)?
            .context("a folder on the way is missing")?;
        let mut clashes = Vec::new();
        match other_entry(&dst, &src)? {
            Some(there) if fs::symlink_metadata(&there).is_ok_and(|m| m.is_dir()) => {
                self.merge(&src, &there, (from, to), rules, (machine, tz), &mut clashes)?;
            }
            Some(_) => bail!("something other than a folder is at {}", to.as_str()),
            None => rename_dir(&src, &dst)?,
        }
        Ok(clashes)
    }

    /// Moves everything in the folder `src` into the folder `dst`, as `move_folder` says.
    fn merge(
        &self,
        src: &Path,
        dst: &Path,
        (from, to): (&RelPath, &RelPath),
        rules: &SyncRules,
        (machine, tz): (&str, &TimeZone),
        clashes: &mut Vec<(RelPath, RelPath)>,
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let s = entry.path();
            let Some(there) = other_entry(&dst.join(&name), &s)? else {
                fs::rename(&s, dst.join(&name))
                    .with_context(|| format!("moving {}", s.display()))?;
                continue;
            };
            let name = name.to_string_lossy();
            let (Some(old), Some(new)) = (
                safe_rel_path(&format!("{}/{name}", from.as_str())),
                safe_rel_path(&format!("{}/{name}", to.as_str())),
            ) else {
                continue;
            };
            let there_is_dir = fs::symlink_metadata(&there).is_ok_and(|m| m.is_dir());
            if kind.is_dir() && there_is_dir {
                self.merge(&s, &there, (&old, &new), rules, (machine, tz), clashes)?;
            } else if kind.is_file() && rules.file_syncs(&new) {
                let bytes = fs::read(&s).with_context(|| format!("reading {}", s.display()))?;
                // Named for the version it holds, as a conflict copy taken from the swarm is, so
                // machines merging the same folders name it the same.
                let name = match self.index.get(&old).and_then(|e| e.from.as_ref()) {
                    Some(made) => conflict_name(&new, &made.machine, timestamp(made.at), tz),
                    None => {
                        let at = modified(&entry.metadata()?);
                        conflict_name(&new, machine, timestamp(at), tz)
                    }
                };
                let copy = self.free_copy_name(&name, Hash::new(&bytes), &BTreeMap::new())?;
                let path = self
                    .inside(&copy, true)?
                    .context("a folder on the way is missing")?;
                fs::rename(&s, &path).with_context(|| format!("moving {}", s.display()))?;
                clashes.push((old, copy));
            }
            // Anything else stays where it is, and so does the old folder holding it: a folder
            // meeting a file, a link, and a file that doesn't sync, which hydra never renames
            // (spec §6).
        }
        // Empty now, unless something had to stay.
        let _ = fs::remove_dir(src);
        Ok(())
    }

    pub fn attention(&self) -> Vec<Attention> {
        let conflicts = self.copies.iter().filter_map(|copy| {
            Some(Attention::Conflict {
                path: conflict_original(copy)?,
                copy: copy.clone(),
            })
        });
        let held = self
            .index
            .iter()
            .filter_map(|(p, e)| Some(held_attention(p, e.held?)));
        let big = self
            .too_big
            .iter()
            .map(|p| Attention::TooBig { path: p.clone() });
        conflicts.chain(held).chain(big).collect()
    }

    /// Runs `hook` just before each path's decision is carried out, for tests that change the
    /// folder mid-pass.
    #[doc(hidden)]
    pub fn before_apply_for_tests(&mut self, hook: impl FnMut(&RelPath) + Send + Sync + 'static) {
        self.before_apply = Some(Box::new(hook));
    }

    /// Keeps the version at `path`: the conflict copy is deleted everywhere.
    pub async fn keep_theirs(
        &mut self,
        node: &SwarmNode,
        path: &RelPath,
        copy: &RelPath,
    ) -> anyhow::Result<()> {
        check_copy(path, copy)?;
        can_write(node)?;
        let history = node_history(node).await?;
        let base = self.index.get(copy).and_then(|e| e.base);
        self.publish(node, history.get(copy), copy, None, &Vec::from_iter(base))
            .await?;
        self.drop_copy(copy)
    }

    /// Keeps the conflict copy's version: it goes to `path`, and the copy is deleted everywhere.
    pub async fn keep_mine(
        &mut self,
        node: &SwarmNode,
        path: &RelPath,
        copy: &RelPath,
    ) -> anyhow::Result<()> {
        check_copy(path, copy)?;
        can_write(node)?;
        let types = types(node).await?;
        let bytes = self
            .read(copy)?
            .context("The conflict copy is no longer there.")?;
        if !allowed(path, bytes.len() as u64, &types) || bytes.is_empty() {
            bail!("{} can't be shared.", copy.as_str());
        }
        let hash = Hash::new(&bytes);
        let history = node_history(node).await?;
        // Made on top of both the version at `path` and the copy.
        let heads: Vec<Hash> = self
            .index
            .get(path)
            .and_then(|e| e.base)
            .into_iter()
            .chain([hash])
            .collect();
        self.write(path, &bytes)?;
        self.publish(node, history.get(path), path, Some((bytes, hash)), &heads)
            .await?;
        let copy_base = self.index.get(copy).and_then(|e| e.base);
        self.publish(
            node,
            history.get(copy),
            copy,
            None,
            &Vec::from_iter(copy_base),
        )
        .await?;
        self.drop_copy(copy)
    }

    /// Publishes a file kept here after the swarm deleted it, and resumes syncing it.
    pub async fn share_again(&mut self, node: &SwarmNode, path: &RelPath) -> anyhow::Result<()> {
        let held = self.index.get(path).and_then(|e| e.held);
        if !matches!(held, Some(Held::EditedAfterDelete | Held::NoLongerShared)) {
            bail!("{} isn't waiting to be shared again.", path.as_str());
        }
        can_write(node)?;
        if path.as_str() == SWARMIGNORE {
            // With no base, the next pass sees the file as an edit, and an owner's goes out
            // through the roster rather than as a file entry.
            let entry = self.entry(path);
            entry.held = None;
            entry.base = None;
            entry.from = None;
            entry.published = None;
            return self.save();
        }
        let types = types(node).await?;
        let history = node_history(node).await?;
        let before = self.index.get(path).and_then(|e| e.base);
        match self.read(path)? {
            Some(bytes) => {
                if bytes.is_empty() || !allowed(path, bytes.len() as u64, &types) {
                    bail!("{} can't be shared.", path.as_str());
                }
                let hash = Hash::new(&bytes);
                self.publish(
                    node,
                    history.get(path),
                    path,
                    Some((bytes, hash)),
                    &Vec::from_iter(before),
                )
                .await?;
            }
            None => self.entry(path).base = None,
        }
        self.entry(path).held = None;
        self.tidy(path);
        self.save()
    }

    /// Deletes a file held on this machine, and resumes syncing its path.
    pub async fn delete_held(&mut self, path: &RelPath) -> anyhow::Result<()> {
        if self.index.get(path).and_then(|e| e.held).is_none() {
            bail!("{} isn't held.", path.as_str());
        }
        self.remove(path)?;
        // Forgetting the base too: whatever the swarm has now is new to this folder.
        self.index.remove(path);
        self.save()
    }

    /// Puts the swarm's version of a held file back, and resumes syncing it.
    pub async fn use_swarm_version(
        &mut self,
        node: &SwarmNode,
        path: &RelPath,
    ) -> anyhow::Result<()> {
        let roster = node
            .roster()
            .await?
            .context("This swarm's member list hasn't arrived yet.")?;
        if path.as_str() == SWARMIGNORE {
            // The swarm's version is the owners', from the roster, never a file entry.
            let remote = ignore_remote(&roster);
            match remote.as_ref().and_then(|r| r.hash) {
                Some(_) => {
                    let text = roster.ignore.as_ref().map_or("", |i| i.text.as_str());
                    self.write(path, text.as_bytes())?;
                }
                None => self.remove(path)?,
            }
            self.took_ignore(path, remote.as_ref());
            self.entry(path).held = None;
            self.tidy(path);
            return self.save();
        }
        let remote = swarm_history(node, &roster)
            .await?
            .get(path)
            .and_then(|h| newest(&h.entries))
            .cloned();
        let from = remote.as_ref().map(Source::of);
        let base = match remote.and_then(|r| r.hash) {
            Some(hash) => {
                let bytes = node
                    .read_blob(hash)
                    .await?
                    .context("The swarm's version hasn't arrived yet.")?;
                self.write(path, &bytes)?;
                Some(hash)
            }
            None => {
                self.remove(path)?;
                None
            }
        };
        let entry = self.entry(path);
        entry.base = base;
        entry.from = from;
        entry.held = None;
        self.tidy(path);
        self.save()
    }

    /// Undoes what a pass recorded when it handed out `FolderEvent::PublishIgnore`, for when the
    /// roster change couldn't be made. With no base, the next pass meets the roster's text as an
    /// edit made alongside this one, so this machine's text is held or kept as a copy, never
    /// written over.
    pub fn ignore_not_published(&mut self) -> anyhow::Result<()> {
        let p = ignore_path();
        if let Some(entry) = self.index.get_mut(&p) {
            entry.base = None;
            entry.from = None;
            entry.published = None;
        }
        self.tidy(&p);
        self.save()
    }

    /// Fires 500 ms after the last change in the folder, and only once every changed file's
    /// size is the same in two reads 100 ms apart. Stops watching when the receiver is dropped.
    pub fn watch(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel(1);
        let root = self.root.clone();
        // Without a watcher the folder still syncs on the periodic pass.
        let _ = std::thread::Builder::new()
            .name("hydra-swarm-watch".into())
            .spawn(move || watch_folder(&root, &tx));
        rx
    }

    /// Carries out one decision. Gives the change applied, as (machine, unix microseconds),
    /// when there was one.
    #[allow(clippy::too_many_arguments)]
    async fn apply(
        &mut self,
        pass: &Pass<'_>,
        p: &RelPath,
        action: Action,
        local: &Local,
        base: Option<Hash>,
        scanned: Option<Hash>,
        events: &mut Vec<FolderEvent>,
    ) -> anyhow::Result<Option<(String, u64)>> {
        let node = pass.node;
        let remote = pass.remote.get(p);
        let history = pass.history.get(p);
        let theirs = remote.map(|r| (r.machine.clone(), r.at));
        let mine = Some((pass.machine.to_string(), now_micros()));
        match action {
            Action::Nothing => {
                // Both sides made the same change: that is the new base.
                if let Some(r) = remote
                    && r.hash == local.hash
                {
                    self.took(p, r);
                }
                Ok(None)
            }
            Action::Write(hash) => {
                let Some(bytes) = node.read_blob(hash).await? else {
                    return Ok(None);
                };
                if !self.as_scanned(p, scanned)? {
                    return Ok(None);
                }
                self.write(p, &bytes)?;
                self.took_hash(p, hash, remote);
                Ok(theirs)
            }
            Action::RemoveFile => {
                if !self.as_scanned(p, scanned)? {
                    return Ok(None);
                }
                self.remove(p)?;
                self.entry(p).base = None;
                self.entry(p).from = None;
                Ok(theirs)
            }
            Action::Publish => {
                let content = match local.hash {
                    Some(hash) => {
                        let Some(bytes) = self.read_unchanged(p, hash)? else {
                            return Ok(None);
                        };
                        Some((bytes, hash))
                    }
                    None => None,
                };
                self.publish(node, history, p, content, &Vec::from_iter(base))
                    .await?;
                Ok(mine)
            }
            Action::Hold(held) => {
                self.entry(p).held = Some(held);
                events.push(FolderEvent::Held(held_attention(p, held)));
                Ok(None)
            }
            Action::ConflictKeepRemote {
                copy_from_local: copy,
            } => {
                let Some((theirs_bytes, theirs_hash, mine_bytes, mine_hash)) =
                    self.both_sides(node, p, local, remote).await?
                else {
                    return Ok(None);
                };
                // The local edit is safe in its copy before the name takes the remote's.
                let copy = self.free_copy_name(&copy, mine_hash, pass.remote)?;
                self.write(&copy, &mine_bytes)?;
                let copy_history = pass.history.get(&copy);
                self.publish(
                    node,
                    copy_history,
                    &copy,
                    Some((mine_bytes, mine_hash)),
                    &[],
                )
                .await?;
                // Saved again meanwhile: that edit meets the remote one next pass.
                if !self.as_scanned(p, scanned)? {
                    return Ok(None);
                }
                self.write(p, &theirs_bytes)?;
                self.took_hash(p, theirs_hash, remote);
                events.push(FolderEvent::NewConflict {
                    path: p.clone(),
                    copy,
                });
                Ok(theirs)
            }
            Action::ConflictKeepLocal {
                copy_from_remote: copy,
            } => {
                let Some((theirs_bytes, theirs_hash, mine_bytes, mine_hash)) =
                    self.both_sides(node, p, local, remote).await?
                else {
                    return Ok(None);
                };
                let copy = self.free_copy_name(&copy, theirs_hash, pass.remote)?;
                self.write(&copy, &theirs_bytes)?;
                let copy_history = pass.history.get(&copy);
                self.publish(
                    node,
                    copy_history,
                    &copy,
                    Some((theirs_bytes, theirs_hash)),
                    &[],
                )
                .await?;
                // The local version has now seen the remote one, kept as the copy.
                let heads: Vec<Hash> = std::iter::once(theirs_hash).chain(base).collect();
                self.publish(node, history, p, Some((mine_bytes, mine_hash)), &heads)
                    .await?;
                events.push(FolderEvent::NewConflict {
                    path: p.clone(),
                    copy,
                });
                Ok(mine)
            }
            Action::Restore(hash) => {
                let Some(bytes) = node.read_blob(hash).await? else {
                    return Ok(None);
                };
                if !self.as_scanned(p, scanned)? {
                    return Ok(None);
                }
                self.write(p, &bytes)?;
                self.took_hash(p, hash, remote);
                events.push(FolderEvent::Restored(p.clone()));
                Ok(theirs)
            }
        }
    }

    /// Brings `.swarmignore` in step with the roster's, as `decide` would a file whose swarm
    /// version is the roster's text and whose writers are the owners. An owner's edit goes out as
    /// `FolderEvent::PublishIgnore`; anyone else's is held. Gives the change applied, as `apply`
    /// does.
    #[allow(clippy::too_many_arguments)]
    async fn sync_ignore(
        &mut self,
        node: &SwarmNode,
        roster: &Roster,
        me: &Member,
        on_disk: &IgnoreFile,
        (author, machine): (&str, &str),
        tz: &TimeZone,
        events: &mut Vec<FolderEvent>,
    ) -> anyhow::Result<Option<(String, u64)>> {
        let p = ignore_path();
        let (bytes, modified) = match on_disk {
            IgnoreFile::Absent => (None, 0),
            IgnoreFile::Text { bytes, modified } => (Some(bytes.clone()), *modified),
            IgnoreFile::Unreadable => return Ok(None),
        };
        let scanned = bytes.as_ref().map(Hash::new);
        // An empty `.swarmignore` says the same as none, so emptying it and deleting it are one
        // change, and neither brings an empty file back.
        let mine = bytes.filter(|b| !b.is_empty());
        let local = Local {
            hash: mine.as_ref().map(Hash::new),
            modified,
        };
        let remote = ignore_remote(roster);
        let owner = roster.role_of(&me.person) == Some(Role::Owner) && node.can_write();
        let theirs_text = roster
            .ignore
            .as_ref()
            .map(|i| Bytes::from(i.text.clone()))
            .unwrap_or_default();
        let theirs = remote.as_ref().map(|r| (r.machine.clone(), r.at));
        let mut entry = self.index.get(&p).cloned().unwrap_or_default();
        // The roster has moved on from what this machine published. If the newer owner's change
        // wasn't made on top of it and the file still holds it, theirs keeps the name and ours is
        // kept as a conflict copy, which never syncs. Otherwise it is an ordinary later edit.
        if let (Some(sent), Some(r)) = (entry.published, remote.as_ref())
            && r.hash != sent.hash
        {
            let replaces = roster.ignore.as_ref().and_then(|i| i.replaces.clone());
            let unseen = replaces != sent.hash.map(|h| h.to_hex());
            if let Some(mine) = mine.as_ref().filter(|_| unseen && local.hash == sent.hash) {
                if !self.as_scanned(&p, scanned)? {
                    return Ok(None);
                }
                let name = conflict_name(&p, machine, timestamp(local.modified), tz);
                let copy = self.free_copy_name(&name, Hash::new(mine), &BTreeMap::new())?;
                self.write(&copy, mine)?;
                match r.hash {
                    Some(_) => self.write(&p, &theirs_text)?,
                    None => self.remove(&p)?,
                }
                self.took_ignore(&p, Some(r));
                events.push(FolderEvent::NewConflict { path: p, copy });
                return Ok(theirs);
            }
            entry.published = None;
            self.entry(&p).published = None;
        }
        // Made an owner since the edit was held: it goes through like any other.
        if owner && entry.held == Some(Held::ReaderEdit) {
            entry.held = None;
            self.entry(&p).held = None;
        }
        let action = decide(
            &p,
            entry.base,
            &local,
            remote.as_ref(),
            (author, machine),
            owner,
            entry.held,
            tz,
        );
        match action {
            Action::Nothing => {
                if let Some(r) = remote.as_ref().filter(|r| r.hash == local.hash) {
                    self.took(&p, r);
                }
                Ok(None)
            }
            Action::Write(_) | Action::Restore(_) => {
                if !self.as_scanned(&p, scanned)? {
                    return Ok(None);
                }
                self.write(&p, &theirs_text)?;
                self.took_ignore(&p, remote.as_ref());
                if matches!(action, Action::Restore(_)) {
                    events.push(FolderEvent::Restored(p));
                }
                Ok(theirs)
            }
            Action::RemoveFile => {
                if !self.as_scanned(&p, scanned)? {
                    return Ok(None);
                }
                self.remove(&p)?;
                self.took_ignore(&p, remote.as_ref());
                Ok(theirs)
            }
            Action::Publish => {
                let Some(text) = ignore_text(mine.as_ref(), events) else {
                    return Ok(None);
                };
                events.push(FolderEvent::PublishIgnore {
                    text,
                    replaces: entry.base.map(|h| h.to_hex()),
                });
                let entry = self.entry(&p);
                entry.base = local.hash;
                entry.from = None;
                entry.published = Some(Published { hash: local.hash });
                Ok(Some((machine.to_string(), now_micros())))
            }
            Action::Hold(held) => {
                self.entry(&p).held = Some(held);
                events.push(FolderEvent::Held(held_attention(&p, held)));
                Ok(None)
            }
            // Conflict copies stay on this machine: the name has no type, so it never syncs, and
            // only `.swarmignore` itself has any effect.
            Action::ConflictKeepRemote {
                copy_from_local: copy,
            } => {
                let Some(mine) = mine else {
                    return Ok(None);
                };
                let copy = self.free_copy_name(&copy, Hash::new(&mine), &BTreeMap::new())?;
                self.write(&copy, &mine)?;
                if !self.as_scanned(&p, scanned)? {
                    return Ok(None);
                }
                self.write(&p, &theirs_text)?;
                self.took_ignore(&p, remote.as_ref());
                events.push(FolderEvent::NewConflict { path: p, copy });
                Ok(theirs)
            }
            Action::ConflictKeepLocal {
                copy_from_remote: copy,
            } => {
                let Some(text) = ignore_text(mine.as_ref(), events) else {
                    return Ok(None);
                };
                let copy = self.free_copy_name(&copy, Hash::new(&theirs_text), &BTreeMap::new())?;
                self.write(&copy, &theirs_text)?;
                // Made on top of theirs, which is kept as the copy.
                events.push(FolderEvent::PublishIgnore {
                    text,
                    replaces: remote.as_ref().and_then(|r| r.hash).map(|h| h.to_hex()),
                });
                let entry = self.entry(&p);
                entry.base = local.hash;
                entry.from = None;
                entry.published = Some(Published { hash: local.hash });
                events.push(FolderEvent::NewConflict { path: p, copy });
                Ok(Some((machine.to_string(), now_micros())))
            }
        }
    }

    /// Records the roster's `.swarmignore` as the version this folder now has.
    fn took_ignore(&mut self, p: &RelPath, remote: Option<&Remote>) {
        self.entry(p).published = None;
        match remote {
            Some(r) => self.took(p, r),
            None => {
                let entry = self.entry(p);
                entry.base = None;
                entry.from = None;
            }
        }
    }

    /// Publishes `content` at `p` (a delete when None), made on top of `heads`: first a parent
    /// record saying so, then the entry. Records the new base.
    async fn publish(
        &mut self,
        node: &SwarmNode,
        history: Option<&FileHistory>,
        p: &RelPath,
        content: Option<(Bytes, Hash)>,
        heads: &[Hash],
    ) -> anyhow::Result<()> {
        let hash = content.as_ref().map(|(_, h)| *h);
        let record = ParentRecord {
            hash,
            parent: heads.first().copied(),
            ancestors: lineage(heads, history),
        };
        node.put_parent(p, &record).await?;
        match content {
            Some((bytes, _)) => {
                node.put_file(p, bytes).await?;
            }
            None => node.delete_file(p).await?,
        }
        let entry = self.entry(p);
        entry.base = hash;
        entry.from = None;
        Ok(())
    }

    /// Records `r`, the swarm's entry for `p`, as the version this folder now has.
    fn took(&mut self, p: &RelPath, r: &Remote) {
        let entry = self.entry(p);
        entry.base = r.hash;
        entry.from = Some(Source::of(r));
    }

    /// Records `hash` as the version this folder now has, taken from `r` when it is `r`'s.
    fn took_hash(&mut self, p: &RelPath, hash: Hash, r: Option<&Remote>) {
        match r.filter(|r| r.hash == Some(hash)) {
            Some(r) => self.took(p, r),
            None => {
                let entry = self.entry(p);
                entry.base = Some(hash);
                entry.from = None;
            }
        }
    }

    /// Writes out `edit`, another writer's version of `p` that the newest version wasn't made
    /// on top of, as a conflict copy named for its machine and time, and publishes it when
    /// `share` (a reader's copy stays here), unless a copy of `p` already holds or held that
    /// content. Every machine names it the same, so they converge on one copy.
    async fn capture(
        &mut self,
        pass: &Pass<'_>,
        p: &RelPath,
        edit: &Remote,
        tz: &TimeZone,
        on_disk: &BTreeMap<RelPath, OnDisk>,
        share: bool,
    ) -> anyhow::Result<Option<RelPath>> {
        let Some(hash) = edit.hash else {
            return Ok(None);
        };
        if self.captured(p, hash, pass.history, on_disk) {
            return Ok(None);
        }
        let Some(bytes) = pass.node.read_blob(hash).await? else {
            return Ok(None);
        };
        let name = conflict_name(p, &edit.machine, timestamp(edit.at), tz);
        let copy = self.free_copy_name(&name, hash, pass.remote)?;
        self.write(&copy, &bytes)?;
        if share {
            let copy_history = pass.history.get(&copy);
            self.publish(pass.node, copy_history, &copy, Some((bytes, hash)), &[])
                .await?;
        }
        Ok(Some(copy))
    }

    /// Whether some conflict copy of `p`, here or in the swarm, holds `hash` or once did (a
    /// resolved copy's delete records what it held).
    fn captured(
        &self,
        p: &RelPath,
        hash: Hash,
        history: &BTreeMap<RelPath, FileHistory>,
        on_disk: &BTreeMap<RelPath, OnDisk>,
    ) -> bool {
        let copy_of_p = |c: &RelPath| conflict_original(c).as_ref() == Some(p);
        let in_swarm = history.iter().filter(|(c, _)| copy_of_p(c)).any(|(_, h)| {
            h.entries.iter().any(|e| e.hash == Some(hash))
                || h.records
                    .iter()
                    .any(|(_, r)| r.hash == Some(hash) || r.ancestors.contains(&hash))
        });
        let here = self
            .index
            .iter()
            .any(|(c, e)| copy_of_p(c) && e.base == Some(hash))
            || on_disk
                .iter()
                .any(|(c, f)| copy_of_p(c) && f.hash == Some(hash));
        in_swarm || here
    }

    /// `name`, or the first numbered variant of it (`… 2.md`, `… 3.md`) not already holding
    /// other content, here or in the swarm.
    fn free_copy_name(
        &self,
        name: &RelPath,
        hash: Hash,
        remote: &BTreeMap<RelPath, Remote>,
    ) -> anyhow::Result<RelPath> {
        for n in 1..=999 {
            let candidate = if n == 1 {
                name.clone()
            } else {
                numbered_copy(name, n)
            };
            let here = self.read(&candidate)?.map(|b| Hash::new(&b));
            let there = remote.get(&candidate).and_then(|r| r.hash);
            if here.is_none_or(|h| h == hash) && there.is_none_or(|h| h == hash) {
                return Ok(candidate);
            }
        }
        bail!("no free name for another copy of {}", name.as_str())
    }

    /// For a conflict: the remote content and the local content, each with its hash, once both
    /// are to hand and the local file is still what was scanned.
    async fn both_sides(
        &self,
        node: &SwarmNode,
        p: &RelPath,
        local: &Local,
        remote: Option<&Remote>,
    ) -> anyhow::Result<Option<(Bytes, Hash, Bytes, Hash)>> {
        let (Some(theirs), Some(mine)) = (remote.and_then(|r| r.hash), local.hash) else {
            return Ok(None);
        };
        let Some(theirs_bytes) = node.read_blob(theirs).await? else {
            return Ok(None);
        };
        let Some(mine_bytes) = self.read_unchanged(p, mine)? else {
            return Ok(None);
        };
        Ok(Some((theirs_bytes, theirs, mine_bytes, mine)))
    }

    /// Whether `p` on disk is still what the scan saw (`scanned`: its content's hash, None when
    /// absent), so that a save made since is never overwritten or removed.
    fn as_scanned(&self, p: &RelPath, scanned: Option<Hash>) -> anyhow::Result<bool> {
        Ok(self.read(p)?.map(|b| Hash::new(&b)) == scanned)
    }

    /// Moves index entries spelled differently (by case) from the path that owns them onto
    /// that path, and adds the index's own spellings to `names` for paths the swarm lacks.
    fn adopt_spellings(&mut self, names: &mut HashMap<String, RelPath>) {
        let keys: Vec<RelPath> = self.index.keys().cloned().collect();
        for k in keys {
            let owner = names.entry(fold(&k)).or_insert_with(|| k.clone()).clone();
            if owner != k
                && let Some(e) = self.index.remove(&k)
            {
                self.index.entry(owner).or_insert(e);
            }
        }
    }

    /// The scan keyed by the swarm's spelling of each path, remembering the disk's spelling where
    /// it differs. A second spelling of the same path (possible on a case-sensitive disk) stays
    /// local.
    fn respell(&mut self, scan: Scan, names: &HashMap<String, RelPath>) -> Scan {
        let key = |p: &RelPath| names.get(&fold(p)).cloned().unwrap_or_else(|| p.clone());
        let mut seen = scan.seen;
        let mut out = Scan::default();
        self.disk_names.clear();
        for (p, f) in scan.files {
            let k = key(&p);
            if out.files.contains_key(&k) {
                continue;
            }
            if let Some(s) = seen.remove(&p) {
                out.seen.insert(k.clone(), s);
            }
            if k != p {
                self.disk_names.insert(k.clone(), p);
            }
            out.files.insert(k, f);
        }
        out.too_big = scan.too_big.iter().map(key).collect();
        out.unreadable = scan.unreadable.iter().map(key).collect();
        out.ignore = scan.ignore;
        out
    }

    fn entry(&mut self, p: &RelPath) -> &mut Entry {
        self.index.entry(p.clone()).or_default()
    }

    /// Forgets `p` once nothing about it is worth keeping.
    fn tidy(&mut self, p: &RelPath) {
        if self.index.get(p).is_some_and(|e| !e.worth_keeping()) {
            self.index.remove(p);
        }
    }

    fn drop_copy(&mut self, copy: &RelPath) -> anyhow::Result<()> {
        self.remove(copy)?;
        self.index.remove(copy);
        self.copies.remove(copy);
        self.save()
    }

    /// `p` on disk, refusing a way through a link or junction, so nothing is ever read or
    /// written outside the folder. With `create`, missing folders on the way are made; without,
    /// None means one is missing.
    fn inside(&self, p: &RelPath, create: bool) -> anyhow::Result<Option<PathBuf>> {
        let p = self.disk_names.get(p).unwrap_or(p);
        let mut at = self.root.clone();
        let mut parts = p.as_str().split('/').peekable();
        while let Some(part) = parts.next() {
            at.push(part);
            if parts.peek().is_none() {
                break;
            }
            match fs::symlink_metadata(&at) {
                Ok(m) if m.file_type().is_symlink() => {
                    bail!(
                        "{} is inside a link, which hydra doesn't follow",
                        p.as_str()
                    )
                }
                Ok(m) if m.is_dir() => {}
                Ok(_) => bail!("{} is inside something that isn't a folder", p.as_str()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if !create {
                        return Ok(None);
                    }
                    fs::create_dir(&at).with_context(|| format!("creating {}", at.display()))?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(at))
    }

    fn is_file(&self, p: &RelPath) -> bool {
        self.inside(p, false)
            .ok()
            .flatten()
            .and_then(|path| fs::symlink_metadata(path).ok())
            .is_some_and(|m| m.is_file())
    }

    /// Puts `bytes` at `p` through a temp file and a rename, and remembers what was written so
    /// the next scan sees it as the base rather than a local edit.
    fn write(&mut self, p: &RelPath, bytes: &[u8]) -> anyhow::Result<()> {
        let path = self
            .inside(p, true)?
            .context("a folder on the way is missing")?;
        write_via_temp(&path, bytes)?;
        let meta = fs::symlink_metadata(&path)?;
        self.entry(p).seen = Some(Seen {
            size: meta.len(),
            modified: modified(&meta),
            hash: Hash::new(bytes),
        });
        Ok(())
    }

    fn remove(&mut self, p: &RelPath) -> anyhow::Result<()> {
        if let Some(path) = self.inside(p, false)? {
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() => fs::remove_file(&path)?,
                Ok(_) => bail!("{} isn't a file", p.as_str()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(e) = self.index.get_mut(p) {
            e.seen = None;
        }
        Ok(())
    }

    /// The file at `p`, or None if there's none.
    fn read(&self, p: &RelPath) -> anyhow::Result<Option<Bytes>> {
        let Some(path) = self.inside(p, false)? else {
            return Ok(None);
        };
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => bail!("{} isn't a file", p.as_str()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(Bytes::from(bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The file at `p` if it still hashes to `hash`, as scanned. None if it has changed since
    /// (it's picked up next pass), is empty, or has grown past the cap.
    fn read_unchanged(&self, p: &RelPath, hash: Hash) -> anyhow::Result<Option<Bytes>> {
        Ok(self
            .read(p)?
            .filter(|b| !b.is_empty() && b.len() as u64 <= MAX_FILE_BYTES && Hash::new(b) == hash))
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut index = serde_json::Map::new();
        for (p, e) in &self.index {
            index.insert(p.as_str().to_string(), serde_json::to_value(e)?);
        }
        if let Some(folders) = &self.folders {
            let folders = FolderIndex {
                known: folders
                    .iter()
                    .map(|(p, k)| (p.as_str().to_string(), k.clone()))
                    .collect(),
            };
            index.insert(FOLDERS_KEY.to_string(), serde_json::to_value(folders)?);
        }
        let bytes = serde_json::to_vec_pretty(&index)?;
        write_via_temp(&self.state.join(INDEX_FILE), &bytes)
    }
}

fn ignore_path() -> RelPath {
    safe_rel_path(SWARMIGNORE).expect("a safe path")
}

/// The roster's `.swarmignore` as the swarm's version of the file: its text's hash (none when
/// empty, as an empty file says nothing), set when and by whom, from that person's first machine.
fn ignore_remote(roster: &Roster) -> Option<Remote> {
    let set = roster.ignore.as_ref()?;
    let machine = roster
        .members
        .iter()
        .find(|m| m.person == set.by)
        .and_then(|m| m.machines.first())
        .map(|m| m.name.clone())
        .unwrap_or_default();
    Some(Remote {
        hash: (!set.text.is_empty()).then(|| Hash::new(&set.text)),
        at: set.at,
        author: set.by.to_string(),
        machine,
    })
}

/// The text to publish for `.swarmignore`, empty when it is gone or empty, or None (with the
/// reason in `events`) when the roster couldn't take it.
fn ignore_text(bytes: Option<&Bytes>, events: &mut Vec<FolderEvent>) -> Option<String> {
    let Some(bytes) = bytes else {
        return Some(String::new());
    };
    let problem = if bytes.len() > MAX_IGNORE_BYTES {
        format!(
            "{SWARMIGNORE} is over {} KB, so it isn't shared.",
            MAX_IGNORE_BYTES / 1024
        )
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => return Some(text.to_string()),
            Err(_) => format!("{SWARMIGNORE} isn't plain text, so it isn't shared."),
        }
    };
    events.push(FolderEvent::Error(problem));
    None
}

fn held_attention(p: &RelPath, held: Held) -> Attention {
    let path = p.clone();
    match held {
        Held::ReaderEdit => Attention::ReaderEdit { path },
        Held::EditedAfterDelete => Attention::EditedAfterDelete { path },
        Held::NoLongerShared => Attention::NoLongerShared { path },
    }
}

/// The newest of a path's entries: latest time, ties to the higher author, as `files()` picks.
fn newest(entries: &[Remote]) -> Option<&Remote> {
    entries
        .iter()
        .max_by(|a, b| (a.at, &a.author).cmp(&(b.at, &b.author)))
}

fn timestamp(micros: u64) -> Timestamp {
    Timestamp::from_microsecond(i64::try_from(micros).unwrap_or(i64::MAX)).unwrap_or(Timestamp::MAX)
}

/// Whether `base`, the version of `p` this folder took in from `from`'s entry, still counts:
/// that author is a writer, or the roster kept that version of theirs, or a version that counts
/// was made on top of it.
fn counts(
    roster: &Roster,
    history: Option<&FileHistory>,
    p: &RelPath,
    from: &Source,
    base: Hash,
) -> bool {
    let kept = roster
        .retired
        .get(&from.author)
        .and_then(|r| r.files.get(p.as_str()))
        .is_some_and(|v| v.hash == Some(base));
    let built_on = history.is_some_and(|h| {
        h.entries.iter().any(|e| e.hash == Some(base))
            || h.records
                .iter()
                .any(|(_, r)| r.hash == Some(base) || r.ancestors.contains(&base))
    });
    roster.writes(&from.author) || kept || built_on
}

/// `heads` and every version before them that the parent records know of, nearest first.
fn lineage(heads: &[Hash], history: Option<&FileHistory>) -> Vec<Hash> {
    let mut known: HashMap<Hash, Vec<Hash>> = HashMap::new();
    for (_, r) in history.map_or(&[][..], |h| &h.records[..]) {
        if let Some(hash) = r.hash {
            known.entry(hash).or_default().extend(&r.ancestors);
        }
    }
    let mut all = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<Hash> = heads.iter().copied().collect();
    while let Some(h) = queue.pop_front() {
        if !seen.insert(h) {
            continue;
        }
        all.push(h);
        if let Some(earlier) = known.get(&h) {
            queue.extend(earlier);
        }
    }
    // The recent window, plus any older version that is still some author's latest entry, so
    // that entry never looks like an edit made alongside this one.
    let current: HashSet<Hash> = history
        .map(|h| h.entries.iter().filter_map(|e| e.hash).collect())
        .unwrap_or_default();
    all.iter()
        .enumerate()
        .filter(|(i, h)| *i < MAX_ANCESTORS || current.contains(*h))
        .map(|(_, h)| *h)
        .collect()
}

/// `p` folded for comparing paths the way Windows and macOS do.
fn fold(p: &RelPath) -> String {
    p.as_str().to_lowercase()
}

/// For each case-folded path in the swarm, the key that owns it (the one whose newest entry is
/// newest, live entries first), and the other keys still holding content, with their owner.
fn case_owners(
    history: &BTreeMap<RelPath, FileHistory>,
) -> (HashMap<String, RelPath>, Vec<(RelPath, RelPath)>) {
    let mut groups: HashMap<String, Vec<&RelPath>> = HashMap::new();
    for p in history.keys() {
        groups.entry(fold(p)).or_default().push(p);
    }
    let rank = |p: &RelPath| {
        newest(&history[p].entries).map(|e| (e.hash.is_some(), e.at, e.author.clone()))
    };
    let mut names = HashMap::new();
    let mut shadowed = Vec::new();
    for (folded, keys) in groups {
        let Some(owner) = keys.iter().max_by_key(|p| rank(p)).copied() else {
            continue;
        };
        for k in keys.iter().filter(|k| **k != owner) {
            if rank(k).is_some_and(|r| r.0) {
                shadowed.push((owner.clone(), (*k).clone()));
            }
        }
        names.insert(folded, owner.clone());
    }
    shadowed.sort();
    (names, shadowed)
}

fn can_write(node: &SwarmNode) -> anyhow::Result<()> {
    if !node.can_write() {
        bail!("This machine can only read this swarm.");
    }
    Ok(())
}

/// The entries of a path, other than the newest, holding content the newest wasn't made on top
/// of. None until every entry's author's parent record for it has arrived and matches it.
fn unseen_edits(history: &FileHistory) -> Option<Vec<&Remote>> {
    let record_of = |e: &Remote| {
        history
            .records
            .iter()
            .find(|(author, r)| *author == e.author && r.hash == e.hash)
            .map(|(_, r)| r)
    };
    if history.entries.iter().any(|e| record_of(e).is_none()) {
        return None;
    }
    let top = newest(&history.entries)?;
    let heads: Vec<Hash> = top
        .hash
        .into_iter()
        .chain(record_of(top)?.ancestors.iter().copied())
        .collect();
    let seen: HashSet<Hash> = lineage(&heads, Some(history)).into_iter().collect();
    Some(
        history
            .entries
            .iter()
            .filter(|e| e.author != top.author && e.hash.is_some_and(|h| !seen.contains(&h)))
            .collect(),
    )
}

async fn node_history(node: &SwarmNode) -> anyhow::Result<BTreeMap<RelPath, FileHistory>> {
    let roster = node
        .roster()
        .await?
        .context("This swarm's member list hasn't arrived yet.")?;
    swarm_history(node, &roster).await
}

/// Every counting entry per path, at the path the folder moves put it.
async fn swarm_history(
    node: &SwarmNode,
    roster: &Roster,
) -> anyhow::Result<BTreeMap<RelPath, FileHistory>> {
    let moves = Moves::from_moves(&node.moves(roster).await?);
    Ok(mapped(&node.file_entries(roster).await?, &moves, &|_| true))
}

/// `raw` with each entry, and its author's parent record, at the path it belongs at now that the
/// folder moves are applied (`Moves::map`), keeping only paths that `syncs`. A path an entry was
/// made at before a rename reached its author is read at the renamed path, so work done under
/// the old name lands in the renamed folder as the same file.
fn mapped(
    raw: &BTreeMap<RelPath, FileHistory>,
    moves: &Moves,
    syncs: &dyn Fn(&RelPath) -> bool,
) -> BTreeMap<RelPath, FileHistory> {
    let mut out: BTreeMap<RelPath, FileHistory> = BTreeMap::new();
    for (p, h) in raw {
        for e in &h.entries {
            out.entry(moves.map(p, e.at))
                .or_default()
                .entries
                .push(e.clone());
        }
        // A parent record goes where its author's entry went. One whose entry hasn't arrived yet
        // follows every move, as the oldest entry would.
        for (author, r) in &h.records {
            let at = h
                .entries
                .iter()
                .filter(|e| &e.author == author)
                .map(|e| e.at)
                .max()
                .unwrap_or(0);
            out.entry(moves.map(p, at))
                .or_default()
                .records
                .push((author.clone(), r.clone()));
        }
    }
    out.retain(|p, _| syncs(p));
    for h in out.values_mut() {
        h.entries
            .sort_by(|a, b| (a.at, &a.author).cmp(&(b.at, &b.author)));
    }
    out
}

/// What is at `dst` other than the folder `src` itself, as the disk spells it: an entry of that
/// exact name, or else, on a disk that ignores case, one differing only by case. On a disk that
/// minds case such a folder is another folder, and merging into it would leave the files in it
/// under a spelling the index doesn't have, to be read as deleted.
fn other_entry(dst: &Path, src: &Path) -> anyhow::Result<Option<PathBuf>> {
    let (Some(dir), Some(name)) = (dst.parent(), dst.file_name()) else {
        return Ok(None);
    };
    let name = name.to_string_lossy();
    let entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(d) => d.filter_map(|e| Some(e.ok()?.path())).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let named = |p: &&PathBuf, folded: bool| {
        p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            if folded {
                n.to_lowercase() == name.to_lowercase()
            } else {
                n == name
            }
        })
    };
    // Whether the disk takes `dst` as `p`: it finds something under the name as given, and it is
    // `p` itself.
    let same_name = |p: &&PathBuf| {
        fs::symlink_metadata(dst).is_ok()
            && match (fs::canonicalize(dst), fs::canonicalize(p)) {
                (Ok(a), Ok(b)) => a == b,
                _ => true,
            }
    };
    let found = entries.iter().find(|p| named(p, false)).or_else(|| {
        entries
            .iter()
            .find(|p| named(p, true))
            .filter(|p| same_name(p))
    });
    // The folder itself, spelled differently: a change of case, not another folder.
    let itself = |p: &PathBuf| {
        p == src
            || fs::canonicalize(p)
                .ok()
                .is_some_and(|c| fs::canonicalize(src).ok() == Some(c))
    };
    Ok(found.filter(|p| !itself(p)).cloned())
}

/// Renames the folder `src` to `dst`. A change of case alone goes through a temporary name when
/// the disk won't take it directly, and is put back if the second step fails, so the folder is
/// never left under the temporary name.
fn rename_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::rename(src, dst).with_context(|| format!("renaming {}", src.display()))?;
    let (Some(dir), Some(name)) = (dst.parent(), dst.file_name()) else {
        return Ok(());
    };
    let spelled = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .any(|e| e.file_name() == name);
    if spelled {
        return Ok(());
    }
    let tmp = dir.join(format!("{}{RECASE_SUFFIX}", name.to_string_lossy()));
    fs::rename(dst, &tmp).with_context(|| format!("renaming {}", src.display()))?;
    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::rename(&tmp, src);
        return Err(e).with_context(|| format!("renaming {}", src.display()));
    }
    Ok(())
}

fn check_copy(path: &RelPath, copy: &RelPath) -> anyhow::Result<()> {
    if conflict_original(copy).as_ref() != Some(path) {
        bail!(
            "{} isn't a conflict copy of {}.",
            copy.as_str(),
            path.as_str()
        );
    }
    Ok(())
}

async fn types(node: &SwarmNode) -> anyhow::Result<Vec<String>> {
    Ok(node
        .roster()
        .await?
        .context("This swarm's member list hasn't arrived yet.")?
        .types)
}

/// This machine's name in the swarm: the roster's entry for this endpoint, else `me`'s.
fn machine_name(roster: &Roster, me: &Member, endpoint: &str) -> String {
    let machines: Vec<_> = roster
        .members
        .iter()
        .filter(|m| m.person == me.person)
        .chain(std::iter::once(me))
        .flat_map(|m| &m.machines)
        .collect();
    machines
        .iter()
        .find(|m| m.endpoint == endpoint)
        .or(machines.first())
        .map(|m| m.name.clone())
        .unwrap_or_default()
}

fn modified(meta: &Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

/// The file index and the folder index (None when it has never been saved) from `index.json`.
#[allow(clippy::type_complexity)]
fn load_index(
    file: &Path,
) -> anyhow::Result<(
    BTreeMap<RelPath, Entry>,
    Option<BTreeMap<RelPath, KnownFolder>>,
)> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((BTreeMap::new(), None)),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let damaged = || format!("the folder index {} is damaged", file.display());
    let mut raw: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).with_context(damaged)?;
    let folders = match raw.remove(FOLDERS_KEY) {
        Some(v) => {
            let folders: FolderIndex = serde_json::from_value(v).with_context(damaged)?;
            Some(
                folders
                    .known
                    .into_iter()
                    .filter_map(|(p, k)| Some((safe_rel_path(&p)?, k)))
                    .collect(),
            )
        }
        None => None,
    };
    let mut files = BTreeMap::new();
    for (p, v) in raw {
        let entry: Entry = serde_json::from_value(v).with_context(damaged)?;
        if let Some(p) = safe_rel_path(&p) {
            files.insert(p, entry);
        }
    }
    Ok((files, folders))
}

/// Writes `bytes` to a temp file beside `path`, then renames it over `path`, so nobody ever sees
/// a half-written file.
fn write_via_temp(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("a file needs a folder")?;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    let tmp = dir.join(format!(".hydra-{}{TMP_SUFFIX}", to_hex(&nonce)));
    let written = (|| {
        let mut f = File::create_new(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.with_context(|| format!("writing {}", path.display()))
}

/// Every file under `root` that syncs, never following links or junctions, and never going into
/// a folder none of whose files can sync (one starting with a dot, or one `.swarmignore` takes
/// out). The root `.swarmignore` is read on its own, for the pass to sync through the roster.
fn scan(root: &Path, rules: &SyncRules, seen: &HashMap<RelPath, Seen>) -> io::Result<Scan> {
    let mut out = Scan::default();
    // Folders holding something other than folders the scan went into, so not empty.
    let mut cluttered: BTreeSet<RelPath> = BTreeSet::new();
    let mut dirs: Vec<(PathBuf, Option<RelPath>)> = vec![(root.to_path_buf(), None)];
    while let Some((dir, here)) = dirs.pop() {
        let mut clutter = |here: &Option<RelPath>| {
            if let Some(h) = here {
                cluttered.insert(h.clone());
            }
        };
        // Records a synced file's name in the folder it is in, for spotting a rename by names.
        let synced = |out: &mut Scan, name: &str| {
            if let Some(f) = here.as_ref().and_then(|h| out.folders.get_mut(h)) {
                f.synced_files.insert(name.to_string());
            }
        };
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_symlink() {
                clutter(&here);
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                match rel_from_disk(root, &path) {
                    Some(p) if rules.folder_may_sync(&p) => {
                        if let Some(f) = here.as_ref().and_then(|h| out.folders.get_mut(h)) {
                            f.subfolders.insert(name);
                        }
                        out.folders.insert(
                            p.clone(),
                            DiskFolder {
                                empty: false,
                                synced_files: BTreeSet::new(),
                                subfolders: BTreeSet::new(),
                            },
                        );
                        dirs.push((path, Some(p)));
                    }
                    Some(_) => clutter(&here),
                    None => {
                        clutter(&here);
                        dirs.push((path, None));
                    }
                }
                continue;
            }
            clutter(&here);
            if !kind.is_file() || name.ends_with(TMP_SUFFIX) {
                continue;
            }
            let Some(p) = rel_from_disk(root, &path) else {
                continue;
            };
            if p.as_str() == SWARMIGNORE {
                out.ignore = read_ignore(&path, &entry);
                continue;
            }
            if !rules.file_syncs(&p) {
                continue;
            }
            synced(&mut out, &name);
            let Ok(meta) = entry.metadata() else {
                out.unreadable.insert(p);
                continue;
            };
            let (size, modified) = (meta.len(), modified(&meta));
            if size > MAX_FILE_BYTES {
                out.too_big.insert(p);
                continue;
            }
            let hash = match seen.get(&p) {
                _ if size == 0 => None,
                Some(s) if s.size == size && s.modified == modified => Some(s.hash),
                _ => match fs::read(&path) {
                    Ok(bytes) if bytes.is_empty() => None,
                    Ok(bytes) if bytes.len() as u64 > MAX_FILE_BYTES => {
                        out.too_big.insert(p);
                        continue;
                    }
                    Ok(bytes) => {
                        let hash = Hash::new(&bytes);
                        let size = bytes.len() as u64;
                        out.seen.insert(
                            p.clone(),
                            Seen {
                                size,
                                modified,
                                hash,
                            },
                        );
                        Some(hash)
                    }
                    Err(_) => {
                        out.unreadable.insert(p);
                        continue;
                    }
                },
            };
            out.files.insert(p, OnDisk { modified, hash });
        }
    }
    // Empty means nothing but empty folders, so the deepest are settled first.
    let mut deepest: Vec<RelPath> = out.folders.keys().cloned().collect();
    deepest.sort_by_key(|p| std::cmp::Reverse(p.as_str().matches('/').count()));
    for p in deepest {
        let empty = !cluttered.contains(&p)
            && out.folders[&p].subfolders.iter().all(|n| {
                safe_rel_path(&format!("{}/{n}", p.as_str()))
                    .and_then(|c| out.folders.get(&c))
                    .is_some_and(|f| f.empty)
            });
        if let Some(f) = out.folders.get_mut(&p) {
            f.empty = empty;
        }
    }
    Ok(out)
}

fn read_ignore(path: &Path, entry: &fs::DirEntry) -> IgnoreFile {
    let Ok(meta) = entry.metadata() else {
        return IgnoreFile::Unreadable;
    };
    // Far past what the roster keeps; never read whole.
    if meta.len() > MAX_FILE_BYTES {
        return IgnoreFile::Unreadable;
    }
    match fs::read(path) {
        Ok(bytes) => IgnoreFile::Text {
            bytes: Bytes::from(bytes),
            modified: modified(&meta),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => IgnoreFile::Absent,
        Err(_) => IgnoreFile::Unreadable,
    }
}

/// The watcher's thread: collects changed paths, and once they have been quiet for `DEBOUNCE`
/// and their sizes have settled, sends one signal.
fn watch_folder(root: &Path, tx: &mpsc::Sender<()>) {
    let (raw_tx, raw) = std::sync::mpsc::channel::<PathBuf>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                let _ = raw_tx.send(path);
            }
        }
    });
    let Ok(mut watcher) = watcher else {
        return;
    };
    if watcher.watch(root, RecursiveMode::Recursive).is_err() {
        return;
    }
    let mut changed: HashSet<PathBuf> = HashSet::new();
    let mut last: Option<Instant> = None;
    while !tx.is_closed() {
        let wait = last.map_or(IDLE, |t| DEBOUNCE.saturating_sub(t.elapsed()));
        match raw.recv_timeout(wait) {
            Ok(path) => {
                changed.insert(path);
                last = Some(Instant::now());
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                if last.is_none_or(|t| t.elapsed() < DEBOUNCE) {
                    continue;
                }
                if !sizes_settled(&changed) {
                    last = Some(Instant::now());
                    continue;
                }
                changed.clear();
                last = None;
                // A full channel already holds a signal, which covers this one too.
                let _ = tx.try_send(());
            }
        }
    }
}

fn sizes_settled(paths: &HashSet<PathBuf>) -> bool {
    let sizes = || -> Vec<Option<u64>> {
        paths
            .iter()
            .map(|p| fs::metadata(p).ok().map(|m| m.len()))
            .collect()
    };
    let before = sizes();
    std::thread::sleep(SETTLE);
    before == sizes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        safe_rel_path(s).unwrap()
    }

    #[test]
    fn an_older_index_has_no_folder_index_and_an_older_hydra_can_read_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(INDEX_FILE);
        let base = Hash::new(b"one");
        // As an older hydra wrote it: paths only.
        let older = serde_json::json!({ "notes/a.md": { "base": base } });
        fs::write(&file, serde_json::to_vec(&older).unwrap()).unwrap();
        let (files, folders) = load_index(&file).unwrap();
        assert_eq!(files[&rel("notes/a.md")].base, Some(base));
        assert!(folders.is_none());

        let mut f = Folder::new(dir.path().join("root"), dir.path().to_path_buf(), false).unwrap();
        f.folders = Some(BTreeMap::new());
        f.save().unwrap();
        let (_, folders) = load_index(&file).unwrap();
        assert_eq!(
            folders,
            Some(BTreeMap::new()),
            "saved and empty, not never saved"
        );

        f.folders = Some(BTreeMap::from([(
            rel("notes"),
            KnownFolder {
                applied_at: 5,
                notified_at: 0,
            },
        )]));
        f.save().unwrap();
        // How an older hydra reads it: every key as a file entry, keeping only safe paths.
        let raw: BTreeMap<String, Entry> =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let paths: Vec<&str> = raw
            .keys()
            .filter(|p| safe_rel_path(p).is_some())
            .map(|p| p.as_str())
            .collect();
        assert_eq!(paths, vec!["notes/a.md"]);
        let (files, folders) = load_index(&file).unwrap();
        assert_eq!(files[&rel("notes/a.md")].base, Some(base));
        assert_eq!(folders.unwrap()[&rel("notes")].applied_at, 5);
    }

    #[test]
    fn a_folder_named_like_an_index_field_leaves_the_index_readable_by_an_older_hydra() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(INDEX_FILE);
        let mut f = Folder::new(dir.path().join("root"), dir.path().to_path_buf(), false).unwrap();
        let names = ["base", "from", "held", "seen", "published"];
        f.folders = Some(
            names
                .iter()
                .map(|n| {
                    let k = KnownFolder {
                        applied_at: 7,
                        notified_at: 0,
                    };
                    (rel(n), k)
                })
                .collect(),
        );
        f.save().unwrap();
        // How an older hydra reads it: every key, whatever it is, as a file entry.
        let older: Result<BTreeMap<String, Entry>, _> =
            serde_json::from_slice(&fs::read(&file).unwrap());
        assert!(older.is_ok(), "{older:?}");
        let (_, folders) = load_index(&file).unwrap();
        let folders = folders.unwrap();
        for n in names {
            assert_eq!(folders[&rel(n)].applied_at, 7);
        }
    }

    /// Makes `dir` mind case where the disk allows it (Windows, per folder). Elsewhere it already
    /// does (Linux) or can't (macOS), and the test checks whichever it is.
    fn mind_case(dir: &Path) {
        if cfg!(windows) {
            let _ = std::process::Command::new("fsutil")
                .args(["file", "setCaseSensitiveInfo"])
                .arg(dir)
                .arg("enable")
                .output();
        }
    }

    #[test]
    fn a_folder_differing_only_by_case_is_another_folder_on_a_disk_that_minds_case() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        mind_case(&root);
        fs::create_dir(root.join("Docs")).unwrap();
        fs::create_dir(root.join("x")).unwrap();
        let found = other_entry(&root.join("docs"), &root.join("x")).unwrap();
        if root.join("docs").exists() {
            // The disk ignores case: `docs` is `Docs`.
            assert_eq!(found, Some(root.join("Docs")));
        } else {
            assert_eq!(found, None, "Docs is another folder here, not docs");
            // And the exact name wins over one differing by case.
            fs::create_dir(root.join("docs")).unwrap();
            let found = other_entry(&root.join("docs"), &root.join("x")).unwrap();
            assert_eq!(found, Some(root.join("docs")));
        }
    }

    #[test]
    fn a_folder_differing_only_by_case_is_the_same_folder_on_a_disk_that_ignores_case() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("Docs")).unwrap();
        fs::create_dir(dir.path().join("x")).unwrap();
        let found = other_entry(&dir.path().join("docs"), &dir.path().join("x")).unwrap();
        let expected = dir
            .path()
            .join("docs")
            .exists()
            .then(|| dir.path().join("Docs"));
        assert_eq!(found, expected);
        // A change of case of the folder itself is never another folder.
        let found = other_entry(&dir.path().join("docs"), &dir.path().join("Docs")).unwrap();
        assert_eq!(found, None);
    }
}
