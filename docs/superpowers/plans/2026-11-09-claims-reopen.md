# Reopening a Finished Claim Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a writer undo a mistaken done from the swarm page, so the key can be claimed again.

**Architecture:** A new `ReopenRecord` in the swarm document under the context's claims prefix. `settle` drops every done and claim on a key that was made at or before the newest writer's reopen of it, whatever its source, so nothing in the heard-records machinery needs to change. The service gets a `ClaimReopen` command; the page gets a Reopen button on Recent done rows, copying the Release button's confirm flow.

**Tech Stack:** Rust, iroh-docs, gpui, nextest.

**Spec:** `docs/superpowers/specs/2026-11-04-swarm-claims-design.md`, §4.8.

## Global Constraints

- Never stop hydraui by name (no `taskkill /IM hydraui.exe`, `Stop-Process -Name hydraui`, `pkill hydraui`).
- Work on `feature/swarm-claims` in `E:\AI\davojc\hydra-worktrees\swarm-claims`. Commit there; never push.
- No MCP tool for reopening. Sessions can't undo a done.
- Who may reopen: any machine whose person is a writer or owner (`People::writes`). Retired machines' reopens don't count.
- A reopen covers records with time `<=` its `at`: a done's `at`, a claim's `claimed_at`.
- Follow the crate's habits: comments that say why, tests named as sentences, plain sentences for anything a person reads.

## Review Focus

1. A reopen racing a fresh done: the done stamped after the reopen must win; one stamped before must not.
2. A holder told it lost the key when it was marked done must not show as holding it again after a reopen, even while its claim record hasn't lapsed yet.
3. A done heard from a teammate's answer (`heard_dones`) and never in this document must still be covered by a reopen.
4. A reader or a retired writer's reopen record changes nothing.
5. Pruning: a reopen deleted after `keep_done` must not bring back a done, so a reopen is never pruned before every done it covers would have aged out.

---

### Task 1: Reopen in hydra-swarms

**Files:**
- Modify: `crates/hydra-swarms/src/claims.rs` (record, `Records.reopens`, `settle`)
- Modify: `crates/hydra-swarms/src/paths.rs` (`reopen_key`)
- Modify: `crates/hydra-swarms/src/node/claims.rs` (`Record::Reopen`, `parse_record`, `claim_records`, `write_reopen`, `prune_claims`)
- Modify: `crates/hydra-swarms/src/service.rs` and `crates/hydra-swarms/src/service/claims.rs` (`Command::ClaimReopen`)
- Test: unit tests in `claims.rs` and `paths.rs`; a network test in `crates/hydra-swarms/tests/` beside the existing claims service tests.

**Interfaces:**
- Produces:
  - `pub struct ReopenRecord { pub context: String, pub key: String, pub at: u64 }` (serde, same derives as `DoneRecord`).
  - `Records.reopens: Vec<(String, ReopenRecord)>`.
  - `pub fn reopen_key(context: &str, key: &str) -> Vec<u8>`, shaped like `done_key` with `reopen` in place of `done`.
  - `Command::ClaimReopen { swarm: String, context: String, key: String, reply: <same reply type as ClaimRelease> }`. It refuses with a plain sentence if this machine's person isn't a writer, writes `ReopenRecord { at: now }` and refreshes claims so the page updates at once.
- Consumes: `settle`, `People::writes`, `write_record`, the `ClaimRelease` command as the pattern.

- [ ] **Step 1: Write failing `settle` tests in `claims.rs`**
  - `a_reopen_frees_a_finished_key`: done("k", 10s) by "a", reopen("k", 20s) by "b" → "k" absent (free).
  - `a_done_after_a_reopen_counts`: done 10s, reopen 20s, done 30s by "c" → `Done { author: "c", at: 30s }`.
  - `a_done_at_the_reopens_time_is_covered`: done 20s, reopen 20s → free.
  - `a_reopen_ends_claims_made_before_it`: claim("k", 5s) by "a" still renewing (renewed_at = now), done 10s, reopen 20s → free; and a claim at 25s by "b" → `Held` by "b".
  - `a_readers_reopen_changes_nothing`: reopen by "reader" → still `Done`. Same for a retired author.
  - `only_the_newest_reopen_matters`: reopens at 20s and 40s, done at 30s → free.
  - Also check a claim on a reopened key doesn't count against the limit when covered.
- [ ] **Step 2: Run them and watch them fail** (`cargo nextest run -p hydra-swarms --lib claims`): compile error, `ReopenRecord` not defined.
- [ ] **Step 3: Implement** `ReopenRecord`, `Records.reopens`, and in `settle` compute per key the newest `at` among reopens whose author `people.writes`; drop dones with `d.at <=` it and claims with `claimed_at <=` it before anything else. Fix every `Records { .. }` literal.
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Storage.** `reopen_key` with a `paths.rs` test that it stays apart from claim, done and release keys for the same context and key, as `claim_keys_stay_apart` does. `Record::Reopen` in `parse_record` (filed-under-its-own-key check like the others), `claim_records` pushes to `reopens`, `SwarmNode::write_reopen`, and `prune_claims` deletes this machine's reopens with `now - at > keep` (a done it covers has `at <=` the reopen, so it has aged out by then).
- [ ] **Step 6: Network test, written first:** `a_writer_reopens_a_done_key_and_it_can_be_claimed_again`, with two machines: one takes and marks "k" done; the other sees Done, sends `ClaimReopen`, and then a take of "k" succeeds on either machine, and the first machine's list shows "k" no longer done. Add `a_reader_cannot_reopen` checking the command's refusal. Watch them fail, then add `Command::ClaimReopen` and its handler next to `ClaimRelease`.
- [ ] **Step 7: Verify.** `cargo nextest run --profile ci -p hydra-swarms`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`. All pass.
- [ ] **Step 8: Commit** `hydra-swarms: a writer can reopen a finished key`.

### Task 2: Reopen button on the swarm page

**Files:**
- Modify: `crates/hydraui/src/workspace/claims_page.rs`

**Interfaces:**
- Consumes: `Command::ClaimReopen` from Task 1; the Release button, `can_release`, its confirm dialog and `swarms_command` as the pattern.

- [ ] **Step 1: Write a failing unit test** for a pure `can_reopen(my_role: Role) -> bool`: true for Writer and Owner, false for Reader. Watch it fail.
- [ ] **Step 2: Implement** `can_reopen`, then a **Reopen** button on each Recent done row shown when it is true. It opens a confirm dialog titled `Reopen {key}?` with the text `{key} can be claimed again. If it really was finished, it may be done twice.` and a `Reopen` button that sends `Command::ClaimReopen`. Errors show the way Release's do.
- [ ] **Step 3: Verify.** `cargo nextest run --profile ci -p hydraui`, clippy and fmt as in Task 1. All pass.
- [ ] **Step 4: Commit** `hydraui: Reopen on the swarm page's finished keys`.

The hands-on check (Reopen a done key on the page, see it leave Recent done, then claim it from a session) goes to the user with the rest of the checklist; an agent can't drive the GUI reliably.
