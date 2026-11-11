# Swarm Folders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A swarm syncs its folder layout, not only its files. Empty folders appear everywhere, a rename renames, a delete keeps unseen work, folders starting with `.` never sync, and an owner-only `.swarmignore` says what else doesn't.

**Architecture:**
- **Rules:** a pure `SyncRules` value (file types, the dot rule, `.swarmignore`) decides what syncs. It replaces the bare `allowed(p, 0, types)` calls.
- **`.swarmignore`:** stored as an owner-signed roster change (`Change::SetIgnore`), so owner-at-the-time enforcement and older-hydra safety come free. On disk it is a pseudo-file that goes through `decide`.
- **Folders:** folders get their own records under `folders/` in the swarm document. A pure `layout` module turns the folder index, the scanned folders and the records into a plan: renames on disk, creates, removals and records to publish, plus a `Moves` map.
- **Moves:** the `Moves` map rewrites file history from old paths to new ones before the existing per-file pass runs. A rename therefore publishes no file entries, and is never an edit.

**Tech Stack:** Rust, iroh-docs, the `ignore` crate (already in Cargo.lock at 0.4.33), nextest.

**Spec:** `docs/superpowers/specs/2026-11-11-swarm-folders-design.md`, which builds on `docs/superpowers/specs/2026-10-27-swarms-design.md` §5.

## Global Constraints

- Never stop hydraui by name (no `taskkill /IM hydraui.exe`, `Stop-Process -Name hydraui`, `pkill hydraui`).
- Work on `feature/swarm-folders` in `E:\AI\davojc\hydra-worktrees\swarm-folders`. Commit there; never push.
- Never delete anything that isn't synced. No rule removes a file that doesn't sync, or a folder that isn't empty.
- Older hydras must keep working: they ignore `folders/` records and `Change::SetIgnore` (the log already skips changes it can't parse, `node.rs` `changes()`).
- A folder whose name starts with `.` never syncs, and nothing in it does. Files whose own name starts with `.` aren't affected. `.swarmignore` at the root always syncs.
- Follow the crate's habits: comments that say why, tests named as sentences, plain sentences for anything a person reads.
- Verify every task with `cargo nextest run --profile ci -p hydra-swarms`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`.

## Rulings made while planning (spec §5.1 and §6)

- **Ruling A:** a folder holding a synced file is live because of that file. When a writer's machine finds a known folder on disk that is live only that way, it writes a `Present` record for it, timed at the newest file entry beneath it (spec §5.1, "catch-up record"). Folders that already exist get recorded the same way on upgrade.
- **Ruling B:** a rename writes one `MovedTo` record at the old path, plus a `Present` record at the new path. It writes no file entries. Every machine maps file entries under the old path that are older than the move onto the new path. This is how "a rename is never an edit" holds, and how work done under the old name lands in the new folder.
- **Ruling C:** an older hydra doesn't see the mapping. It keeps the files at their old paths and still shows the old folder. Spec §6 accepts this.

## Review Focus

1. **A rename differing only by case** (`Notes` → `notes`) on Windows, where both are the same folder on disk: it must be recorded as a rename and must not delete the files.
2. **A rename that lands while a file in that folder is being saved** on the receiving machine. The disk rename has to be retried, never half-done or forced.
3. **A machine offline through a rename and then a further rename of the same folder,** that comes back with edits under the oldest name. Its edits must land at the newest name.
4. **Deleting a deep tree:** folders are removed deepest first. A folder holding a held file or a conflict copy isn't empty, so it stays.
5. **Two machines each making the same new folder** at the same time, spelled `Drafts` and `drafts`: the result is one folder, not a loop of renames.

---

### Task 1: The rules — dot folders, `.swarmignore` patterns and the roster setting

**Files:**
- Create: `crates/hydra-swarms/src/rules.rs` (`SyncRules`)
- Modify: `crates/hydra-swarms/src/lib.rs` (module)
- Modify: `crates/hydra-swarms/src/roster.rs` (`Change::SetIgnore`, `Roster.ignore`)
- Modify: `crates/hydra-swarms/Cargo.toml` (add `ignore = "0.4"`; add it to `[workspace.dependencies]` if that is how other crates are pinned)

**Interfaces:**
- Produces:
  - `pub const SWARMIGNORE: &str = ".swarmignore";` in `rules.rs`.
  - `pub struct IgnoreSetting { pub text: String, pub at: u64, pub by: PersonId }` (serde, Clone, PartialEq, Eq, Debug).
  - `Change::SetIgnore { text: String }`.
  - `Roster.ignore: Option<IgnoreSetting>` (`#[serde(default)]`), set when an owner's change folds. The latest `at` wins, ties broken by `by`. A text over 64 KiB is ignored.
  - `pub struct SyncRules` with `pub fn new(types: &[String], ignore: Option<&str>) -> SyncRules`, `pub fn file_syncs(&self, p: &RelPath) -> bool`, `pub fn folder_may_sync(&self, p: &RelPath) -> bool` and `pub fn under_dot_folder(p: &RelPath) -> bool`.
- Consumes: `paths::allowed` and `RelPath`.

The rules:
- **`under_dot_folder(p)`** is true when any folder component of `p` starts with `.`. The last component counts only for a folder.
- **`file_syncs(p)`:**
  - true for exactly `.swarmignore` at the root;
  - otherwise false when `under_dot_folder`;
  - otherwise `allowed(p, 0, types)` and not ignored. Use `ignore::gitignore::GitignoreBuilder` rooted at a fixed dummy root, and `matched_path_or_any_parents(p, false).is_ignore()`.
- **`folder_may_sync(p)`:** false when the folder's own name or any parent starts with `.`, or when the pattern matches it as a directory.
- A pattern that doesn't parse is skipped. The rest still apply.

- [ ] **Step 1: Write failing unit tests in `rules.rs`.**
  - `a_folder_starting_with_a_dot_never_syncs`: `.github/a.md` is false; `.github` as a folder is false; `docs/.hidden/a.md` is false.
  - `a_file_starting_with_a_dot_is_not_affected`: with types `[".md"]`, `docs/.notes.md` is true.
  - `swarmignore_at_the_root_always_syncs`: true even with types `[".md"]` and ignore text `*`. `docs/.swarmignore` is false.
  - `swarmignore_patterns_take_files_and_folders_out`: ignore `build/\n*.tmp.md\n`. Then `build/a.md` is false, `build` as a folder is false, `a.tmp.md` is false and `a.md` is true.
  - `a_bang_line_brings_back_only_what_a_pattern_took_out`: ignore `drafts/**\n!drafts/keep.md\n.github/\n!.github/**`. Then `drafts/keep.md` is true, `drafts/x.md` is false, and `.github/a.md` is false (the dot rule wins). With ignore `!a.py` and types `[".md"]`, `a.py` is false.
- [ ] **Step 2: Write failing roster tests in `roster.rs`.**
  - `an_owners_ignore_change_sets_the_ignore`;
  - `a_writers_ignore_change_is_dropped`;
  - `the_later_ignore_change_wins`;
  - `an_oversize_ignore_change_is_dropped`.
  
  Use `fold` and `sign` the way the `SetTypes` tests do.
- [ ] **Step 3: Run them and watch them fail:** `cargo nextest run -p hydra-swarms --lib rules roster`. Expected: a compile error, because `rules` doesn't exist.
- [ ] **Step 4: Implement.** In `fold`, `Change::SetIgnore` mirrors `Change::SetTypes`'s `by_owner` check.
- [ ] **Step 5: Run them; they pass.** Then run the full verification from Global Constraints.
- [ ] **Step 6: Commit:** `hydra-swarms: sync rules with the dot rule and .swarmignore, and the owner-only ignore setting`.

### Task 2: The folder pass obeys the rules, and `.swarmignore` syncs from owners only

**Files:**
- Modify: `crates/hydra-swarms/src/folder.rs`
- Modify: `crates/hydra-swarms/src/service.rs` (`folder_events`)
- Test: `crates/hydra-swarms/tests/folders.rs`

**Interfaces:**
- Consumes: `SyncRules`, `SWARMIGNORE` and `Roster.ignore` from Task 1, and `Service::owner_change` (`service.rs` line 2355).
- Produces:
  - `FolderEvent::PublishIgnore(String)`: the text an owner saved in `.swarmignore` here, for the service to sign as `Change::SetIgnore`.
  - In `tests/folders.rs`, the `Side` helper signs and writes `Change::SetIgnore` itself when it sees this event, so the tests need no service.

Changes to the pass:
- `reconcile` builds `SyncRules::new(&roster.types, roster.ignore.as_ref().map(|i| i.text.as_str()))`, and uses `rules.file_syncs` wherever it uses `allowed(p, 0, &types)` today: the history filter, the index paths, and the `scan` call.
- `scan` takes `&SyncRules`. It doesn't descend into a folder whose name starts with `.`, apart from reading `.swarmignore` at the root.
- Paths that stop syncing are left alone and never deleted, as types taken off the list already are. Entries for them are skipped.
- **`.swarmignore` as a pseudo-file**, decided by `decide` at path `.swarmignore`:
  - `remote` is built from `roster.ignore`: `hash = Hash::new(text)`, `at`, `author = by` as a string, and `machine` = that person's first machine name or empty.
  - `writer` is whether this machine's person is an owner.
  - `local` is the file on disk.
  - `Action::Write` writes the roster's text. `Action::Publish` pushes `FolderEvent::PublishIgnore(local text)`, or an empty text if the file is gone, and sets the base to the local hash.
  - `Hold(ReaderEdit)` holds a writer's edit: it uses the existing `Held::ReaderEdit` attention and notice.
  - Conflict actions write their copies as usual. The copy's name has no type, so it never syncs, which gives the spec's "the conflict copy has no effect".
  - `use_swarm_version(".swarmignore")` writes the roster's text rather than reading a blob.
- `service.rs` `folder_events` handles `PublishIgnore(text)` with `owner_change(Change::SetIgnore { text }, …)`. On failure it is shown the way `FolderEvent::Error` is.

- [ ] **Step 1: Write failing two-machine tests in `tests/folders.rs`.**
  - `a_file_in_a_dot_folder_stays_local`: the owner writes `.github/a.md` and `a.md`. The writer gets `a.md` and never `.github/a.md`. Check after `a.md` arrives.
  - `a_dot_folder_file_synced_before_upgrade_stays_on_both_and_stops_syncing`:
    - Put an entry for `.github/a.md` straight into the document with `node.put_file`, and have both folders hold it.
    - Reconcile both. The file stays on both.
    - The owner edits it. The writer still has the old text after the owner's next pass and `hears` on another file.
  - `an_owners_swarmignore_line_stops_matching_files_and_deletes_nothing`:
    - `drafts/x.md` is synced.
    - The owner writes `.swarmignore` with `drafts/`, and the writer receives `.swarmignore`.
    - The owner edits `drafts/x.md`, and the writer keeps its old text.
    - Both still have the file.
  - `removing_the_line_brings_the_files_back`: continue the test above. The owner empties `.swarmignore`. A new file `drafts/y.md` made on the owner reaches the writer.
  - `a_writers_swarmignore_edit_is_held_and_has_no_effect`:
    - The writer edits `.swarmignore`, and gets a `ReaderEdit` attention for `.swarmignore`.
    - The owner's `.swarmignore` is unchanged.
    - The owner's new `drafts/z.md` doesn't reach the writer while the owner's ignore still lists `drafts/`.
    - `use_swarm_version` restores the owner's text.
- [ ] **Step 2: Run them and watch them fail:** `cargo nextest run --profile ci -p hydra-swarms --test folders`.
- [ ] **Step 3: Implement** the pass changes, the pseudo-file and the service handling.
- [ ] **Step 4: Run them; they pass.** Then run the full verification.
- [ ] **Step 5: Commit:** `hydra-swarms: the folder pass obeys the dot rule and an owner's .swarmignore`.

### Task 3: Folder records in the swarm document

**Files:**
- Modify: `crates/hydra-swarms/src/paths.rs` (`FOLDERS_PREFIX`, `folder_key`, `path_of_folder_key`)
- Modify: `crates/hydra-swarms/src/node.rs` (`FolderRecord`, `put_folder`, `folder_records`)

**Interfaces:**
- Produces:
  - `pub const FOLDERS_PREFIX: &[u8] = b"folders/";`, with `pub fn folder_key(p: &RelPath) -> Vec<u8>` and `pub fn path_of_folder_key(k: &[u8]) -> Option<RelPath>`, shaped like `file_key` and `path_of_key`.
  - `#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)] #[serde(tag = "state", rename_all = "snake_case")] pub enum FolderState { Present, Deleted, MovedTo { to: String } }`.
  - `pub struct FolderRecord { pub state: FolderState, pub at: u64 }`, where `at` is unix microseconds chosen by the writer (Ruling A), not the entry's timestamp.
  - `pub struct FolderVersion { pub record: FolderRecord, pub author: String, pub machine: String }`.
  - `SwarmNode::put_folder(&self, p: &RelPath, record: &FolderRecord) -> anyhow::Result<()>`, a JSON blob of at most 64 KiB.
  - `SwarmNode::folder_records(&self, roster: &Roster) -> anyhow::Result<BTreeMap<RelPath, Vec<FolderVersion>>>`:
    - Includes every author's newest record per path, counting authors who `roster.writes` or are in `roster.retired`, as claims' reopens count.
    - Leaves out unsafe paths and a `MovedTo` whose `to` isn't a safe path.
    - Sorts each list oldest first by `(at, author)`.

- [ ] **Step 1: Write failing tests.**
  - `paths.rs`: `folder_keys_round_trip_and_stay_apart_from_file_keys` (as `file_keys_round_trip_and_never_prefix_each_other`).
  - A node test in `tests/two_nodes.rs`, `folder_records_from_a_writer_reach_the_other_machine_and_a_readers_are_ignored`: put `Present` for `notes` and `MovedTo { to: "docs" }` for `old`, and the other node's `folder_records` shows both. A record from a non-writer author isn't returned.
- [ ] **Step 2: Watch them fail. Step 3: Implement. Step 4: Pass, plus the full verification.**
- [ ] **Step 5: Commit:** `hydra-swarms: folder records in the swarm document`.

### Task 4: The layout decision (pure)

**Files:**
- Create: `crates/hydra-swarms/src/layout.rs`
- Modify: `crates/hydra-swarms/src/lib.rs`

**Interfaces:**
- Consumes: `FolderRecord`, `FolderState` and `FolderVersion` from Task 3, and `RelPath`.
- Produces:

```rust
pub struct Moves { /* the moves in force, after loops are dropped */ }
impl Moves {
    pub fn from_records(records: &BTreeMap<RelPath, Vec<FolderVersion>>) -> Moves;
    /// Where an entry for `p` made at `at` belongs now.
    pub fn map(&self, p: &RelPath, at: u64) -> RelPath;
}

pub struct DiskFolder { pub may_sync: bool, pub empty: bool, pub synced_files: BTreeSet<String> /* names relative to it */, pub subfolders: BTreeSet<String> }

pub struct LayoutInput<'a> {
    pub known: &'a BTreeMap<RelPath, KnownFolder>,    // the folder index: what this machine and the swarm last agreed on
    pub disk: &'a BTreeMap<RelPath, DiskFolder>,      // folders scanned now (those under a dot folder left out)
    pub records: &'a BTreeMap<RelPath, Vec<FolderVersion>>,
    pub live_files: &'a BTreeSet<RelPath>,            // synced file paths after `Moves::map`, non-tombstone
    pub writer: bool,
    pub now: u64,
    pub newest_file_at: &'a dyn Fn(&RelPath) -> Option<u64>, // newest file entry under a folder, for Ruling A
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct KnownFolder { pub applied_at: u64 }

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LayoutPlan {
    pub publish: Vec<(RelPath, FolderRecord)>,
    pub detected_moves: Vec<(RelPath, RelPath)>,      // renames made here: index and disk names follow
    pub disk_moves: Vec<(RelPath, RelPath)>,          // incoming renames to carry out on disk, parents first
    pub create: Vec<RelPath>,
    pub remove_if_empty: Vec<RelPath>,                // deepest first, after the file pass
    pub forget: Vec<RelPath>,                         // drop from `known`
    pub notices: Vec<LayoutNotice>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LayoutNotice {
    RenameLost { mine: RelPath, kept: RelPath, by_machine: String },
    DeleteKeptForWork { path: RelPath, deleted_by_machine: String },
    MyDeleteKept { path: RelPath },
}

pub fn plan(input: &LayoutInput) -> LayoutPlan;
```

The rules `plan` follows. These are the spec's §4 and §5, and the plan's Rulings A–C.
- **The newest record per path** decides by `(at, author, machine)`, as file entries do.
- **Moves:**
  - A newest `MovedTo` at A moves A to its `to`.
  - Moves are applied oldest first. Each move's source and target are first mapped through the older moves. This gives `notes→docs` plus `notes/bob→notes/robert` = `docs/robert`, in either order.
  - A move that would put a folder inside itself drops the older move of the two, then everything is recomputed. That is the crossed-moves case: "the later move wins and the earlier one is undone".
  - `map(p, at)` applies only moves whose `at` is greater than or equal to the entry's `at`.
- **Live:** a folder is live when its newest record is `Present`, or it has a live descendant: a path in `live_files`, or a live subfolder.
- **Detected rename** (writer only):
  - It needs a known folder missing from `disk`, and exactly one folder in `disk` that isn't known, isn't the target of an incoming move, and has the same `synced_files` names and the same `subfolders` as the known folder last had.
  - Match on names only, and fold case where `paths::fold` does. One candidate is a rename; zero or several is a delete plus a create.
  - A rename publishes `MovedTo { to }` at the old path and `Present` at the new one, both with `at = now`, and lists it in `detected_moves`.
- **Detected delete** (writer only): a known folder missing from disk with no rename publishes `Deleted` (`at = now`) and is forgotten.
- **Detected create** (writer only): a folder in `disk` that `may_sync`, isn't known, and is empty or holds a synced file joins. It publishes `Present` (`at = now`) and becomes known.
- **Catch-up (Ruling A):** a known folder on disk that is live only through its files, with no record, publishes `Present` at `newest_file_at(folder)`. A folder with no files under it uses `now`.
- **Incoming:**
  - A record newer than `known[p].applied_at` acts once.
  - `Present` for a folder not on disk adds it to `create`.
  - A move from a folder on disk to a target adds it to `disk_moves`.
  - `Deleted` adds the folder to `remove_if_empty` unless it is live, and forgets it unless it stays.
- **Notices:**
  - When an incoming newer move overrides a rename made here, `RenameLost`.
  - When an incoming delete leaves a folder here because of a live descendant that the deleting machine hadn't seen, `DeleteKeptForWork`. That is any descendant whose newest entry or record is after the delete or isn't known to it, judged by the delete's `at`.
  - On the deleting machine, when its deleted folder turns live again, `MyDeleteKept`.
- **Ignored by an owner's `.swarmignore`:** a known folder whose `DiskFolder.may_sync` turns false is forgotten. It publishes no record, and nothing is removed anywhere (spec §4.1). If the line is removed, it joins again as a create.
- **Readers** (`writer == false`): publish nothing and detect nothing. Local creates, renames and deletes stay local and unflagged. A known folder a reader deleted is forgotten without a record, so it is recreated only by a record newer than its `applied_at`.

- [ ] **Step 1: Write failing unit tests in `layout.rs`,** one per rule, using small builders:
  - `an_empty_new_folder_is_published`;
  - `a_folder_holding_only_files_that_dont_sync_never_joins`;
  - `a_nested_empty_layout_joins_whole`;
  - `a_known_folder_that_gains_only_unsynced_files_stays` (no record published);
  - `a_rename_is_detected_by_names_even_with_an_edit_in_the_same_scan`;
  - `two_renamed_empty_folders_fall_back_to_delete_and_create`;
  - `an_incoming_rename_moves_the_folder_on_disk`;
  - `the_later_of_two_renames_wins_and_the_loser_is_told`;
  - `a_parent_and_a_child_rename_both_apply_in_either_order`;
  - `crossed_moves_end_without_a_loop_and_the_later_wins`;
  - `map_moves_an_older_entry_and_leaves_a_newer_one`;
  - `a_rename_and_a_delete_of_the_same_folder_keep_the_renamed_folder`;
  - `a_delete_removes_an_empty_folder_deepest_first`;
  - `a_delete_keeps_a_folder_with_an_unseen_file_or_subfolder_and_tells_both`;
  - `a_catch_up_record_takes_the_newest_file_time`;
  - `a_reader_publishes_nothing`;
  - `folders_differing_only_by_case_are_one_folder`: `Drafts` and `drafts` are created at the same time, and the result is one folder with no moves.
  - `a_rename_differing_only_by_case_is_a_rename` (Review Focus 1): `Notes` → `notes` gives `MovedTo`, not a delete;
  - `a_folder_an_owner_ignores_is_forgotten_without_a_record`.
- [ ] **Step 2: Watch them fail:** `cargo nextest run -p hydra-swarms --lib layout`. Expected: a compile error.
- [ ] **Step 3: Implement `layout.rs`. Step 4: Pass, plus the full verification.**
- [ ] **Step 5: Commit:** `hydra-swarms: the layout decision for folders`.

### Task 5: The folder pass syncs the layout

**Files:**
- Modify: `crates/hydra-swarms/src/folder.rs`
- Test: `crates/hydra-swarms/tests/folders.rs`

**Interfaces:**
- Consumes: `layout::{plan, Moves, LayoutInput, LayoutPlan, KnownFolder, DiskFolder, LayoutNotice}` from Task 4, `put_folder` and `folder_records` from Task 3, and `SyncRules` from Task 1.
- Produces:
  - `FolderEvent::RenameLost { mine: RelPath, kept: RelPath, machine: String }`;
  - `FolderEvent::DeleteKeptForWork { path: RelPath, machine: String }`;
  - `FolderEvent::MyDeleteKept { path: RelPath }`.
  - The folder index is saved beside the file index in `index.json`, as a new field `folders: BTreeMap<RelPath, KnownFolder>` with `#[serde(default)]`.

Order inside `reconcile`:
1. The scan also returns each folder as a `DiskFolder` (the `scan` already walks every directory).
2. Read `folder_records` and build `Moves`.
3. Remap `history` keys with `Moves::map`, for entries and parent records alike, before `case_owners`.
4. Call `layout::plan`.
5. Carry out `detected_moves`: rewrite index keys and `disk_names` from the old prefix to the new one, so the file pass sees no change.
6. Carry out `disk_moves`, parents first. If the target exists, move each child in and merge. A clash between two files becomes a conflict by the file rules on the next pass. Rewrite the index and the in-memory scan paths. If a rename on disk fails (a file is open), skip that move this pass and try again next pass.
7. Create the folders in `create`.
8. Publish every record in `publish`.
9. Run the existing file pass unchanged.
10. Remove every folder in `remove_if_empty` that is truly empty (`fs::remove_dir`, deepest first).
11. Turn the notices into events. Save the folder index.

- [ ] **Step 1: Write failing two-machine tests in `tests/folders.rs`,** using `synced_pair`, `eventually` and `cut_off` or `reconnect`:
  - `an_empty_folder_reaches_the_other_machine`;
  - `a_nested_empty_layout_reaches_the_other_machine`;
  - `a_rename_carries_a_file_the_renamer_hadnt_seen`: cut off, the writer adds `notes/new.md`, the owner renames `notes` to `docs`, reconnect. Both end with `docs/a.md` and `docs/new.md`, and neither has `notes`.
  - `two_renames_of_one_folder_converge_on_the_later_name`, with a `RenameLost` event on the losing side;
  - `an_edit_under_the_old_name_lands_in_the_renamed_folder_as_the_file`: no conflict copy, and the text is the edit.
  - `a_delete_keeps_an_unseen_file_and_its_folder`, with `DeleteKeptForWork` on one side and `MyDeleteKept` on the other;
  - `deleting_every_file_leaves_the_folder_everywhere`;
  - `a_node_modules_folder_with_empty_subfolders_never_syncs`;
  - `an_existing_folder_with_files_gets_a_catch_up_record`: delete the folder's only file, and the folder stays on both machines.
  - `a_rename_blocked_by_an_open_file_waits_and_then_completes` (Review Focus 2, Windows only, `#[cfg(windows)]`):
    - The writer holds `notes/a.md` open with `File::open`, and the owner renames `notes` to `docs`.
    - After a pass, the writer still has `notes/a.md` whole and no partial `docs`.
    - The writer drops the handle. The next pass ends with `docs/a.md`.
  - `edits_under_the_oldest_name_land_at_the_newest_name` (Review Focus 3): the writer is cut off and edits `a/x.md`. The owner renames `a` to `b`, then `b` to `c`. After reconnecting, both have `c/x.md` with the edit.
  - `a_deleted_folder_holding_a_conflict_copy_stays` (Review Focus 4): on a deep tree, the deletion removes the empty folders and leaves the one holding a conflict copy.
- [ ] **Step 2: Watch them fail:** `cargo nextest run --profile ci -p hydra-swarms --test folders`.
- [ ] **Step 3: Implement. Step 4: Pass, plus the full verification.**
- [ ] **Step 5: Commit:** `hydra-swarms: folders sync as a layout: create, rename and delete`.

### Task 6: Notices on the bell

**Files:**
- Modify: `crates/hydra-swarms/src/service.rs` (`folder_events`)

**Interfaces:**
- Consumes: the three events from Task 5.

The text. `{swarm}` is `self.name`.
- `RenameLost`: `"{swarm}: {mine} was also renamed on {machine}, to {kept}. That name was later, so it's the one kept."`
- `DeleteKeptForWork`: `"{swarm}: {path} was deleted on {machine}, but it holds work added here, so it was kept."`
- `MyDeleteKept`: `"{swarm}: you deleted {path}, but someone had added to it, so it's back with their work."`

- [ ] **Step 1: Write a failing test** next to the existing `folder_events` tests (or in `tests/service.rs` if that is where notice text is tested): each event produces the text above.
- [ ] **Step 2: Watch it fail. Step 3: Implement. Step 4: Pass, plus the full workspace suite** (`cargo nextest run --workspace --profile ci`).
- [ ] **Step 5: Commit:** `hydra-swarms: tell people when a folder rename or delete didn't go their way`.

The hands-on check, with two dev copies and their own `HYDRA_HOME`, goes to the user:
- make an empty layout;
- rename a folder;
- delete one while the other machine adds a file to it;
- add a `.swarmignore` line;
- edit `.swarmignore` as a writer.
