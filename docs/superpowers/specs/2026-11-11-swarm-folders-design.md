# Swarm folders: the layout syncs, not only the files

Builds on the swarms design (`2026-10-27-swarms-design.md`, §5 Sync rules). Where this document
says nothing, §5 still holds.

## 1. Why

A swarm's folder is often set up as a layout for other people to fill in: one person makes
`notes/alice/`, `notes/bob/` and `skills/` and expects everyone else to see the same layout
ready for their files.

Today only files sync. A folder exists on another machine only as the path to a file in it, so:

- an empty folder never reaches anyone else, which is the case that prompted this;
- a renamed folder reaches the others as every file deleted and added again, and the old folder
  is left behind there, empty;
- a deleted folder leaves an empty folder behind on every other machine.

## 2. Goals

- **A folder appears everywhere, even empty.** A layout made on one machine shows up on every
  other machine.
- **A rename renames.** Renaming or moving a folder on one machine renames it on the others,
  with everything in it, including files added there on that machine that the person renaming
  hadn't seen yet.
- **A delete never loses work.** Deleting a folder deletes, on the other machines, the files the
  person deleting knew about, and the folder if nothing is left. A file someone added there that
  the deleting person hadn't seen is kept, and so is its folder.
- **The layout stays until someone deletes it.** Deleting every file in a folder leaves the
  folder. Only deleting the folder removes it.

## 3. Non-goals

- Renaming a single file stays a delete plus an add, as §5 says today.
- Readers' folders don't sync from them. Readers send nothing, as today.
- Folders keep no history of their own beyond what deciding a sync needs.
- No settings in hydraui for this. Which folders sync follows from the file-type list, the dot
  rule and `.swarmignore` (§4).

## 4. Which folders sync

These rules are the user's.

- **A folder whose name starts with `.` never syncs, and nor does anything in it** (the
  user's decision): `.git`, `.github`, `.vscode`, `.obsidian`. That includes files of a type that
  would otherwise sync, such as a `.md` inside `.github`, which do sync today. Files whose own
  name starts with `.` aren't affected.
  - *My call:* a swarm that already syncs files inside such a folder stops syncing them on
    upgrade. Nothing is deleted anywhere. Every machine keeps the copies it has, as if the
    swarm had never shared them, and entries for those paths are ignored from then on.
- A folder **counts**, and syncs, when it is **empty**, or when it **holds a file that syncs**
  at any depth.
- A folder that is empty apart from empty folders counts as empty, so a layout several levels
  deep can be set up before anything is in it.
- A folder that holds only files that don't sync doesn't count (a `node_modules`, a
  build folder). It stays on that machine and isn't flagged, like a file that doesn't sync.
- *My call:* a folder inside one that doesn't count doesn't count either, even when it is empty.
  Otherwise the empty folders inside a `node_modules` or a build folder would sync to everyone.
- **Once a folder has synced, it stays in the layout until someone deletes it** (the user's
  decision), whatever later lands in it on any machine. A folder's contents differ from machine
  to machine, so one person's stray file that doesn't sync must never remove a layout folder
  for everyone.
- So the rules above decide only whether a folder **joins** the layout when hydra first sees
  it. A folder that holds only files that don't sync at that point (`node_modules`)
  never joins.
- Accepted gap: a build folder that hydra happens to see while it is still empty joins the
  layout, and shows up empty on the other machines until someone deletes it.
- The swarm folder itself always counts and is never recorded.
- Folder names follow the same rules as file names (`RelPath`): safe on every OS, and matched
  case-insensitively where the file rules already do that.

### 4.1 `.swarmignore`

The user's idea: a file in the swarm that says what doesn't sync.

- **Where:** `.swarmignore` in the swarm folder's root. *My call:* only that one counts, not
  files with that name in subfolders, so there is one place to look.
- **Syntax:** the same patterns as `.gitignore` (`build/`, `*.tmp`, `drafts/**`, `!keep.md`).
  *My call:* use an existing gitignore matcher (such as the `ignore` crate's), not a new one.
- **It only takes things out.** A file still needs a type on the swarm's list to sync. A `!`
  line can bring back something an earlier line took out, but never a folder starting with `.`
  (the built-in rule wins) and never a file whose type isn't on the list.
- **`.swarmignore` itself always syncs,** whatever the type list and the dot rules say, and no
  line in it can take it out.
- **Only owners' changes count** (the user's decision), as for the file-type list, since it
  decides what syncs for everyone.
  - A writer's edit to it is held on their machine like a reader's edit is today: it doesn't
    sync, an alert says so, and **Use the swarm's version** puts it back.
  - On every machine, a version of `.swarmignore` counts only if it was written by someone who
    was an owner at the time, by the same rule as other owner-only changes.
- **When a line is added:**
  - Files that now match stop syncing. Nothing is deleted on any machine, and each machine
    keeps the copy it has.
  - Folders that now match leave the layout the same way. *My call:* unlike a stray local file
    (§4), this is an owner's deliberate change for the whole swarm, so it does take a folder out
    of the layout.
- **When a line is removed:** files that no longer match start syncing as if they were new, and
  so do folders. Where two machines have different copies, §5's conflict rules apply.
- **Conflicting edits** to `.swarmignore` by two owners follow §5's file rules: the newer one
  keeps the name, the other becomes a conflict copy, and the conflict copy has no effect.

## 5. Behaviour

### 5.1 Creating

- A writer's new folder that counts is recorded in the swarm and created on every machine, empty
  or not.
- A folder made only as the path to an incoming file is recorded too, by whoever's file made it.
  *My call:* recording it is harmless and keeps one rule: every folder that counts has a
  record.
- *My call:* a record that only says a folder exists, written on someone else's behalf (for an
  incoming file's folder, or for every existing folder when a machine first upgrades), carries
  the timestamp of the newest file entry that implied it, never this machine's clock now. A
  catch-up record must never look newer than a real rename or delete.

### 5.2 Renaming and moving

Renaming a folder and moving it to another place in the swarm are the same thing here.

- On the machine that renamed it, the rename is recognised and recorded as a rename of the
  folder: from its old path to its new one.
- On every other machine, the folder is renamed in place, with everything in it, before any
  file changes from the same rename are applied. Files there that the person renaming hadn't
  seen move with the folder and then sync under the new path as new files.
- **Two renames of the same folder at about the same time:** the later rename wins, by the same
  rule as files: the entry's timestamp, ties broken by author, then machine. Everyone ends up
  with one folder, at the later name, with everything from both sides in it. Nothing is lost, so
  there is no conflict copy. The person whose rename lost is told on the bell.
- **Work done in the old folder by someone who hadn't seen the rename yet:**
  - It ends up in the renamed folder, and never brings the old folder back.
  - This covers a file edited in the old folder, and a file added there, before the rename
    reached that machine.
  - *My call:* "hadn't seen it" is judged by timestamp. An entry under the old path that is
    older than the rename follows the rename. A newer one is someone deliberately making a new
    folder with the old name, and stands.
  - *My call:* a rename is never an edit. A file edited under the old name lands in the renamed
    folder as that file, with the edit, not as a conflict copy. Only a real edit on both sides
    is a conflict, by §5's rule.
- **Other operations at about the same time** (*my calls*, all following "nothing is lost"):
  - **Rename and delete of the same folder:** the rename survives. The delete removes only what
    the person deleting knew about, and the folder under its new name is something they hadn't
    seen. Both are told on the bell.
  - **A parent and a child renamed at the same time** (`notes→docs` alongside
    `notes/bob→notes/robert`): both apply, giving `docs/robert`.
  - **A rename onto a name someone else has just created:** the two folders merge. Files with
    the same name in both are a conflict by §5's file rule.
  - **Crossed moves** (A moved into B while B is moved into A): the later move wins and the
    earlier one is undone, so it always ends in a layout without a loop.
- **Recognising a rename:**
  - hydra spots a rename when one scan shows a counting folder gone and a new folder appeared
    with the same contents as the last-synced state of the old one: the same synced files by
    name, and the same layout of folders.
  - *My call:* names, not content. A file edited in the same scan as the rename (the watcher's
    debounce can put a rename and a save together) doesn't stop the rename being recognised.
    The edit then syncs as an ordinary edit under the new path.
  - This works the same for changes made while hydraui runs and for changes found at start-up.
    The watcher only triggers a rescan; it doesn't say what changed.
  - *My call:* when the match is ambiguous, for example two empty folders renamed in the same
    scan, it falls back to a delete plus a create. On the other machines that gives the same
    layout, except that a file added there to the old folder stays in the old folder under the
    delete rules (§5.3). That is acceptable for a case this rare.

### 5.3 Deleting

- Deleting a folder records the folder as deleted, together with the file deletes §5 already
  records for its files.
- On another machine, the files the deleting person knew about are deleted, by the file rules
  in §5. Then the folder is removed if it is empty.
- If a file that syncs is still there, one the person deleting hadn't seen, it is kept and
  syncs as a new file. The folder therefore counts again and comes back on every machine,
  holding that file.
  - The person who deleted it and the person whose file kept it are both told on the bell:
    "notes/bob was deleted on Olive's laptop, but it holds a file added here, so it was kept."
  - *My call:* this is a notice, not a **Needs attention** entry. There's nothing to decide: to
    delete it after all, delete the folder again.
- *My call:* a subfolder the person deleting hadn't seen keeps the folder too, the same way an
  unseen file does, even when that subfolder is empty, because it counts (§4). The delete
  covers the subfolders the person deleting knew about (their records), not any others.
- If only files that don't sync are left, the folder stays on that machine, out of the layout,
  and isn't flagged.
- An edit to a file in a folder deleted elsewhere follows §5's delete-vs-edit rule for that
  file. The folder stays because the file does.

### 5.4 Deleting every file in a folder

- The files are deleted everywhere. The folder stays, and stays recorded, because it is now
  empty and so still counts.

### 5.5 Readers

- A reader's machine applies incoming folder creates, renames and deletes like anyone else's.
- A folder a reader makes, renames or deletes on their own machine isn't shared and isn't
  flagged. *My call:* unlike a reader's edit to a file, there's no content at stake to hold.

## 6. Constraints

- **Older hydras ignore folder records.**
  - A new machine's empty folders don't appear on them.
  - A rename made on an older hydra still reaches others as delete plus add, and the old folder
    stays.
  - An older hydra still shares files inside folders starting with `.`; newer ones ignore
    those entries.
  - Nothing breaks either way.
- **Folder records live in the swarm document beside file entries,** under their own prefix, so
  the file sync and the claims service never read them as files or claims.
- **Every rule in §5 is decided by a pure function,** like `decide` for files, with unit tests
  for each case in §5 and §4.
- **Two-machine tests,** like the existing folder tests, cover at least:
  - an empty folder appearing;
  - a nested empty layout appearing;
  - a rename carrying an unseen file;
  - two concurrent renames converging on the later name;
  - an edit made under the old name landing in the renamed folder;
  - a delete keeping an unseen file and its folder;
  - deleting every file leaving the folder;
  - a `.github` folder holding a `.md` not syncing, and its empty subfolders not syncing;
  - a `node_modules` folder with empty subfolders not syncing;
  - an owner adding a `.swarmignore` line: matching files stop syncing and nothing is deleted;
    removing it brings them back;
  - a writer's edit to `.swarmignore` being held and not taking effect anywhere.
- **Never delete anything that isn't synced.** No rule here removes a file that doesn't sync, or
  a folder that isn't empty.
- Plain sentences for anything a person reads, as in the rest of hydra.

## 7. Left to the builder

- How folder records are keyed and what they carry, beyond: path, present or deleted, renamed
  from where, the timestamp, author and machine.
- How the last-synced state of folders is kept in the folder index.
- How the rename map is applied to entries under an old path, as long as §5.2's outcome holds.
- The exact wording of the bell notices.
