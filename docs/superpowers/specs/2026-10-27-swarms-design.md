# Swarms: folders kept in sync between hydra machines

Hydra machines, the user's own and their teammates', form **swarms** over iroh. Each swarm has a
folder on every member machine, and hydraui keeps those folders in sync with iroh-docs and
iroh-blobs. The first use is sharing skills, but this feature is only the sync. How sessions
find and use what's in the folder is a separate feature.

Mockup: [swarms-mockups/settings.html](swarms-mockups/settings.html). It shows
Settings › Swarms, a swarm's page, the invite and join dialogs, the two alerts, and "You and this
machine". The text below wins where they differ.

## 1. Why

The user wants a shared library that every hydra machine in a team carries locally, starting
with skills. It has to work peer to peer: there's no server of ours, and teammates' machines
come and go.

### Goals

- **Swarms, many per machine.** A machine can be in several swarms, and each swarm maps to one
  local folder on that machine.
- **Folder sync** of the swarm's folder between members while hydraui runs on them, using
  iroh-docs for the file index and iroh-blobs for the contents.
- **Roles.** Owners, writers and readers (§4), with membership and roles enforced by every
  member's hydra.
- **Single-use invites** for people, and single-use codes for adding another of your own
  machines.
- **Conflicts are never lost:** both copies are kept and someone is told (§5).

### Non-goals

- How Claude sessions use the folder's contents, skills included.
- Syncing while hydraui isn't running. No background service.
- A richer permission model: per-folder rights, approval flows, audit. Later; owners and roles
  are its starting point.
- Cryptographic revocation. A removed member keeps whatever they already received (§4).
- Automatic merging of edits, version history, selective sync, large files.
- The phone app. Phones don't join swarms.

### Anti-goals

- An edit disappearing without anyone being told.
- A change from someone who isn't currently a writer or owner landing in anyone's folder.
- A file type not on the swarm's list leaving a machine.
- hydra touching anything outside a swarm's folder: no following links out of it, no `..`
  paths from peers.
- Sync work blocking hydraui's UI thread.

## 2. Decisions the user made

- **The feature is called Swarms** (not groups), in the UI and in code.
- **Over iroh**, with **iroh-docs + iroh-blobs** keeping the folder in sync.
- **Members are teammates and machines**, and a machine can be in **multiple swarms**.
- **Only some people can write**, and a richer permission model comes later.
- **Owners:** owners manage the swarm, and **owners can add other owners**.
- **Changing a role or removing a member** happens without making a new swarm. An owner can
  remove a writer, then re-add them as a reader.
- **Runs only while hydraui is running.**
- **No locks; deal with conflicts when they happen.** The newer change keeps the name, the
  other is kept beside it as a conflict copy, and hydraui notifies.
- **Deletes spread to everyone.** A delete that meets an edit is a conflict: the edit is kept.
- **A reader who changes a file:** that file stops syncing for them and they're alerted.
- **Only listed file extensions sync**, and the default list is just `.md`.
- **Invites are single-use.**
- **Identity:**
  - The user sets their **name once**, and it is used in every swarm.
  - The user **names the machine** rather than hydra taking the host name.
  - **Each swarm has its own key** on each machine.
  - The id is **bound to the machine**, not to a hydraui window or instance.
- **The user's own machines are linked to them** (option B). A machine added with a single-use
  code joins every swarm its person is in.
- **Removing one of your machines replaces your personal key**, so a lost machine is really
  shut out.

## 3. Identity

- **Person.** The first time the user makes, joins or links into a swarm, hydraui asks for
  **Your name** and creates a personal identity: a key pair, kept in the OS keyring. Linking a
  machine (§6) copies it to that machine. A person's role is per swarm, and every machine of
  theirs has that role.
- **Machine name.** It is set once, defaults to the computer's host name, and can be edited.
- **Swarm key.** On joining a swarm, the machine creates a key for that swarm, kept in the
  keyring. This is the machine's iroh identity in that swarm, and each swarm runs its own iroh
  endpoint with it. This isn't meant to hide that one person is in several swarms: a person's
  name and personal key appear in every swarm they're in.
- **Machine key.** *My call:* each machine also has one stable key of its own, used only between
  the same person's machines. It carries machine linking, offering a new swarm to your other
  machines, and name changes. This needs a private channel that doesn't depend on sharing a
  swarm.
- **Shared by all windows.** All of this is stored per OS user, not per hydraui window.
- **Changes spread.** A change of name or machine name updates every swarm the machine is in.
- **Settings.** The **You and this machine** page holds the name, the machine name, the list of
  your machines (with Remove…) and **Add another machine of mine…**.

## 4. Roles and enforcement

- **Owner:** everything a writer can do, plus:
  - inviting, promoting, demoting and removing members;
  - adding other owners;
  - editing the file-type list;
  - renaming the swarm.

  The creator is the first owner. Owners can demote or remove other owners, but the last owner
  can't be removed or demoted. *My call:* the last owner can't leave either, until they've
  passed ownership on.
- **Writer:** adds, changes and deletes files.
- **Reader:** receives everything, sends nothing.
- **Enforcement:** iroh-docs signs every entry with its author, so every member's hydra applies
  three rules:
  - A membership or settings change counts only if it was signed by someone who was an owner at
    the time.
  - A file change reaches the folder only if its author is currently a writer or owner.
  - Machines whose person was removed are refused as sync peers.
- **Readers' capability.** *My call:* readers hold only the read capability, so they couldn't
  write even with a modified hydra. Writers and owners hold the write capability. A removed
  writer still holds it, but their entries are ignored by everyone (§1, non-goals).
- **What readers send.** *My call:* readers can't write to the swarm document, so anything that
  has to come from them goes to an online owner's hydra, which writes it for them. That covers
  name and machine-name changes, a new machine of theirs, and leaving. These updates wait until
  an owner is online.
- **Online status** ("3 of 5 machines online") is live connections, not stored records. Only
  "last seen" is stored.
- **Removal.** When a member is removed, their hydra tells them ("You were removed from
  team-skills"), if it is online, and stops syncing. Their files are left as they are.

## 5. Sync rules

- **What syncs:**
  - Files under the swarm folder whose extension is on the swarm's list (default `.md`).
  - The list belongs to the swarm, is synced, and is edited by owners.
  - *My call:* files over 10 MB don't sync, and hydraui says which.
  - Anything else stays on that machine and isn't flagged.
- **Local changes** (create, edit, rename as delete plus create, delete) are picked up by
  watching the folder, while hydraui runs. Changes made while it was closed are picked up when it
  starts, by comparing the folder with the last-synced state.
- **Conflicts:**
  - Each machine remembers the version of each file it last synced.
  - A conflict is a local change since then meeting a different incoming change.
  - The newer change keeps the file's name. *My call:* "newer" means the iroh-docs entry's
    timestamp, which is the author's clock, with ties broken by author id. A machine with a fast
    clock wins the name, but it never makes an edit disappear, because the other side is always
    kept as a conflict copy or, for delete vs edit, as the edit. The other is saved beside it as
    `<name> (conflict, <machine name>, <d Mon HH.MM>).<ext>`, and that copy syncs like any file.
  - hydraui shows a notification and lists the conflict under the swarm's **Needs attention**.
    *My call:* **Compare**, **Keep theirs** and **Keep mine** resolve it and remove the
    conflict copy.
- **Delete vs edit:** the edit is kept as a conflict. *My call:* the file stays on the editing
  machine with **Share it again** or **Delete it**.
- **Reader edits:** that file stops syncing on the reader's machine, an alert says so, and
  **Use the swarm's version** puts it back and resumes syncing.
- **Starting a swarm:** creating a swarm from a folder shares the allowed files already in it.
  *My call:* joining needs an empty or new folder.
- **Leaving** keeps the folder's files and stops syncing them.

## 6. Invites, joining and linking machines

- **Invite (owners only).**
  - Pick **Writer**, **Reader** or **Owner**, then copy a `hydra-swarm:` code to send any way
    you like.
  - The code is **single-use** and, *my call*, expires after 7 days. It can be cancelled, and
    pending invites are listed under Members.
  - Using it needs the inviting machine's hydraui to be running: the joiner connects to it, it
    checks and spends the code, records the new member, and hands over the read or write
    capability.
- **Join.** Paste the code and pick a folder; the user's name and machine name are already
  known. The folder fills as sync runs.
- **Add another machine of mine.**
  - A single-use code (7-day expiry) from one of your running machines.
  - The new machine is named, receives your personal identity, and joins every swarm you're in
    with your role. It asks for a folder per swarm.
  - The new machine has to be added to each swarm's member list, and readers can't write that
    list. *My call:* any online owner's hydra adds it automatically after checking that the
    request is signed by that person's identity.
  - Swarms you join later are offered to your other machines without a new invite.
- **Removing one of your machines** (You and this machine › Remove…) shuts it out, which is
  the user's decision (option a):
  - hydra makes you a new personal key and hands it to your remaining machines over their
    machine-to-machine channel.
  - It re-registers you in every swarm under the new key, signed with the old one, and from then
    on the old key is no longer accepted. The removed machine's swarm keys and machine key are
    refused as peers everywhere.
  - Remaining machines that are offline pick up the new key the next time they reach one of your
    machines that has it.
  - The removed machine keeps whatever files it already has, and if it held the write capability
    it still has it. Its entries are ignored, as for a removed writer (§4).
  - In a swarm where you're a reader, the re-registration waits for an owner to be online to
    write it (§4).

## 7. Where it shows in hydraui

- **Settings › SWARMS** (mockup): **You and this machine**, one entry per swarm (with a status
  dot and a count of things needing attention), **+ New swarm** and **Join a swarm…**.
- **A swarm's page** has:
  - its name and a status pill ("Up to date · 3 of 5 machines online" / "Syncing…");
  - your role;
  - the folder, with Open and Change…;
  - the last change;
  - **Needs attention**;
  - **Members**: people with their machines underneath, a role, online or last seen, owner
    actions, and pending invites;
  - **File types that sync**;
  - **Leave swarm…**.
- **Notifications** use hydraui's existing toasts, for conflicts, reader edits, removal, and a
  machine of yours being offered a new swarm.

## 8. Constraints

- **Platforms.** Windows desktop is primary, and hydraui also builds on macOS. Nothing in this
  feature is Windows-only.
- **Versions.** iroh 1.x (the workspace has 1.4.1) with **iroh-docs 0.101** and **iroh-blobs
  0.103**. Both depend on `iroh ^1` (checked on crates.io).
- **Runtime.** Follow the phone link's pattern (PR #4): a tokio runtime on its own thread,
  bridged into GPUI over channels.
- **One window runs sync.** One hydraui window per machine runs all swarm sync, taking a
  cross-process lock, and the others show status and take over if it closes. This reuses
  `link::owner`'s approach.
- **Secrets.** Keys live in the OS keyring (`keyring`, already a dependency). *My call:* swarm
  stores (iroh-docs and iroh-blobs data) live under hydra's shared state root, one per swarm,
  not inside any environment's state folder.
- **Workspace standards.** Tests for the sync rules (conflicts, delete vs edit, reader edits,
  allow-list, size cap, path safety) and for role enforcement. clippy clean.
- **Dev builds** of hydraui are stopped by PID or `scripts\stop-dev-hydraui.ps1`, never by
  name.

## 9. Left to the builder

- **Doc layout:** how files and swarm metadata (members, roles, file types, name) are laid
  out in the iroh-docs document.
- **Signed changes:** the format of signed membership changes, and how "owner at the time" is
  decided when owners change while some machines are offline.
- **The handshake protocol** for invites and machine linking: ALPN, message shapes, and the
  code format beyond its `hydra-swarm:` prefix.
- **Folder watching:** the watcher, debouncing, and detecting changes made while hydraui was
  closed.
- **Relays:** the iroh relay defaults (n0's public relays, as the phone link uses).
- **Copy** beyond the mockup.
