# hydraui Task Board Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A per-project Kanban board of idea cards, stored in `<project>/.hydra/tasks.md`, opened as a **Tasks** tab beside a terminal from the terminal row's right-click menu.

**Architecture:** A pure `tasks` module parses and writes the markdown file and applies board changes by card identity. A pure `tasks_project` module finds a terminal's project, commits a change to disk safely (re-read, apply, atomic write) and edits `.gitignore`. A gpui `TaskBoard` entity per project holds the board and watches the file; a `TaskBoardView` renders the columns. The workspace keeps a per-terminal Tasks tab state, shows a two-tab strip over the middle, and saves the state in the layout file.

**Tech Stack:** Rust, gpui + gpui-component 0.7.3, `notify` (file watching), `tempfile` (tests), `serde`/TOML (layout file).

**Spec:** `docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md` (builds on `docs/superpowers/specs/2026-10-19-hydraui-environment-tabs-design.md`).

## Global Constraints

- The file is `<project>/.hydra/tasks.md`; columns in this order, headings exactly: `## Backlog`, `## Pickup`, `## InProgress`, `## Done`, under `# Tasks`.
- A card is one paragraph of plain text, no title. Written as `- ` + text, wrapped continuation lines indented two spaces (wrap at 100 columns).
- Never drop a card; never refuse the file; never write outside the project folder.
- The file and `.hydra` are created on the first card saved with text, never on opening the board.
- Writes are atomic: use `hydraui_sets::layout::write_atomic`.
- Layout files stay `version = 2`.
- No GUI automation or synthetic keystrokes from subagents (ruling from the previous plan: a subagent once typed into the user's live window). UI is checked by the user running the app.
- CI trio must pass: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Commit trailer:
  ```
  Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01H41b1nP3WW5B2BmVdtooEE
  ```

## Review Focus

1. Claude edits `tasks.md` while the user is typing in a card: Claude's card appears, the user's edit is kept and saved on top of Claude's version (Task 1 `a_change_applies_to_a_file_changed_underneath`, Task 2 `commit_rereads_the_file`).
2. Hand-written variants (`* idea`, `- [ ] idea`, `## In Progress`, wrapped and nested lines, text before `## Backlog`) load without losing a card (Task 1 `forgiving_variants_keep_every_card`, `extra_text_survives_a_round_trip`).
3. Two cards with identical text: moving or deleting the second one touches the second, not the first (Task 1 `identical_cards_are_told_apart_by_occurrence`).
4. A terminal outside any git repository: the board lives in that folder and no commit/private question is asked (Task 2 `outside_a_repo_the_folder_is_the_project`, Task 5 `asks_git` false).
5. A `.gitignore` with no trailing newline, or already ignoring the file by another rule: **Keep private** appends on its own line; **Commit tasks** removes only hydraui's exact line (Task 2 `keep_private_appends_on_its_own_line`, `commit_tasks_removes_only_our_line`).

---

### Task 1: The tasks file — parse, write, apply changes

**Files:**
- Create: `crates/hydraui/src/tasks.rs`
- Modify: `crates/hydraui/src/lib.rs` (add `pub mod tasks;`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum Column { Backlog, Pickup, InProgress, Done }
  impl Column { pub const ALL: [Column; 4]; pub fn heading(self) -> &'static str; /* "Backlog", "Pickup", "InProgress", "Done" */ }

  #[derive(Debug, Clone, Default, PartialEq)]
  pub struct Board {
      /// Cards per column, indexed by `Column as usize`, top first.
      pub columns: [Vec<String>; 4],
      /// Everything that isn't a card under a known column, verbatim, in original order.
      pub extra: String,
  }
  impl Board {
      pub fn cards(&self, column: Column) -> &[String];
      pub fn card_ref(&self, column: Column, index: usize) -> Option<CardRef>;
  }

  /// A card by its column, text and which occurrence of that text it is in the column (0-based).
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct CardRef { pub column: Column, pub text: String, pub nth: usize }

  #[derive(Debug, Clone, PartialEq)]
  pub enum Change {
      /// A new card at the top of Backlog.
      Add { text: String },
      Edit { card: CardRef, text: String },
      /// To `index` in `to` (clamped to the column's length).
      Move { card: CardRef, to: Column, index: usize },
      Delete { card: CardRef },
      ClearDone,
  }

  /// The card a change names isn't in the board any more.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Missing;

  pub fn parse(text: &str) -> Board;
  pub fn write(board: &Board) -> String;
  /// Applies `change`; an `Edit` to empty text deletes the card; text is normalised (trimmed,
  /// whitespace runs incl. newlines collapsed to one space); an `Add` of empty text is a no-op.
  pub fn apply(board: &mut Board, change: &Change) -> Result<(), Missing>;
  ```

- [ ] **Step 1: Write the failing tests** (in `tasks.rs` `#[cfg(test)] mod tests`)

```rust
const CANONICAL: &str = "# Tasks\n\n## Backlog\n- Let the terminal list show unread output, maybe a dot\n  that clears when you look at it.\n\n## Pickup\n- Ctrl+K opens the launcher.\n\n## InProgress\n\n## Done\n- Collapse button.\n";

#[test]
fn canonical_file_parses() {
    let b = parse(CANONICAL);
    assert_eq!(b.cards(Column::Backlog), ["Let the terminal list show unread output, maybe a dot that clears when you look at it."]);
    assert_eq!(b.cards(Column::Pickup), ["Ctrl+K opens the launcher."]);
    assert!(b.cards(Column::InProgress).is_empty());
    assert_eq!(b.cards(Column::Done), ["Collapse button."]);
    assert_eq!(b.extra, "");
}

#[test]
fn canonical_file_round_trips() { assert_eq!(write(&parse(CANONICAL)), CANONICAL); }

#[test]
fn empty_board_writes_all_four_columns() {
    assert_eq!(write(&Board::default()), "# Tasks\n\n## Backlog\n\n## Pickup\n\n## InProgress\n\n## Done\n");
}

#[test]
fn long_cards_wrap_at_100_columns_with_two_space_continuation() {
    let mut b = Board::default();
    let text = "word ".repeat(40).trim().to_string();
    b.columns[0].push(text.clone());
    let out = write(&b);
    assert!(out.lines().all(|l| l.chars().count() <= 100));
    assert!(out.lines().any(|l| l.starts_with("  word")));
    assert_eq!(parse(&out).cards(Column::Backlog), [text]);
}

#[test]
fn forgiving_variants_keep_every_card() {
    let text = "## backlog\n* star idea\n+ plus idea\n- [ ] box idea\n- [x] done box\n## In Progress\n- parent\n  - nested child\n## Done\n";
    let b = parse(text);
    assert_eq!(b.cards(Column::Backlog), ["star idea", "plus idea", "box idea", "done box"]);
    assert_eq!(b.cards(Column::InProgress), ["parent nested child"]);
    assert!(b.cards(Column::Pickup).is_empty());
}

#[test]
fn extra_text_survives_a_round_trip() {
    let text = "Intro line\n\n## Backlog\n- a\nloose prose\n## Notes\n- not a card\n";
    let b = parse(text);
    assert_eq!(b.cards(Column::Backlog), ["a"]);
    assert!(b.extra.contains("Intro line") && b.extra.contains("loose prose"));
    assert!(b.extra.contains("## Notes\n- not a card"));
    let again = parse(&write(&b));
    assert_eq!(again, b);
}

#[test]
fn identical_cards_are_told_apart_by_occurrence() {
    let mut b = parse("## Backlog\n- same\n- other\n- same\n");
    let second = b.card_ref(Column::Backlog, 2).unwrap();
    assert_eq!(second.nth, 1);
    apply(&mut b, &Change::Move { card: second, to: Column::Done, index: 0 }).unwrap();
    assert_eq!(b.cards(Column::Backlog), ["same", "other"]);
    assert_eq!(b.cards(Column::Done), ["same"]);
}

#[test]
fn changes_do_what_they_say() {
    let mut b = parse(CANONICAL);
    apply(&mut b, &Change::Add { text: "  new\n idea ".into() }).unwrap();
    assert_eq!(b.cards(Column::Backlog)[0], "new idea");
    let r = b.card_ref(Column::Pickup, 0).unwrap();
    apply(&mut b, &Change::Edit { card: r, text: "edited".into() }).unwrap();
    assert_eq!(b.cards(Column::Pickup), ["edited"]);
    let r = b.card_ref(Column::Pickup, 0).unwrap();
    apply(&mut b, &Change::Edit { card: r, text: "   ".into() }).unwrap();
    assert!(b.cards(Column::Pickup).is_empty(), "empty edit deletes");
    let r = b.card_ref(Column::Backlog, 0).unwrap();
    apply(&mut b, &Change::Move { card: r, to: Column::Backlog, index: 99 }).unwrap();
    assert_eq!(b.cards(Column::Backlog).last().unwrap(), "new idea");
    apply(&mut b, &Change::ClearDone).unwrap();
    assert!(b.cards(Column::Done).is_empty());
    apply(&mut b, &Change::Add { text: "".into() }).unwrap();
    assert_eq!(b.cards(Column::Backlog).len(), 2, "empty add is a no-op");
}

#[test]
fn a_change_applies_to_a_file_changed_underneath() {
    // The board read CANONICAL; meanwhile Claude added a card to Pickup.
    let seen = parse(CANONICAL);
    let card = seen.card_ref(Column::Pickup, 0).unwrap();
    let mut fresh = parse(&CANONICAL.replace("## Pickup\n", "## Pickup\n- from claude\n"));
    apply(&mut fresh, &Change::Move { card, to: Column::InProgress, index: 0 }).unwrap();
    assert_eq!(fresh.cards(Column::Pickup), ["from claude"]);
    assert_eq!(fresh.cards(Column::InProgress), ["Ctrl+K opens the launcher."]);
}

#[test]
fn a_change_to_a_vanished_card_is_missing() {
    let seen = parse(CANONICAL);
    let card = seen.card_ref(Column::Done, 0).unwrap();
    let mut fresh = parse("## Done\n");
    assert_eq!(apply(&mut fresh, &Change::Delete { card }), Err(Missing));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p hydraui tasks::`
Expected: FAIL to compile (`parse`, `Column` not defined).

- [ ] **Step 3: Implement `tasks.rs`**

Line-based parser: a heading line (`#`+ space) starts a section; a level-2 heading whose text, lowercased with spaces removed, equals a column's lowercased heading selects that column; any other heading (including `# Tasks`, which is dropped and re-written) ends the column and goes to `extra`. Inside a column, a line starting at column 0 with `- `, `* ` or `+ ` starts a card (strip a leading `[ ] ` / `[x] ` / `[X] `); following lines that are indented (any leading whitespace, including nested list markers, whose marker is stripped) continue it; blank lines end it; any other line goes to `extra`. `extra` keeps original lines joined with `\n`; written after `## Done`'s cards, separated by one blank line, only when non-empty. `# Tasks` itself is not kept in `extra`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hydraui tasks::`
Expected: PASS (10 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/hydraui/src/tasks.rs crates/hydraui/src/lib.rs
git commit -m "feat: hydraui reads and writes a project's tasks.md board"
```

---

### Task 2: The project, committing to disk, and `.gitignore`

**Files:**
- Create: `crates/hydraui/src/tasks_project.rs`
- Modify: `crates/hydraui/src/lib.rs` (add `pub mod tasks_project;`)

**Interfaces:**
- Consumes: Task 1 `Board`, `Change`, `Missing`, `parse`, `write`, `apply`.
- Produces:
  ```rust
  /// The git work tree containing `dir` (nearest ancestor-or-self with a `.git` dir or file),
  /// else `dir` itself.
  pub fn project_root(dir: &Path) -> PathBuf;
  /// Whether `project` is a git work tree (has `.git` itself).
  pub fn is_repo(project: &Path) -> bool;
  pub fn tasks_path(project: &Path) -> PathBuf; // project/.hydra/tasks.md
  pub const IGNORE_LINE: &str = ".hydra/tasks.md";

  pub struct Committed {
      pub board: Board,
      /// The exact text now on disk.
      pub text: String,
      /// This write created the file.
      pub created: bool,
  }
  #[derive(Debug)]
  pub enum CommitError { Missing, Io(std::io::Error) }
  /// Re-reads the file (absent = empty board), applies `change`, writes atomically (creating
  /// `.hydra`) unless the result equals what was read and the file exists — so an empty Add on
  /// a missing file creates nothing.
  pub fn commit(project: &Path, change: &Change) -> Result<Committed, CommitError>;
  /// Reads the board; absent file = `(Board::default(), String::new())`.
  pub fn load(project: &Path) -> std::io::Result<(Board, String)>;

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Privacy { Committed, PrivateByUs, IgnoredElsewhere }
  /// `PrivateByUs` when `.gitignore` at the project root has `IGNORE_LINE` as a whole line;
  /// `IgnoredElsewhere` when the `ignore` crate's gitignore rules (as `git_watch::ignore_rules`)
  /// ignore the file anyway; else `Committed`.
  pub fn privacy(project: &Path) -> Privacy;
  pub fn keep_private(project: &Path) -> std::io::Result<()>;
  pub fn commit_tasks(project: &Path) -> std::io::Result<()>;
  ```

- [ ] **Step 1: Write the failing tests** (tempfile-based)

```rust
#[test]
fn a_repo_subfolder_belongs_to_the_repo() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    std::fs::create_dir_all(d.path().join("a/b")).unwrap();
    assert_eq!(project_root(&d.path().join("a/b")), d.path());
    assert!(is_repo(d.path()));
}
#[test]
fn a_linked_worktree_is_its_own_project() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    let wt = d.path().join("wt");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();
    assert_eq!(project_root(&wt.join("src")), wt);
}
#[test]
fn outside_a_repo_the_folder_is_the_project() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(project_root(d.path()), d.path());
    assert!(!is_repo(d.path()));
}
#[test]
fn the_first_card_creates_the_file() {
    let d = tempfile::tempdir().unwrap();
    let c = commit(d.path(), &Change::Add { text: "".into() }).unwrap();
    assert!(!c.created && !tasks_path(d.path()).exists(), "empty add creates nothing");
    let c = commit(d.path(), &Change::Add { text: "idea".into() }).unwrap();
    assert!(c.created);
    assert_eq!(std::fs::read_to_string(tasks_path(d.path())).unwrap(), c.text);
    assert_eq!(c.board.cards(Column::Backlog), ["idea"]);
}
#[test]
fn commit_rereads_the_file() {
    let d = tempfile::tempdir().unwrap();
    commit(d.path(), &Change::Add { text: "mine".into() }).unwrap();
    let (seen, _) = load(d.path()).unwrap();
    std::fs::write(tasks_path(d.path()), "## Backlog\n- mine\n## Pickup\n- claude's\n").unwrap();
    let card = seen.card_ref(Column::Backlog, 0).unwrap();
    let c = commit(d.path(), &Change::Edit { card, text: "mine, edited".into() }).unwrap();
    assert_eq!(c.board.cards(Column::Backlog), ["mine, edited"]);
    assert_eq!(c.board.cards(Column::Pickup), ["claude's"]);
}
#[test]
fn keep_private_appends_on_its_own_line() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    std::fs::write(d.path().join(".gitignore"), "target").unwrap();
    keep_private(d.path()).unwrap();
    keep_private(d.path()).unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join(".gitignore")).unwrap(), "target\n.hydra/tasks.md\n");
    assert_eq!(privacy(d.path()), Privacy::PrivateByUs);
}
#[test]
fn keep_private_creates_gitignore() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    keep_private(d.path()).unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join(".gitignore")).unwrap(), ".hydra/tasks.md\n");
}
#[test]
fn commit_tasks_removes_only_our_line() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    std::fs::write(d.path().join(".gitignore"), "target\n.hydra/tasks.md\n# keep\n").unwrap();
    commit_tasks(d.path()).unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join(".gitignore")).unwrap(), "target\n# keep\n");
    assert_eq!(privacy(d.path()), Privacy::Committed);
}
#[test]
fn another_rule_counts_as_ignored_elsewhere() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    std::fs::write(d.path().join(".gitignore"), ".hydra/\n").unwrap();
    assert_eq!(privacy(d.path()), Privacy::IgnoredElsewhere);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p hydraui tasks_project::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `tasks_project.rs`** with the signatures above. `.gitignore` edits preserve every other line byte-for-byte and the file's existing line ending style is not required (write `\n`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hydraui tasks_project::`
Expected: PASS (9 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/hydraui/src/tasks_project.rs crates/hydraui/src/lib.rs
git commit -m "feat: hydraui finds a terminal's project and keeps its tasks.md committed or private"
```

---

### Task 3: Layout file remembers a terminal's Tasks tab

**Files:**
- Modify: `crates/hydraui-sets/src/layout.rs` (`TerminalFile`, `PlannedTerminal`, `plan_layout`, tests)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "lowercase")]
  pub enum TasksTab { Open, Shown }
  // TerminalFile gains:
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tasks: Option<TasksTab>,
  // PlannedTerminal gains:
  pub tasks: Option<TasksTab>,
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_terminals_tasks_tab_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("l.hydraui");
    let layout = parse_layout("version = 2\n[[env]]\nname = \"work\"\n[[env.terminal]]\ncwd = \"~\"\ntasks = \"shown\"\n[[env.terminal]]\ncwd = \"~\"\n").unwrap();
    assert_eq!(layout.envs[0].terminals[0].tasks, Some(TasksTab::Shown));
    assert_eq!(layout.envs[0].terminals[1].tasks, None);
    write_layout(&p, &layout).unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(text.matches("tasks").count(), 1, "omitted when none: {text}");
    assert_eq!(read_layout(&p).unwrap(), layout);
}
```

Also extend the existing `plan_layout` test that checks a planned terminal's `title` to assert `tasks` is carried into `PlannedTerminal` (e.g. `Some(TasksTab::Open)` from `tasks = "open"`), and that an unopened terminal's `TerminalFile` in `plan.unopened` keeps its `tasks` value.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p hydraui-sets layout::`
Expected: FAIL to compile (`TasksTab` not defined).

- [ ] **Step 3: Add the field and carry it through `plan_layout`**; update every `TerminalFile { .. }` / `PlannedTerminal { .. }` literal in the workspace (`crates/hydraui/src/workspace/layout_file.rs` save path writes `tasks: None` for now; Task 5 fills it).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hydraui-sets/src/layout.rs crates/hydraui/src/workspace/layout_file.rs
git commit -m "feat: layout files remember a terminal's Tasks tab"
```

---

### Task 4: The board entity and view

**Files:**
- Create: `crates/hydraui/src/task_board.rs` (entity: board state + watcher)
- Create: `crates/hydraui/src/task_board_view.rs` (rendering and interaction)
- Modify: `crates/hydraui/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 (`Board`, `Column`, `CardRef`, `Change`), Task 2 (`load`, `commit`, `Committed`, `CommitError`, `privacy`, `keep_private`, `commit_tasks`, `is_repo`, `tasks_path`).
- Produces:
  ```rust
  // task_board.rs
  pub struct TaskBoard { /* project, board, last text, watcher, editing guard */ }
  pub enum TaskBoardEvent {
      /// The first card was just written to a new file in a git repository: ask commit/private.
      AskGit,
      /// A change couldn't apply (card gone) or the write failed: toast this text.
      Problem(String),
  }
  impl EventEmitter<TaskBoardEvent> for TaskBoard {}
  impl TaskBoard {
      pub fn new(project: PathBuf, cx: &mut Context<Self>) -> Self;
      pub fn project(&self) -> &Path;
      pub fn board(&self) -> &Board;
      /// Commits `change` (Task 2 `commit`), updates the board, emits AskGit when
      /// `created && is_repo(project)`, Problem on `Missing` ("That card changed in the file; your change wasn't applied.") or IO error.
      pub fn change(&mut self, change: Change, cx: &mut Context<Self>);
  }

  // task_board_view.rs
  pub struct TaskBoardView { /* board: Entity<TaskBoard>, editing: Option<(Option<CardRef>, Entity<InputState>)>, focus */ }
  impl TaskBoardView {
      pub fn new(board: Entity<TaskBoard>, window: &mut Window, cx: &mut Context<Self>) -> Self;
  }
  impl Render for TaskBoardView {}
  impl Focusable for TaskBoardView {}
  ```

- [ ] **Step 1: Watcher and reload in `TaskBoard`.** `notify::recommended_watcher` on the project folder, `RecursiveMode::Recursive`, forwarding only events whose paths include `tasks_path(project)` through an `UnboundedSender<()>` to a spawned task (as `git_watch::GitWatch` and the workspace's `_git_watch` task do). On a signal: `load`; if the text equals the last read/written text, ignore (no echo); else replace the board and `cx.notify()`. The watcher is dropped with the entity.

- [ ] **Step 2: Unit test the echo rule** without gpui: factor `fn needs_reload(last: &str, now: &str) -> bool` and test `needs_reload("a", "a") == false`, `needs_reload("a", "b") == true`.

- [ ] **Step 3: Render the board** (spec §5): `h_flex` of four equal `v_flex` columns, each with its heading and count (`Backlog 3`), scrolling independently (`overflow_y_scroll` with an id per column). Backlog's header has a `+` button: it starts editing a new card at the top (an `InputState` with `.multi_line()`/`auto_grow`, focused) without writing anything. Done's header has a **Clear** button, disabled when empty, which opens a confirm dialog "Clear N done cards?" (Cancel / Clear, danger) then `Change::ClearDone`. A header `⋯` button opens a menu with the privacy action: under `Privacy::Committed` **Keep private** (`keep_private`), under `PrivateByUs` **Commit tasks** (`commit_tasks`), under `IgnoredElsewhere` a disabled item "Ignored by another .gitignore rule"; only when `is_repo(project)`.

- [ ] **Step 4: Cards.** Each card is a rounded box with its full text wrapped. Click → edit in place with the input pre-filled. Enter saves (`Change::Edit`, or `Change::Add` for a new card); Esc cancels; blur saves. While a card is being edited, a reload replaces the board but the editor keeps its text; on save the change goes through `commit`, which applies to the fresh file. Right-click menu: **Move to Backlog/Pickup/InProgress/Done** (each other column, index 0), **Move up**, **Move down** (disabled at the ends), **Delete**.

- [ ] **Step 5: Drag and drop.** `on_drag` with a `DraggedCard { card: CardRef }` value and a small preview; each card and each column body accepts `on_drop::<DraggedCard>`; dropping on a card inserts before it, on a column's empty space appends. `drag_over` shows an insertion highlight. Dropping onto its own position is a no-op (no write). Auto-scroll near the edge: builder's call if gpui makes it cheap; else omit and ledger it.

- [ ] **Step 6: Build and lint**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p hydraui task_board`
Expected: clean; echo-rule test PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/hydraui/src/task_board.rs crates/hydraui/src/task_board_view.rs crates/hydraui/src/lib.rs
git commit -m "feat: hydraui's task board shows a project's cards as four Kanban columns"
```

---

### Task 5: Tasks tab in the workspace

**Files:**
- Create: `crates/hydraui/src/workspace/tasks_tab.rs`
- Modify: `crates/hydraui/src/workspace.rs` (fields, `render_env`, `mod tasks_tab;`, close/restart paths, `focus_active`)
- Modify: `crates/hydraui/src/workspace/terminal_list.rs` (row menu gains **Tasks**)
- Modify: `crates/hydraui/src/workspace/layout_file.rs` (save/restore `tasks`)

**Interfaces:**
- Consumes: Task 3 `TasksTab`, `PlannedTerminal.tasks`; Task 4 `TaskBoard`, `TaskBoardEvent`, `TaskBoardView`; Task 2 `project_root`, `keep_private`.
- Produces (in `Workspace`):
  ```rust
  /// Terminals with a Tasks tab, and whether it's the one shown (`true`) or the terminal.
  tasks_tabs: HashMap<PaneId, bool>,
  /// One board per project, shared by every Tasks tab on it; dropped when no tab uses it.
  boards: HashMap<PathBuf, (Entity<TaskBoard>, Subscription)>,
  /// One view per terminal's Tasks tab.
  board_views: HashMap<PaneId, Entity<TaskBoardView>>,

  pub(super) fn open_tasks(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>);
  pub(super) fn show_tasks(&mut self, id: PaneId, shown: bool, window: &mut Window, cx: &mut Context<Self>);
  pub(super) fn close_tasks(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>);
  /// The pure part: which project a terminal's board belongs to.
  fn tasks_project(cwd: &Path) -> PathBuf; // = tasks_project::project_root(cwd)
  ```

- [ ] **Step 1: Model.** `open_tasks(id)`: project = `project_root(pane.cwd())`; get or create the `TaskBoard` for it (subscribe: `AskGit` → dialog "Keep tasks private?" with body "hydraui made .hydra/tasks.md in <project name>. Commit it with the code, or keep it out of git?" and buttons **Commit tasks** (close) / **Keep private** (`keep_private`, toast on error); `Problem(text)` → warning toast titled "Tasks"); create the `TaskBoardView`; `tasks_tabs.insert(id, true)`; focus the view; autosave. Calling it when the tab exists just shows it. `show_tasks(id, true)` re-resolves the project: if it differs from the view's board, swap to the other project's board (creating it), dropping the old board when no other view uses it. `close_tasks`: remove view and tab, drop unused board, show and focus the terminal, autosave. Closing a terminal (`close_terminal_now`, env close) calls the same clean-up; Restart keeps the Tasks tab.

- [ ] **Step 2: Row menu.** In `terminal_list.rs`, add `PopupMenuItem::new("Tasks")` first in the row's context menu, calling `open_tasks(id)` (and `show_terminal(id)` first if it isn't the shown terminal).

- [ ] **Step 3: Middle tab strip.** In `render_env`, when the shown terminal has a Tasks tab, wrap the terminal element: a `h_flex` strip (height 28px, bottom border) with two tabs — the terminal's label (`label_for`/name as the list shows it) and **Tasks** with ✕ — the selected one highlighted with the environment tint as env tabs are; below it either the terminal view or the board view. The file pane split is unchanged and wraps this whole element. A terminal without a Tasks tab renders exactly as today.

- [ ] **Step 4: Focus and keys.** `focus_active` focuses the board view when the shown terminal's Tasks tab is selected. Ctrl+W while the board has focus closes the Tasks tab (route through `ClosePane` as the file pane does). No new key binding for switching tabs (spec leaves it to the builder; ledger it).

- [ ] **Step 5: Layout file.** Save: `tasks: self.tasks_tabs.get(id).map(|&shown| if shown { TasksTab::Shown } else { TasksTab::Open })`. Open: after a terminal's pane is created, if `t.tasks` is `Some`, `open_tasks` then `show_tasks(id, matches!(t.tasks, Some(TasksTab::Shown)))` without autosaving mid-open (use the existing open-time guard). Saved when a Tasks tab opens, closes, or switches.

- [ ] **Step 6: Unit test the pure part.** In `tasks_tab.rs`: `tasks_project` for a repo subfolder returns the repo root (tempfile + `.git` dir). Plus a layout-file test in `layout_file.rs`'s tests if one exists for `current_layout`, else rely on Task 3's round trip.

- [ ] **Step 7: CI trio**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/hydraui/src/workspace.rs crates/hydraui/src/workspace/tasks_tab.rs crates/hydraui/src/workspace/terminal_list.rs crates/hydraui/src/workspace/layout_file.rs
git commit -m "feat: a terminal's right-click menu opens its project's task board as a Tasks tab"
```

---

### Task 6: Guide and spec notes

**Files:**
- Modify: `site/app.html` (a "Task board" section near the terminal list section)
- Modify: `docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md` (only if implementation rulings changed anything user-visible)

- [ ] **Step 1: Write the guide section**: opening Tasks from a terminal's right-click menu; the four columns and what they're for; adding, editing (Enter saves, Esc cancels), dragging and the card menu; Clear on Done; the file `.hydra/tasks.md` and its format (show the markdown example from the spec); the commit/private question and the `⋯` menu; Claude can edit the file directly and the board updates. Match the existing guide's voice and markup.

- [ ] **Step 2: Commit**

```bash
git add site/app.html docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
git commit -m "docs: hydraui guide covers the task board"
```
