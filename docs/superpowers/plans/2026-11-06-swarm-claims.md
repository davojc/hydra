# Swarm Claims Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one machine in a swarm claim a job (a key in a context), with Claude sessions in any window as the first callers and a Claims section on the swarm page.

**Architecture:** The rules are pure functions in `hydra-swarms/src/claims.rs`. Claim records live in the swarm document under `claims/`, a gated `hydra/claims/1` ALPN carries the ask step, and each swarm's `Worker` keeps a book of live holders, which it renews and tells when they lose a claim. Every window, including the one running the swarms, reaches the service through one local TCP bridge. The MCP tools and the swarm page sit on top.

**Tech Stack:** Rust, iroh, iroh-docs, tokio, gpui, tiny_http, `cargo test`, nextest `network` group.

**Spec:** `docs/superpowers/specs/2026-11-04-swarm-claims-design.md`

## Global Constraints

- Never stop hydraui by name (no `taskkill /IM hydraui.exe`, `Stop-Process -Name hydraui`, `pkill hydraui`). Stop dev builds by PID or with `scripts\stop-dev-hydraui.ps1`.
- Work on a new branch `feature/swarm-claims` from `main`, not on `feature/files-per-session`. Copy the uncommitted spec and this plan onto it, and commit them in Task 1 only if the user has asked for the build to start.
- Defaults: `renew_secs` 60, `lapse_after` 3, `max_per_machine` 1, `keep_done` 30 days.
- `max_per_machine`: a positive number is the limit, -1 is no limit, 0 pauses the context (claims already held run on).
- The earliest `claimed_at` wins; on equal times the lowest machine id (author id) wins. Done beats any live claim.
- Nothing in `hydra-swarms` knows about pipelines, email or MCP. `HolderKind::PipelineRun` exists, but nothing uses it yet.
- Claims never appear in the swarm folder. Folder sync only reads `files/` and `parents/`, so it must stay that way.
- Pipelines, email and a `hydra claim` command are out of scope.
- Follow the crate's habits: `lock()` helpers, plain sentences for anything a person reads, comments that say why, tests named as sentences.
- Network tests go in `crates/hydra-swarms/tests/`, which nextest already places in the `network` group.

## Decisions this plan makes (spec left them to the builder)

| Decision | Choice | Why |
|---|---|---|
| Context settings storage | A new signed `Change::SetClaimContext` in the existing `log/`, folded into `Roster.claim_contexts` | Entitlement "when written" and survival of a removed owner come free, as with `SetTypes`. Older hydras skip it: `changes()` drops JSON it can't parse |
| Record layout | `claims/<context>/<blake3(key) hex>/claim`, `/done`, `/release/<target author>`, each one per author | Message-IDs hold `/` and `<`. The plain key is inside the record |
| Own release | Deletes the machine's own claim entry (a tombstone). Only someone else's release writes a release record | No extra record type for the common case |
| Limits for key and context | Key up to 1024 bytes. Context name 1 to 64 of `a-z 0-9 - _` | Keeps entries small |
| Clock slack | `CLOCK_SLACK` = 30 s, so the default lapse is 210 s | Above likely skew. A setter exists for tests |
| Ask timeout | `ASK_TIMEOUT` = 3 s, all members at once | Bound on how long a claim can wait |
| Finding the swarm | `claims_take` has an optional `swarm` (id or name). Without it, the one swarm that has the context | Environments have no link to swarms today |
| Second holder on one machine | A key this machine holds through a live handle answers `Held` (this machine) | Two sessions on one machine must not share a job. Taking up (§4.7) applies only when no live handle exists |
| Local channel | Loopback TCP with JSON lines and a secret, from `<running_dir>/swarms-claims.json` | A dropped connection also tells the host its holders ended, and the client reconnects to the next window |
| Done from a removed member | `/done` from an author in `roster.retired` still counts. Their claims don't | Removing a teammate must not make their last 30 days of work claimable again |
| "Limit in force" | Each claim record carries the `limit` it was made under | Settings history isn't kept. The value is self-reported, which is acceptable because this rule catches bugs and isn't a security check |
| "Settings changed" event | Delivered through `SwarmView`. There is no separate event | The page is its only reader |

## Review Focus

1. **A key that is released, then claimed again, by the same machine.** The old release record targets the old `claimed_at`, so it must not end the new claim. Pinned in Task 2 (`a_release_only_ends_the_claim_it_names`).
2. **A heard claim whose holder has released it.** A claim learned through asking must not block the key until it lapses once the tombstone arrives. Pinned in Task 3 (`a_tombstone_clears_a_heard_claim`).
3. **The MCP server stalling.** `claims_take` can take seconds, and the server handles one request at a time, so hooks and other tools would queue behind it. Pinned in Task 6 (`a_slow_claim_does_not_hold_up_other_tools`).
4. **The window running the swarms closes.** Other windows' holders must be renewed by the next window, without being told they lost. Pinned in Task 5 (`holders_survive_the_host_changing`).
5. **A machine waking from sleep.** Its timer fires with a stale `renewed_at`, and it must report lost rather than renew a lapsed claim back to life. Pinned in Task 4 (`a_woken_machine_is_told_it_lost`).

---

### Task 1: Context settings in the roster

**Files:**
- Create: `crates/hydra-swarms/src/claims.rs` (settings only for now)
- Modify: `crates/hydra-swarms/src/lib.rs`, `crates/hydra-swarms/src/roster.rs` (`Change` at :97, `Roster` at :218, `apply` at :501)

**Interfaces:**
- Produces:
  - `claims::ContextSettings { renew_secs: u32, lapse_after: u32, max_per_machine: i32, keep_done_secs: u64 }` (`Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug`), with `Default` giving 60, 3, 1, 2_592_000
  - `ContextSettings::lapse_micros(&self, slack: Duration) -> u64`
  - `ContextSettings::normalised(self) -> Self`: `renew_secs` at least 1, `lapse_after` at least 1, `max_per_machine` at least -1, `keep_done_secs` at least 60
  - `claims::valid_context_name(&str) -> bool`
  - `Change::SetClaimContext { name: String, settings: ContextSettings }`
  - `Roster.claim_contexts: BTreeMap<String, ContextSettings>`

- [ ] **Step 1: Write the failing tests** in `roster.rs`'s test module and `claims.rs`:

```rust
#[test]
fn only_owners_set_claim_contexts() {
    // Writer's change ignored; owner's change folded, with settings normalised.
    assert!(writer_fold.claim_contexts.is_empty());
    assert_eq!(owner_fold.claim_contexts["inbox"], ContextSettings::default());
}
#[test]
fn a_context_with_a_bad_name_is_ignored() { /* "Has Space" leaves claim_contexts empty */ }
#[test]
fn defaults_are_the_specs() {
    let d = ContextSettings::default();
    assert_eq!((d.renew_secs, d.lapse_after, d.max_per_machine, d.keep_done_secs), (60, 3, 1, 2_592_000));
    assert_eq!(d.lapse_micros(Duration::from_secs(30)), 210_000_000);
}
```

- [ ] **Step 2: Run them** with `cargo test -p hydra-swarms --lib claim`. Expected: compile failure, `ContextSettings` not found.
- [ ] **Step 3: Implement.** Add `pub mod claims;`, the settings type, the `Change` variant (with `#[serde(default)]` on the new `Roster` field if `Roster` is ever deserialised) and the `apply` arm, guarded by `by_owner` like `SetTypes`.
- [ ] **Step 4: Run** `cargo test -p hydra-swarms --lib`. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add crates/hydra-swarms
git commit -m "hydra-swarms: claim contexts in the roster"
```

### Task 2: The rules

**Files:**
- Modify: `crates/hydra-swarms/src/claims.rs`

**Interfaces:**
- Consumes: `ContextSettings`.
- Produces:
  - `HolderKind { Session, PipelineRun }`, `Holder { kind: HolderKind, label: String }`
  - `ClaimRecord { context: String, key: String, claimed_at: u64, renewed_at: u64, limit: i32, holder: Holder }`
  - `DoneRecord { context: String, key: String, at: u64 }`
  - `ReleaseRecord { context: String, key: String, target: String, claimed_at: u64, at: u64 }`
  - `People { pub members: BTreeMap<String, (String, Role)>, pub retired: BTreeSet<String> }`, keyed by author and holding person id and role; `People::from_roster(&Roster) -> People`
  - `Records { pub claims: Vec<(String, ClaimRecord)>, pub dones: Vec<(String, DoneRecord)>, pub releases: Vec<(String, ReleaseRecord)> }`, one context's entries by author; `Default + Clone`
  - `KeyState { Free, Held { author: String, claim: ClaimRecord }, Done { author: String, at: u64 } }`
  - `settle(records: &Records, people: &People, settings: &ContextSettings, now: u64, slack: Duration) -> BTreeMap<String, KeyState>`
  - `Take { Go, TakeUp(ClaimRecord), Held { author: String, claim: ClaimRecord }, Done { author: String, at: u64 }, Paused, AtLimit(i32) }`
  - `decide_take(states: &BTreeMap<String, KeyState>, key: &str, me: &str, settings: &ContextSettings) -> Take`

`settle` applies these in order, and the implementer writes the body from them:

1. Drop claims whose author isn't a writer or owner. Drop dones whose author is neither that nor in `retired`. Drop dones older than `keep_done_secs`.
2. Drop a claim named by a release (same `target` and `claimed_at`) whose author is the target's person or an owner.
3. Drop lapsed claims: `now - renewed_at > lapse_micros(slack)`, saturating.
4. Per author, sort the remaining claims by `(claimed_at, key)` and walk them, keeping a count. A claim with `limit >= 0` and `count >= limit` is dropped and not counted; every other claim is counted.
5. Per key: a surviving done gives `Done` (the newest done). Otherwise the minimum `(claimed_at, author)` gives `Held`. Otherwise the key is absent, and `decide_take` treats absent as `Free`.

`decide_take` order: `Done`, then `Held` by `me` gives `TakeUp`, then `Held`, then paused (`max_per_machine == 0`) gives `Paused`, then my count of `Held` states reaching a positive limit gives `AtLimit(limit)`, otherwise `Go`.

- [ ] **Step 1: Write the failing tests**, one per rule, with the spec's values:

```rust
fn the_earliest_claim_wins()                  // a at 200, b at 100 → b, whatever the author order
fn equal_times_go_to_the_lowest_machine_id()  // "aaa" and "bbb" at 100 → "aaa"
fn done_beats_any_live_claim()                // earlier live claim + later done → Done
fn a_claim_lapses_after_its_time_with_slack() // renew 60, lapse 3, slack 30: live at 210 s, gone at 211 s
fn done_is_forgotten_after_keep_done()        // done older than keep_done_secs → absent
fn a_removed_members_done_still_counts_but_their_claim_does_not()
fn a_readers_entries_are_ignored()
fn newest_claims_over_the_limit_are_ignored() // three claims, limit 2 → the newest is ignored, and its key is free
fn lowering_the_limit_takes_nothing_away()    // claims carry limit 2, settings say 1 → both stay
fn a_claim_made_under_zero_is_ignored()       // limit 0
fn no_limit_means_no_limit()                  // limit -1, 50 claims all held
fn a_release_only_ends_the_claim_it_names()   // release targets claimed_at 100, claim now at 500 → held
fn a_person_can_release_their_other_machine() // same person's other author → freed
fn an_owner_can_release_anyones()             // owner → freed
fn a_writer_cannot_release_someone_elses()    // other writer → still held
fn a_split_heals_to_one_holder()              // two live → earliest; both orders of input give the same answer
fn take_says_go_paused_at_limit_held_done_and_take_up() // decide_take table, including paused + own live → TakeUp
```

- [ ] **Step 2: Run** `cargo test -p hydra-swarms --lib claims`. Expected: compile failure.
- [ ] **Step 3: Implement** the types, `People::from_roster`, `settle` and `decide_take`.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** `hydra-swarms: the claim rules`.

### Task 3: Records in the document, and asking

**Files:**
- Modify: `crates/hydra-swarms/src/paths.rs`, `crates/hydra-swarms/src/wire.rs`, `crates/hydra-swarms/src/node.rs` (`GATED_ALPNS` at :96, `Router` at :238, `Core` at :160, `NodeEvent` at :82)
- Create: `crates/hydra-swarms/src/node/claims.rs` (add `mod claims;` in `node.rs`, using the `service.rs` + `service/` layout)
- Test: `crates/hydra-swarms/tests/claims_nodes.rs`

**Interfaces:**
- Consumes: Task 2's types.
- Produces:
  - `paths::CLAIMS_PREFIX = "claims/"`, plus `claim_key`, `done_key`, `release_key(context, key, target)` and `claims_context_prefix(context)`
  - `wire::CLAIMS_ALPN = b"hydra/claims/1"`
  - `wire::ClaimAsk { claim: ClaimRecord }`
  - `wire::ClaimAnswer { claims: Vec<(String, ClaimRecord)>, dones: Vec<(String, DoneRecord)> }`
  - `NodeEvent::ClaimHeard`, emitted when an ask arrives
  - On `SwarmNode`:
    - `write_claim(&self, &ClaimRecord) -> anyhow::Result<()>`
    - `delete_claim(&self, context: &str, key: &str) -> anyhow::Result<()>`
    - `write_done(&self, &DoneRecord) -> anyhow::Result<()>`
    - `write_release(&self, &ReleaseRecord) -> anyhow::Result<()>`
    - `claim_records(&self, context: &str) -> anyhow::Result<Records>`
    - `ask_claim(&self, endpoints: &[String], claim: &ClaimRecord, timeout: Duration) -> Vec<ClaimAnswer>`, which asks all at once and leaves out any that fail or time out
    - `take_answers(&self, answers: Vec<ClaimAnswer>, roster: &Roster)`, which keeps answered records from writers in `heard`
    - `prune_claims(&self, context: &str, settings: &ContextSettings, now: u64, slack: Duration) -> anyhow::Result<()>`, which deletes this author's lapsed claims, expired dones and stale release records
    - `silent_claims_for_tests(&self)`, after which the handler takes asks and never answers
    - `leave_sync_for_tests(&self)`, which calls `doc.leave()`
  - `Core.heard: Mutex<HashMap<(String, String, String), ClaimRecord>>`, keyed by context, key and author

Rules the implementer can't infer:

- `CLAIMS_ALPN` joins `GATED_ALPNS`, so non-members never reach it. The handler still looks the sender up with `roster.member_by_endpoint`, takes the author from that machine's entry and ignores any author claimed by the sender. It refuses anyone who isn't a writer or owner, by closing the stream.
- The handler stores the claim in `heard` **before** reading what it holds, then answers with everything it holds for that key, its own included. That order is what makes "two claimers that can reach each other each see the other" true.
- `claim_records` reads `claims/<context>/` with `include_empty()`. A tombstone from an author removes that author's `heard` entry for the key. A doc claim replaces a `heard` entry from the same author when its `claimed_at` is the same or newer. Remaining `heard` claims merge in. An entry whose blob isn't here yet is skipped, and `ContentReady` brings a re-read.
- Records over 64 KiB (`MAX_RECORD_BYTES`) are skipped.
- `heard` entries whose `renewed_at` is older than an hour are dropped on read, so a heard claim can never block a key for long even if its context's settings are unknown. The lapse rule in `settle` is what normally ends them.

- [ ] **Step 1: Write the failing tests** in `tests/claims_nodes.rs`, reusing the setup style of `tests/two_nodes.rs` (two writers, synced, relays off):

```rust
#[tokio::test]
async fn two_claims_at_once_settle_on_one_with_sync_held_back() {
    // Both nodes leave_sync_for_tests(). Each writes a claim at the same moment with tokio::join!,
    // then ask_claim + take_answers. Stamps differ: a=200, b=100.
    // settle() on each node's claim_records gives Held by b's author on both.
}
#[tokio::test]
async fn a_question_that_arrives_first_stops_the_other_claiming() {
    // Sync held back. a writes and asks; b has written nothing. b's decide_take is now Held by a.
}
#[tokio::test]
async fn a_silent_member_is_passed_over() {
    // b.silent_claims_for_tests(). ask_claim with a 500 ms timeout returns empty inside 2 s.
}
#[tokio::test]
async fn a_tombstone_clears_a_heard_claim() {
    // Sync held back: a asks b, b holds it in heard. Sync back on; a deletes its claim. Eventually b's
    // settle says the key is free, long before any lapse.
}
#[tokio::test]
async fn claims_stay_out_of_the_folder_entries() {
    // After write_claim, file_entries_for_tests() is unchanged.
}
```

Also add round-trip cases to `wire.rs`'s tests for `ClaimAsk` and `ClaimAnswer`, and path tests: a Message-ID such as `<a/b@x>` gives a key with no extra `/`.

- [ ] **Step 2: Run** `cargo test -p hydra-swarms --test claims_nodes`. Expected: compile failure.
- [ ] **Step 3: Implement** paths, wire, the handler and the node methods.
- [ ] **Step 4: Run** the same command plus `cargo test -p hydra-swarms --lib`. Expected: PASS.
- [ ] **Step 5: Commit** `hydra-swarms: claim records and the ask step`.

### Task 4: The service: claim, renew, lose

**Files:**
- Create: `crates/hydra-swarms/src/service/claims.rs` (`impl Worker` methods and the `ClaimBook`)
- Modify: `crates/hydra-swarms/src/service.rs` (`Command` at :214, `Msg` at :639, `Sent` at :1230, `Worker` at :1246 and its `run` select at :1362, `SwarmView` at :152, `view` at :2412), `crates/hydra-swarms/src/lib.rs`
- Test: `crates/hydra-swarms/tests/claims.rs`

**Interfaces:**
- Consumes: Tasks 1 to 3.
- Produces (re-exported from `lib.rs`):

```rust
pub enum ClaimOutcome {
    Yours(ClaimHandle),
    Held { person: String, machine: String, this_machine: bool, since: u64 },
    Done { person: String, at: u64 },
    Paused,
    AtLimit(i32),
}
pub enum LostReason {
    Beaten { person: String, machine: String },
    Done { person: String },
    Released { person: String },
    Lapsed,
}
pub struct ClaimHandle { /* id, swarm, context, key, lost rx, tokio mpsc sender to the Worker */ }
impl ClaimHandle {
    pub fn swarm(&self) -> &str;
    pub fn context(&self) -> &str;
    pub fn key(&self) -> &str;
    /// Next loss, once; None after the swarm has gone.
    pub async fn lost(&mut self) -> Option<LostReason>;
    pub async fn release(self) -> Result<(), String>;
    pub async fn done(self) -> Result<(), String>;
}
// Drop sends Msg::ClaimDrop: renewals stop and the claim lapses by itself.

// New Command variants. Replies are oneshot, as Invite's is.
Command::ClaimTake { swarm: Option<String>, context: String, key: String, holder: Holder,
                     reply: oneshot::Sender<Result<ClaimOutcome, String>> }
Command::ClaimsList { swarm: Option<String>, context: String,
                      reply: oneshot::Sender<Result<ContextView, String>> }
Command::ClaimRelease { swarm: String, context: String, key: String, author: String,
                        reply: oneshot::Sender<Result<(), String>> }
Command::ClaimContextSet { swarm: String, name: String, settings: ContextSettings }

pub struct ContextView { pub name: String, pub settings: ContextSettings,
                         pub live: Vec<ClaimView>, pub done: Vec<DoneView> }
pub struct ClaimView { pub key: String, pub author: String, pub person: String, pub machine: String,
                       pub renewed_at: u64, pub holder: Holder,
                       pub mine: bool /* held by any of this person's machines */ }
pub struct DoneView { pub key: String, pub person: String, pub at: u64 }
// SwarmView gains: pub claims: Vec<ContextView>   (done lists newest first, at most 20 per context)
ServiceConfig::claim_slack_for_tests(self, Duration) -> Self
```

Where it goes:

- `run()` in `service.rs` resolves `swarm: Option<String>` for `ClaimTake` and `ClaimsList`, with a pure `resolve_swarm(views: &[SwarmView], swarm: Option<&str>, context: &str) -> Result<String, String>`: match by id, then by name; without one, the only view with that context. Error copy: `"No swarm here has a context called {context}."` and `"{context} is in more than one swarm: name which one."`
- The take flow, on the Worker, never blocking the select loop:
  1. Check the role (reader: `Err("Readers can't claim in {name}.")`), the key length and that the context exists.
  2. Answer `Held { this_machine: true, .. }` if the book holds a live handle for this key.
  3. Run `decide_take`. `Go` writes a new record and then asks. `TakeUp` adds the existing record to the book and replies `Yours` at once, without asking, since it is already this machine's. Other results reply straight away.
  4. Run the ask in `self.sends` as `Sent::ClaimAsked { id, answers }`, with its book entry marked `Deciding` so it counts towards the limit.
  5. On `ClaimAsked`: `take_answers`, then `settle`. If this machine won, reply `Yours`. If not, delete its own claim entry and reply `Held` or `Done`.
- `ClaimBook` is a private struct: `live: HashMap<u64, BookEntry>`, with `BookEntry { context, key, claim: ClaimRecord, lost: mpsc sender, reply: Option<oneshot>, deciding: bool, renew_at: Instant }`. The Worker gets a `claims_at: Option<Instant>` branch in its select, as `relay_retry_at` has.
- Renewing: at each entry's `renew_at`, compare `now_micros()` with `claim.renewed_at`. Beyond the lapse time (the machine slept) it is lost with `Lapsed` and its entry is deleted. Otherwise write the record with a fresh `renewed_at`. Use wall-clock time for this, since `Instant` doesn't reliably count through sleep.
- Noticing losses: after `pass()`, after `NodeEvent::ClaimHeard`, and after `ClaimAsked`, re-settle every context the book holds. For each non-deciding entry whose key isn't `Held` by this author, send the `LostReason`, remove the entry and delete the claim entry. It is `Released` when a release record names this claim, `Done` when a done exists, `Beaten` when someone else holds it, otherwise `Lapsed`.
- `view()` reads a cached `self.claims_view: Vec<ContextView>`, refreshed in `pass()`, after any claim command, and on a 15 s timer so lapses show. `prune_claims` runs in `periodic()`.
- `ClaimRelease`: when the author is this machine's own and the book holds it, treat it as a loss with `Released` by this person. Otherwise the caller must be that person or an owner (`Err("Only the machine's person or an owner can release this claim.")`), and it writes a `ReleaseRecord`. The record's `claimed_at` comes from the current `settle`.
- `ClaimContextSet`: goes through `owner_change` as `SetTypes` does, with `"Only owners can change {name}'s claim contexts."`, after `valid_context_name` (`"A context's name uses a-z, 0-9, - and _."`).
- `handle.release()`: delete the entry, remove it from the book. `handle.done()`: write the done record first, then delete the claim entry, then remove it from the book. If the handle already lost, both give `Err("This claim is no longer yours.")`.

- [ ] **Step 1: Write the failing tests** in `tests/claims.rs`, reusing the `Svc` helpers by moving them from `tests/service.rs` into a shared `tests/common/mod.rs` that both files use. All contexts use `renew_secs` 1, `lapse_after` 2, slack 1 s, except where noted.

```rust
async fn a_machine_alone_claims()                     // other member stopped; Yours within 10 s
async fn the_second_machine_is_told_who_holds_it()    // Held { person: "olive", .. }
async fn two_sessions_on_one_machine_do_not_share_a_key() // Held { this_machine: true }
async fn a_holder_that_stops_renewing_lapses_and_another_claims() // drop handle; b gets Yours once lapsed
async fn a_live_handle_keeps_its_claim_past_the_lapse_time()      // 5 s on, b still gets Held
async fn a_woken_machine_is_told_it_lost()            // test hook pushes renewed_at back; lost() = Lapsed
async fn done_stays_done()                            // handle.done(); b gets Done; a's slot is free again
async fn at_limit_and_paused()                        // limit 1 → AtLimit(1); settings 0 → Paused, held one runs on
async fn an_owner_releases_anyones_claim()            // holder's lost() = Released { person: owner }
async fn a_writer_cannot_release_someone_elses()      // Err with the exact copy
async fn a_reader_cannot_claim()                      // Err with the exact copy
async fn a_restart_takes_up_the_machines_own_claim()  // stop service, restart within lapse, Take → Yours, same claimed_at
async fn a_silent_member_does_not_stop_a_claim()      // Yours within 10 s
async fn the_view_shows_claims_and_recent_done()      // SwarmView.claims contents, mine flag
```

Plus a `resolve_swarm` table test in `service.rs`'s test module. Add `Command::ClaimAgeForTests { swarm, context, key, by_secs }` for the woken-machine test, marked `#[doc(hidden)]`. Forgetting a done key after `keep_done` is covered by the pure test in Task 2, since its 60 s minimum makes an end-to-end test too slow.

- [ ] **Step 2: Run** `cargo test -p hydra-swarms --test claims`. Expected: compile failure.
- [ ] **Step 3: Implement** per the above. Keep `service.rs` changes to the new variants, branches and fields; the logic goes in `service/claims.rs`.
- [ ] **Step 4: Run** `cargo test -p hydra-swarms`. Expected: PASS, with `tests/service.rs` and `tests/folders.rs` still green.
- [ ] **Step 5: Commit** `hydra-swarms: claiming, renewing and losing in the service`.

### Task 5: The local bridge between windows

**Files:**
- Create: `crates/hydra-swarms/src/bridge.rs`
- Modify: `crates/hydra-swarms/src/lib.rs`, `crates/hydra-swarms/src/service.rs` (`ServiceConfig`, `SwarmsService::start`, `run`)
- Test: `crates/hydra-swarms/tests/bridge.rs`

**Interfaces:**
- Consumes: Task 4's commands and `ClaimHandle`.
- Produces:

```rust
// Host side: ServiceConfig::bridge_file(self, PathBuf) -> Self. `run` spawns the host and removes the
// file when it ends. The host's commands reach `run` over a second channel, polled beside `commands`,
// so holding them doesn't keep the service alive once hydraui has dropped its sender.

// Client side: blocking, no runtime, safe to call from any thread.
pub struct ClaimsClient;
impl ClaimsClient {
    pub fn start(file: PathBuf, on_lost: Box<dyn Fn(u64, LostReason) + Send>) -> ClaimsClient;
    /// Blocks up to 30 s. Err when the swarms aren't reachable.
    pub fn take(&self, swarm: Option<&str>, context: &str, key: &str, holder: Holder)
        -> Result<TakeReply, String>;
    pub fn release(&self, holder_id: u64) -> Result<(), String>;
    pub fn done(&self, holder_id: u64) -> Result<(), String>;
    /// Ends the holder: renewals stop, the claim lapses. Never blocks.
    pub fn end(&self, holder_id: u64);
    pub fn list(&self, swarm: Option<&str>, context: &str) -> Result<ContextView, String>;
}
pub enum TakeReply {
    Yours { holder_id: u64, swarm: String },
    Held { person: String, machine: String, this_machine: bool, since: u64 },
    Done { person: String, at: u64 },
    Paused,
    AtLimit(i32),
}
```

Protocol: JSON lines, one message per line. The file holds `{"port":…, "secret":"<32 hex>"}`, written after the listener binds on `127.0.0.1:0`. The first line is `Hello { secret }`; a wrong secret closes the connection. `ClientMsg` is `Take`, `Release`, `Done`, `End`, `List`, each with a `req` number except `End`. `HostMsg` is `Reply { req, .. }` or `Lost { holder_id, reason }`. `LostReason`, `Holder` and `ContextView` need `Serialize, Deserialize`.

Behaviour:

- The host keeps `(connection, holder_id) → ClaimHandle`. When a connection ends, every handle on it drops, so those holders stop renewing and lapse.
- The client's thread loops: read the file, connect, send `Hello`, re-`Take` every live holder under its existing `holder_id`, then read until EOF, then wait 2 s and go round again.
- A re-`Take` that gets `Yours` is silent. Anything else fires `on_lost` and forgets the holder. "Not open yet" errors retry every 2 s for up to 60 s, then fire `on_lost(Lapsed)`.
- While disconnected, calls return `Err("The swarms aren't running on this machine right now.")` after a 5 s wait for a connection.

- [ ] **Step 1: Write the failing tests** in `tests/bridge.rs`:

```rust
async fn a_client_claims_and_is_told_it_lost()     // owner releases; on_lost fires with Released
async fn a_wrong_secret_is_refused()               // raw TCP with a bad Hello is closed, no reply
async fn a_closed_client_stops_renewing()          // drop client; after lapse another machine claims
async fn holders_survive_the_host_changing()
    // Service 1 with bridge file F; client takes. Stop service 1, start service 2 on the same root and F
    // within the lapse time. Client reconnects; on_lost NOT fired; a second machine still gets Held after
    // the lapse time has passed; the claim's claimed_at is unchanged.
async fn a_client_with_no_host_gets_a_plain_error()
```

- [ ] **Step 2: Run** `cargo test -p hydra-swarms --test bridge`. Expected: compile failure.
- [ ] **Step 3: Implement** `bridge.rs` and the `ServiceConfig` and `run` wiring.
- [ ] **Step 4: Run** `cargo test -p hydra-swarms`. Expected: PASS.
- [ ] **Step 5: Commit** `hydra-swarms: a local bridge so every window can claim`.

### Task 6: MCP tools

**Files:**
- Create: `crates/hydraui/src/mcp/claims_tools.rs`
- Modify:
  - `crates/hydraui/src/mcp/mod.rs` (`McpTerminal::drop` at :178, module list)
  - `crates/hydraui/src/mcp/server.rs` (`McpEvent` at :21, `Server` at :346, `serve` at :406)
  - `crates/hydraui/src/mcp/tools.rs` (`INSTRUCTIONS`, `list` chain at :129)
  - `crates/hydraui/src/workspace.rs` (pump at :437, startup)
  - `crates/hydraui/src/workspace/swarms.rs` (`start` at :132: add `.bridge_file(...)`)
  - `crates/hydraui/src/workspace/orchestrator.rs` (a small `claim_lost` beside `queue_wake` at :1314)

**Interfaces:**
- Consumes: `ClaimsClient`, `TakeReply`, `LostReason`.
- Produces:

```rust
pub trait ClaimsApi: Send + Sync {
    fn take(&self, swarm: Option<&str>, context: &str, key: &str, holder: Holder) -> Result<TakeReply, String>;
    fn release(&self, holder_id: u64) -> Result<(), String>;
    fn done(&self, holder_id: u64) -> Result<(), String>;
    fn end(&self, holder_id: u64);
    fn list(&self, swarm: Option<&str>, context: &str) -> Result<ContextView, String>;
}
impl ClaimsApi for ClaimsClient { /* forwards */ }

pub struct ClaimsSessions { /* api, Mutex<HashMap<token, Vec<SessionClaim>>> */ }
impl ClaimsSessions {
    pub fn new(api: Arc<dyn ClaimsApi>) -> Self;
    pub fn call(&self, token: &str, terminal: &Terminal, name: &str, args: &Value) -> Option<Outcome>;
    pub fn terminal_closed(&self, token: &str);
    /// Forgets the holder and gives (token, text to type) for a loss.
    pub fn lost(&self, holder_id: u64, reason: &LostReason) -> Option<(String, String)>;
}
pub fn schemas() -> Vec<Value>;
pub const CLAIMS_FILE: &str = "swarms-claims.json";   // under crate::claim::running_dir(paths)
// McpEvent::ClaimLost { token: String, text: String }
// Server::set_claims(&self, Arc<ClaimsSessions>), Server::claims(&self) -> Option<Arc<ClaimsSessions>>
```

Tools (the holder label is the terminal's name, the kind `HolderKind::Session`):

| Tool | Arguments | Result text |
|---|---|---|
| `claims_take` | `context`, `key`, optional `swarm` | `You hold {key} in {context}. Work on it, then call claims_done. hydraui keeps the claim alive while this terminal is open.` / `{person} ({machine}) holds {key}. Don't work on it.` / `This machine already holds {key}, in another terminal. Don't work on it.` / `{key} was finished by {person}. Don't work on it again.` / `{context} is paused.` / `This machine is at its limit of {n} in {context}. Finish one first.` |
| `claims_release` | `key`, optional `context` | `Released {key}.` |
| `claims_done` | `key`, optional `context` | `{key} is done.` |
| `claims_list` | `context`, optional `swarm` | JSON of the `ContextView` |

The non-`Yours` outcomes of `claims_take` are **not** errors (`is_error: false`), since they are normal answers. `claims_release` and `claims_done` find the claim among the caller's own; when the key alone matches several, they give the error `Say which context: {a}, {b}.`

Lost text, typed into the session's prompt:

`Your claim on {key} in {context} has gone: {why}. Stop working on it.` where `{why}` is one of `{person} ({machine}) claimed it first`, `{person} finished it`, `{person} released it`, `it lapsed while this machine couldn't renew it`.

Wiring:

- Add to `INSTRUCTIONS`: `Claims stop two teammates' sessions working one job. When the user asks you to work something shared by claiming it, call claims_take first and only start if it says you hold it; finish with claims_done, or claims_release if you stop. If hydraui tells you a claim has gone, stop that work.`
- In `Server::serve`, after reading the body: if it is a `tools/call` whose name starts with `claims_`, move the request to a new thread named `hydraui-mcp-claims`, which calls `ClaimsSessions::call`, responds and broadcasts nothing. Unknown or absent `claims` state gives the error `Swarms aren't set up on this machine.`
- `McpTerminal::drop` calls `terminal_closed(token)` outside the registry lock.
- Workspace startup: `ClaimsClient::start(running_dir(paths).join(CLAIMS_FILE), on_lost)`, once per process and before any terminal can call, where `on_lost` calls `sessions.lost(..)` and broadcasts `McpEvent::ClaimLost`. `swarms::start` adds `.bridge_file(same path)`.
- The pump handles `ClaimLost`: find the pane by `mcp_token`, then `queue_wake(pane, "claims".into(), text)` and `send_wakes`, as `runs_changed` does at :855. `"claims"` matches no run, so the paused check passes. Read `join_wake` first, to confirm the label reads sensibly when wakes join.

- [ ] **Step 1: Write the failing tests** in `claims_tools.rs` with a fake `ClaimsApi`, and in `server.rs`:

```rust
fn take_tells_a_session_it_holds_the_key()         // text, is_error false, sessions remember holder_id
fn take_says_who_holds_it_without_an_error()       // Held → the exact copy, is_error false
fn done_and_release_find_the_callers_own_claim()   // another token's key → is_error true
fn the_same_key_in_two_contexts_asks_which()       // "Say which context: a, b."
fn closing_a_terminal_ends_its_claims()            // fake records end(id) for each
fn a_loss_gives_the_token_and_the_typed_text()     // exact copy for all four reasons
fn the_tools_are_listed()                          // schemas() names the four tools with required args
fn a_slow_claim_does_not_hold_up_other_tools()     // real Server + fake take that sleeps 2 s; a tools/list sent
                                                   // during it answers in under 1 s
fn without_claims_the_tools_say_so()               // no set_claims → the exact copy
```

- [ ] **Step 2: Run** `cargo test -p hydraui mcp::`. Expected: compile failure.
- [ ] **Step 3: Implement** per the above.
- [ ] **Step 4: Run** `cargo test -p hydraui mcp::`. Expected: PASS. Then `cargo build -p hydraui`. Expected: builds.
- [ ] **Step 5: Commit** `hydraui: claims_* tools for sessions`.

### Task 7: The Claims section

**Files:**
- Create: `crates/hydraui/src/workspace/claims_page.rs`
- Modify: `crates/hydraui/src/workspace.rs` (module list), `crates/hydraui/src/workspace/swarms_page.rs` (`swarm_page` at :778: add `.child(self.swarm_claims(swarm, owner, &tz, now, cx))` between members and types; test helper `swarm` at :1678 gains `claims: Vec::new()`)

**Interfaces:**
- Consumes: `SwarmView.claims`, `Command::ClaimRelease`, `Command::ClaimContextSet`.
- Produces, pure and tested:

```rust
fn can_release(claim: &ClaimView, owner: bool) -> bool;         // claim.mine || owner
fn claim_line(claim: &ClaimView, now: u64) -> String;           // "Olive · olive-pc · terminal api · renewed 12 s ago"
fn holder_word(h: &Holder) -> String;                           // "terminal api" / "pipeline run INTH-9"
fn parse_keep_done(typed: &str) -> Result<u64, String>;         // "30d", "12h", "90m"; else the copy below
fn show_keep_done(secs: u64) -> String;                         // 2_592_000 → "30 days"
fn parse_limit(typed: &str) -> Result<i32, String>;             // -1 and up
fn settings_line(s: &ContextSettings) -> String;
    // "Renew every 60 s · lapse after 3 missed · 1 per machine · keep done 30 days"
    // with "no limit per machine" for -1 and "paused" for 0
```

The section, in the HydraUI look through the helpers already in `swarms_page.rs`:

- `section_label("CLAIMS", cx)`, then one block per context: its name in monospace and `settings_line`.
- Live claims: monospace key, `claim_line`, and a ghost `Release` button when `can_release`. Clicking it opens `swarms_confirm` with `Release {key}? {person}'s work on it will be told to stop.` for someone else's claim; your own claims release without asking.
- Recent done: `{key} · {person} · {when}`, using the existing `when`.
- For owners: `Change settings…`, one `text_dialog` per field (four small ghost buttons named after the settings), and `+ New context`. `text_dialog` validates through `claims::valid_context_name`.
- Beside the `keep_done` value, muted: `Keep done keys longer than anything rescanning this context looks back, or an old item can be claimed again.`
- When the swarm has no contexts: owners see `No contexts yet. A context names one kind of job, such as inbox-support.`; everyone else sees no Claims section at all.

Copy for errors: `parse_keep_done`: `Say how long, like 30d, 12h or 90m. The least is 1m.`; `parse_limit`: `A number: 1 or more, -1 for no limit, 0 to pause.`

- [ ] **Step 1: Write the failing tests** in `claims_page.rs`:

```rust
fn your_own_claims_and_owners_get_a_release_button() // mine/owner truth table
fn a_claim_reads_who_where_what_and_when()           // exact claim_line
fn keep_done_reads_days_hours_and_minutes()          // parse + show round trip; "0m", "x" are errors; 1m is the least
fn the_limit_is_a_number_of_minus_one_or_more()      // -2 and "a" are errors
fn settings_read_as_a_sentence()                     // the default line exactly; -1 and 0 forms
```

- [ ] **Step 2: Run** `cargo test -p hydraui claims_page`. Expected: compile failure.
- [ ] **Step 3: Implement** the helpers and `swarm_claims`.
- [ ] **Step 4: Run** `cargo test -p hydraui`. Expected: PASS.
- [ ] **Step 5: Look at it in the running app.** Start a dev build with its own `HYDRA_HOME`, stop it afterwards by PID or with `scripts\stop-dev-hydraui.ps1`, and check by hand:
  - Create a context, see the default settings line, then change each setting.
  - In a Claude session in that build, `claims_take` shows the claim on the swarm page within a second.
  - `claims_done` moves it to Recent done.
  - Open a second window: `claims_take` from a terminal there works, and closing the first window leaves the claim held with no message to the session.
  - Closing the terminal lets the claim lapse after about 3.5 minutes.
- [ ] **Step 6: Commit** `hydraui: the Claims section on a swarm's page`.

### Task 8: Finish

**Files:**
- Modify: `docs/superpowers/specs/2026-11-04-swarm-claims-design.md` (§5.3 and §10 for the builder's choices; §7 for settings living in `log/`)

- [ ] **Step 1: Update the spec** so §7 says context settings are signed changes in the log, §5.3 says `swarm` is an optional argument, and §10 lists the choices from this plan's table.
- [ ] **Step 2: Run everything.** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace --profile ci`. Expected: all clean.
- [ ] **Step 3: Commit** `docs: swarm claims spec matches what was built`.
