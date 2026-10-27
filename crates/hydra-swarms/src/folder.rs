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
use crate::node::{FileHistory, ParentRecord, SwarmNode, now_micros};
use crate::paths::{
    MAX_FILE_BYTES, RelPath, allowed, conflict_name, conflict_original, is_conflict_copy,
    numbered_copy, rel_from_disk, safe_rel_path,
};
use crate::roster::{Member, Role, Roster};

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
}

/// An allowed file in the folder. `hash` is None for an empty file.
struct OnDisk {
    modified: u64,
    hash: Option<Hash>,
}

#[derive(Default)]
struct Scan {
    files: BTreeMap<RelPath, OnDisk>,
    /// Files hashed in this scan.
    seen: BTreeMap<RelPath, Seen>,
    too_big: BTreeSet<RelPath>,
    /// Allowed files that couldn't be read this time; left alone until they can.
    unreadable: BTreeSet<RelPath>,
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
        let index = load_index(&state.join(INDEX_FILE))?;
        Ok(Folder {
            root,
            state,
            index,
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
        let types = roster.types.clone();
        let scan = {
            let root = self.root.clone();
            let seen: HashMap<RelPath, Seen> = self
                .index
                .iter()
                .filter_map(|(p, e)| Some((self.disk_names.get(p).unwrap_or(p).clone(), e.seen?)))
                .collect();
            let types = types.clone();
            tokio::task::spawn_blocking(move || scan(&root, &types, &seen)).await??
        };
        let history: BTreeMap<RelPath, FileHistory> = node
            .file_entries(roster)
            .await?
            .into_iter()
            .filter(|(p, _)| allowed(p, 0, &types))
            .collect();
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
        let writer = roster
            .role_of(&me.person)
            .is_some_and(|r| r >= Role::Writer)
            && node.can_write();
        let author = node.author();
        let machine = machine_name(roster, me, &node.endpoint_id());
        let tz = TimeZone::system();
        let pass = Pass {
            node,
            history: &history,
            remote: &remote,
            machine: &machine,
        };

        let mut paths: BTreeSet<RelPath> =
            scan.files.keys().chain(remote.keys()).cloned().collect();
        // Synced paths of a type no longer on the list are left as they are, not deleted.
        paths.extend(
            self.index
                .iter()
                .filter(|(p, e)| (e.base.is_some() || e.held.is_some()) && allowed(p, 0, &types))
                .map(|(p, _)| p.clone()),
        );
        let mut events = Vec::new();
        let mut last: Option<(String, u64)> = None;
        for p in paths {
            if scan.too_big.contains(&p) || scan.unreadable.contains(&p) {
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
            for (p, edit) in unseen.chain(cased).collect::<Vec<_>>() {
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
        self.index
            .retain(|_, e| e.base.is_some() || e.held.is_some());
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
        let remote = node.files(&roster).await?.remove(path);
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
        out
    }

    fn entry(&mut self, p: &RelPath) -> &mut Entry {
        self.index.entry(p.clone()).or_default()
    }

    /// Forgets `p` once nothing about it is worth keeping.
    fn tidy(&mut self, p: &RelPath) {
        if self
            .index
            .get(p)
            .is_some_and(|e| e.base.is_none() && e.held.is_none())
        {
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
        let index: BTreeMap<&str, &Entry> =
            self.index.iter().map(|(p, e)| (p.as_str(), e)).collect();
        let bytes = serde_json::to_vec_pretty(&index)?;
        write_via_temp(&self.state.join(INDEX_FILE), &bytes)
    }
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
    node.file_entries(&roster).await
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

fn load_index(file: &Path) -> anyhow::Result<BTreeMap<RelPath, Entry>> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let raw: BTreeMap<String, Entry> = serde_json::from_slice(&bytes)
        .with_context(|| format!("the folder index {} is damaged", file.display()))?;
    Ok(raw
        .into_iter()
        .filter_map(|(p, e)| Some((safe_rel_path(&p)?, e)))
        .collect())
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

/// Every allowed file under `root`, never following links or junctions.
fn scan(root: &Path, types: &[String], seen: &HashMap<RelPath, Seen>) -> io::Result<Scan> {
    let mut out = Scan::default();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                dirs.push(path);
                continue;
            }
            if !kind.is_file() || entry.file_name().to_string_lossy().ends_with(TMP_SUFFIX) {
                continue;
            }
            let Some(p) = rel_from_disk(root, &path) else {
                continue;
            };
            if !allowed(&p, 0, types) {
                continue;
            }
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
    Ok(out)
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
