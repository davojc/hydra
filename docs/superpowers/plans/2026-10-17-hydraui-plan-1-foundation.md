# hydraui Plan 1: Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the GPUI stack on Windows, make hydra's crates usable from a GUI without changing the CLI, and build the `hydraui-sets` crate that turns set files into validated pane specs.

**Architecture:** A throwaway spike decides whether GPUI + `alacritty_terminal` + ConPTY works on Windows before any UI is built. hydra-core's `Ctx` gains a `guards` switch and a `Warnings` sink so providers can skip guard hooks and return warnings instead of printing them. `LaunchEnv` exposes its variables as a list for PTY callers. hydra-platform's shell init can emit OSC 7/133 and run a startup command. A new UI-free crate, `hydraui-sets`, parses `~/.hydra/hydraui/sets/*.toml`, resolves environment/cwd/shell for every pane, collects all validation problems, and tracks loaded sets and the session file.

**Tech Stack:** Rust 2024 workspace; existing hydra crates; serde + toml 0.8; thiserror 2; tempfile for tests. Spike only: gpui, alacritty_terminal, portable-pty.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` (roadmap: `docs/superpowers/plans/2026-10-17-hydraui-roadmap.md`).

## Global Constraints

- **Prerequisite:** PowerShell 7 (`pwsh.exe`) must be on PATH before starting (`winget install Microsoft.PowerShell`). Without it, 18 existing hydra-cli tests fail on `main` with `pwsh isn't installed or isn't on PATH`; that's the known baseline, not a regression.
- **The CLI must not change.** With default options, `hydra` writes byte-identical init scripts, gitconfig, hook wrappers and Claude settings, and prints the same warnings (spec §4).
- **hydraui's files** live under `<HYDRA_HOME>/hydraui/` (`sets/<name>.toml`, `session.toml`); hydraui never edits hydra's `config.toml`, `envs/` or `.hydra` files (spec §3.1).
- **Set file key is `env`**; splits have exactly two panes; `split` is `right` or `down`; `ratio` 0.1–0.9, default 0.5; unknown keys are errors (spec §5.1).
- **Environment resolution order:** pane → tab → set → folder binding (spec §5.2).
- **Guard-free gitconfig file name:** `state/<env>/git/gitconfig-noguard` (spec §4.1).
- **Tests** never touch the real `~/.hydra`, `~/.claude`, `~/.claude.json` or global git config: use temp `HydraPaths` and the existing `Fixture` / `common::Home` fences.
- **Commits:**
  - Repo identity (davojc). No `--no-verify`. No employer names.
  - Every commit message ends with:
    ```
    Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
    ```
- **Before every commit, these must pass:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace` (the task's tests plus no new failures)

## Review Focus

1. **A hydraui launch after a CLI launch of the same environment** must leave the CLI's `gitconfig` and hook wrappers untouched, so an open `hydra shell` keeps its guard. Pinned in Task 3.
2. **A set file saved by Notepad (UTF-8 BOM, CRLF line endings):** parses the same as a plain file. Pinned in Task 7.
3. **A pane folder with a space or an apostrophe** (`E:/work/it's here`): OSC 7 still reports the full folder from both pwsh and bash. Pinned in Task 6.
4. **An explicit `env` on a folder bound to another environment:** the set loads; the pane carries `bound_elsewhere`, it isn't a validation error. Pinned in Task 8.
5. **A corrupt or hand-mangled `session.toml`:** hydraui starts with no sets loaded instead of failing. Pinned in Task 9.

---

### Task 1: GPUI spike (throwaway)

A feasibility probe, not product code. Nothing under `spikes/` is committed; only the outcome note is.

**Files:**
- Create (not committed): `spikes/gpui-term/Cargo.toml`, `spikes/gpui-term/src/main.rs`
- Create: `docs/superpowers/notes/2026-10-17-hydraui-spike.md`

**Interfaces:**
- Produces: the note, recording go/no-go, the exact `gpui` version or git revision to pin, whether `gpui-component` builds alongside it, and whether `gpui-terminal` is reusable. Plans 3–4 read it.

- [ ] **Step 1: Keep the spike out of the workspace and git.** The workspace's `members = ["crates/*"]` already excludes `spikes/`. Add a `[workspace]` table to the spike's own manifest so cargo treats it as a separate root, and add `spikes/` to `.git/info/exclude` (local only, not committed):

```bash
mkdir -p spikes/gpui-term/src
echo "spikes/" >> .git/info/exclude
```

`spikes/gpui-term/Cargo.toml`:

```toml
[package]
name = "gpui-term-spike"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
gpui = "0.2.2"
alacritty_terminal = "0.26.0"
portable-pty = "0.9.0"
```

- [ ] **Step 2: Check which gpui `gpui-component` and `gpui-terminal` need.** Run `cargo info gpui-component@0.7.3` and `cargo info gpui-terminal@0.1.0`, and read their `Cargo.toml` on docs.rs. If either depends on gpui from git (Zed's repo) rather than crates.io `0.2.2`, switch the spike to that exact source so all three can coexist; record which in the note.

- [ ] **Step 3: Write the spike.** One window, one pane: spawn `pwsh.exe -NoLogo` in a ConPTY sized 120×30 with `portable_pty::native_pty_system().openpty(PtySize { rows: 30, cols: 120, .. })`, `pair.slave.spawn_command(CommandBuilder::new("pwsh.exe"))`. A reader thread reads `pair.master.try_clone_reader()` into an `alacritty_terminal::vte::ansi::Processor` advancing a `Term<EventProxy>` behind `Arc<FairMutex<_>>` (as Zed's `crates/terminal/src/terminal.rs` does), then notifies the GPUI view. The view renders `term.renderable_content().display_iter` as one text run per row with each cell's foreground colour, and forwards key input as bytes to `pair.master.take_writer()`. On window resize, call `master.resize(..)` and `term.resize(..)`. First try `gpui-terminal` (if Step 2 showed it fits), since it may provide the view outright; write the view by hand only if it doesn't.

Use the pinned gpui's own examples (`examples/` in its crate source, `~/.cargo/registry/src/*/gpui-*/examples`) for the window/app boilerplate; the API changes between versions, so copy from the version you pinned, not from memory.

- [ ] **Step 4: Run it and check each criterion.** `cargo run --release --manifest-path spikes/gpui-term/Cargo.toml`. Record pass/fail for:
  1. The window opens on Windows 11 and shows the pwsh prompt with colour.
  2. Typing, Backspace, arrows, Ctrl+C and Enter work; `Get-ChildItem C:\Windows` scrolls.
  3. Resizing the window resizes the terminal: `$Host.UI.RawUI.WindowSize` matches the new size.
  4. `1..20000 | % { "line $_" }` finishes in under 3 seconds and the window stays responsive.
  5. Release build time, `.exe` size, and any crash or rendering glitch.

- [ ] **Step 5: Write the note.** `docs/superpowers/notes/2026-10-17-hydraui-spike.md`:

```markdown
# hydraui GPUI spike — 2026-10-17

**Decision:** GO | NO-GO (one line why)

| Criterion | Result |
|---|---|
| Window + coloured prompt | |
| Input (typing, arrows, Ctrl+C) | |
| Resize | |
| 20k lines < 3 s, responsive | |
| Build time / exe size | |

**Pin:** gpui = <version or git rev>; gpui-component = <version or "doesn't build: …">; gpui-terminal = <reusable / not, why>; alacritty_terminal = 0.26.0; portable-pty = 0.9.0.

**Notes for plan 3:** <API surprises, Windows-specific issues, code worth keeping>
```

If NO-GO: stop after this task's commit and tell the user. Tasks 2–9 don't depend on the UI stack and may still proceed if the user agrees; plans 3–4 wait for a new stack decision (spec §10: egui + `egui_term` is the fallback).

- [ ] **Step 6: Commit the note only.**

```bash
git status --short   # must show only the note; spikes/ is excluded
git add docs/superpowers/notes/2026-10-17-hydraui-spike.md
git commit -m "docs: hydraui GPUI spike outcome

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Guards switch and returned warnings in `Ctx` / `prepare`

**Files:**
- Modify: `crates/hydra-core/src/provider.rs` (add `Warnings`; `Ctx.guards`, `Ctx.warnings`)
- Modify: `crates/hydra-core/src/resolve.rs` (`PrepareOptions.no_guards`, `LaunchEnv.warnings`, `PrepareError::Failed.warnings`)
- Modify: `crates/hydra-providers/src/testutil.rs` (Fixture fields)
- Modify: `crates/hydra-providers/src/claude.rs` (`warn(..)` → `ctx.warnings.push(..)`)
- Modify: `crates/hydra-cli/src/commands/launch.rs` (print returned warnings), `auth.rs`, `tools.rs`, `whoami.rs` (new `Ctx` fields)
- Test: unit tests in `resolve.rs`

**Interfaces:**
- Produces:
  - `hydra_core::provider::Warnings` with `fn push(&self, w: impl Into<String>)` and `fn take(&self) -> Vec<String>` (`Default`, `Debug`)
  - `Ctx { .., pub guards: bool, pub warnings: &'a Warnings }`
  - `PrepareOptions.no_guards: bool` (default `false` = guards on, the CLI's behaviour)
  - `LaunchEnv.warnings: Vec<String>`
  - `PrepareError::Failed { env, failures, warnings: Vec<String> }`
  - `Fixture.guards: bool` (default `true`), `Fixture.warnings: Warnings`

- [ ] **Step 1: Write the failing tests.** Add to `mod tests` in `crates/hydra-core/src/resolve.rs`:

```rust
    /// A provider that reports what it saw as a warning.
    struct Warner;

    impl Provider for Warner {
        fn id(&self) -> &'static str {
            "warner"
        }
        fn is_configured(&self, _: &EnvConfig) -> bool {
            true
        }
        fn managed_vars(&self) -> &'static [&'static str] {
            &[]
        }
        fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
            ctx.warnings.push(format!("guards {}", ctx.guards));
            Ok(())
        }
        fn contribute(&self, _: &Ctx) -> Result<Contribution, ProviderError> {
            Ok(Contribution::new())
        }
        fn check(&self, _: &Ctx, _: &dyn CommandRunner) -> IdentityReport {
            IdentityReport {
                provider: "warner".into(),
                status: Status::Info,
                detail: String::new(),
            }
        }
    }

    #[test]
    fn warnings_come_back_with_the_launch() {
        let (_d, paths, name) = setup("");
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(Warner)];
        let env = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        assert_eq!(env.warnings, vec!["guards true".to_string()]);
    }

    #[test]
    fn no_guards_reaches_the_providers() {
        let (_d, paths, name) = setup("");
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(Warner)];
        let opts = PrepareOptions {
            no_guards: true,
            ..Default::default()
        };
        let env = run(&paths, &name, &providers, &MemoryStore::default(), &opts).unwrap();
        assert_eq!(env.warnings, vec!["guards false".to_string()]);
    }

    #[test]
    fn warnings_come_back_with_a_failure_too() {
        let (_d, paths, name) = setup("");
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(Warner),
            Box::new(Fake {
                id: "github",
                on: true,
                managed: &[],
                contribution: Contribution::new(),
                fail: Some("no token"),
            }),
        ];
        let PrepareError::Failed { warnings, .. } = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap_err() else {
            panic!("expected Failed")
        };
        assert_eq!(warnings, vec!["guards true".to_string()]);
    }
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydra-core resolve::tests::warnings` — expected: compile errors (`no field warnings`, `no field guards`).

- [ ] **Step 3: Add `Warnings` and the `Ctx` fields.** In `crates/hydra-core/src/provider.rs`, above `pub struct Ctx`:

```rust
/// Notes a launch has for the user (a setting ignored, a backup kept). Providers push them;
/// the caller shows them (the CLI on stderr, hydraui in the pane).
#[derive(Debug, Default)]
pub struct Warnings(std::cell::RefCell<Vec<String>>);

impl Warnings {
    pub fn push(&self, w: impl Into<String>) {
        self.0.borrow_mut().push(w.into());
    }
    pub fn take(&self) -> Vec<String> {
        self.0.take()
    }
}
```

and add to `Ctx` after `secrets`:

```rust
    /// Write the folder guard's hooks (git hook wrappers, Claude's PreToolUse hook). hydraui
    /// launches with `false`.
    pub guards: bool,
    pub warnings: &'a Warnings,
```

In this file's own test (`borrowed_tools_use_the_owners_state_folder`), add `guards: true, warnings: &Warnings::default(),` to the `Ctx` literal.

- [ ] **Step 4: Thread them through `prepare`.** In `crates/hydra-core/src/resolve.rs`:
  - Import `Warnings`: `use crate::provider::{Ctx, Provider, Warnings};`
  - Add to `PrepareOptions` after `config`:
    ```rust
    /// hydraui: leave out the folder guard's hooks (spec: hydraui §4.1).
    pub no_guards: bool,
    ```
  - Add `pub warnings: Vec<String>,` to `LaunchEnv` after `path_drop`, and `.field("warnings", &self.warnings)` to its `Debug` impl.
  - Change the variant to:
    ```rust
    #[error("can't open {env}\n{}", render_failures(.failures))]
    Failed {
        env: EnvName,
        failures: Vec<Failure>,
        /// Warnings providers gave before the launch failed.
        warnings: Vec<String>,
    },
    ```
  - In `prepare`, before `let ctx = Ctx {`: `let warnings = Warnings::default();`; add `guards: !opts.no_guards, warnings: &warnings,` to the `Ctx` literal.
  - In both `return Err(PrepareError::Failed { env: name.clone(), failures, })` sites add `warnings: warnings.take(),`.
  - In the final `Ok(LaunchEnv { .. })` add `warnings: warnings.take(),`.

- [ ] **Step 5: Run the core tests.** `cargo test -p hydra-core` — expected: PASS (the three new tests included).

- [ ] **Step 6: Update the other `Ctx` builders.**
  - `crates/hydra-providers/src/testutil.rs`: import `Warnings`; add fields `pub guards: bool,` and `pub warnings: Warnings,` to `Fixture`, initialised `guards: true, warnings: Warnings::default(),`; in `ctx()` add `guards: self.guards, warnings: &self.warnings,`.
  - `crates/hydra-cli/src/commands/auth.rs`, `tools.rs`, `whoami.rs`, and `print_sign_in_hints` in `launch.rs`: these `Ctx` values never materialise, so add `guards: true, warnings: &Warnings::default(),` (import `hydra_core::provider::Warnings`).

- [ ] **Step 7: Make the Claude provider push instead of print.** In `crates/hydra-providers/src/claude.rs`:
  - In `materialise`, replace `warn(&format!(..))` with `ctx.warnings.push(format!(..))` and `warn(&warning)` with `ctx.warnings.push(warning)`.
  - Change `fn with_guard_hook(text: String) -> String` to `fn with_guard_hook(text: String, warnings: &Warnings) -> String`, replacing its two `warn(&format!(..))` calls with `warnings.push(format!(..))`; the call in `materialise` becomes `with_guard_hook(merged.text, ctx.warnings)`.
  - In the two tests that call `with_guard_hook(` directly, pass `&Warnings::default()` as the second argument.
  - Delete `fn warn`. Remove any import clippy then reports unused.

- [ ] **Step 8: Print the returned warnings in the CLI.** In `crates/hydra-cli/src/commands/launch.rs`, replace the body's `Ok(prepare(..)?)` in `prepare_launch` with:

```rust
    let result = prepare(
        &app.paths,
        name,
        &app.user_home,
        &providers,
        app.store.as_ref(),
        &opts,
    );
    // Same place and colour as when providers printed them themselves.
    let warnings: &[String] = match &result {
        Ok(launch) => &launch.warnings,
        Err(PrepareError::Failed { warnings, .. }) => warnings,
        Err(_) => &[],
    };
    for w in warnings {
        anstream::eprintln!("{}", style::warn(w));
    }
    Ok(result?)
```

and import `PrepareError` from `hydra_core::resolve`.

- [ ] **Step 9: Run everything.** `cargo test --workspace` — expected: all pass except the known pwsh baseline if pwsh is still missing. Then `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 10: Commit.**

```bash
git add crates/hydra-core crates/hydra-providers crates/hydra-cli
git commit -m "feat: guards switch and returned warnings in prepare

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Git provider without guards

**Files:**
- Modify: `crates/hydra-providers/src/git.rs` (`render_gitconfig`, `materialise`, `contribute`)
- Test: unit tests in `git.rs`

**Interfaces:**
- Consumes: `Ctx.guards` (Task 2).
- Produces: `render_gitconfig(name, git, github, user_home, includes, hooks_dir: Option<&Path>) -> String`; with `ctx.guards == false` the provider writes and points `GIT_CONFIG_GLOBAL` at `state/<env>/git/gitconfig-noguard`.

- [ ] **Step 1: Write the failing tests.** Add to `mod tests` in `git.rs`:

```rust
    #[test]
    fn without_guards_a_separate_gitconfig_leaves_the_guarded_one_alone() {
        let mut f = Fixture::new("[git]\nemail = \"a@b.c\"\n");
        git().materialise(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&f.name).join("git");
        let guarded = std::fs::read_to_string(dir.join("gitconfig")).unwrap();
        f.guards = false;
        git().materialise(&f.ctx()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("gitconfig")).unwrap(),
            guarded
        );
        assert!(hooks_dir(&f).join("pre-commit").is_file());
        let free = std::fs::read_to_string(dir.join("gitconfig-noguard")).unwrap();
        assert!(!free.contains("hooksPath"), "{free}");
        assert!(free.contains("email = \"a@b.c\""), "{free}");
        let c = git().contribute(&f.ctx()).unwrap();
        assert_eq!(
            c.vars["GIT_CONFIG_GLOBAL"],
            VarValue::Literal(s(&dir.join("gitconfig-noguard")))
        );
    }

    #[test]
    fn without_guards_no_hook_wrappers_are_written() {
        let mut f = Fixture::new("");
        f.guards = false;
        git().materialise(&f.ctx()).unwrap();
        assert!(!hooks_dir(&f).exists());
    }

    #[test]
    fn without_guards_or_ssh_key_there_is_no_empty_core_section() {
        let mut f = Fixture::new("");
        f.guards = false;
        git().materialise(&f.ctx()).unwrap();
        let free = std::fs::read_to_string(
            f.paths
                .state_dir(&f.name)
                .join("git")
                .join("gitconfig-noguard"),
        )
        .unwrap();
        assert!(!free.contains("[core]"), "{free}");
    }
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydra-providers git::tests::without_guards` — expected: FAIL (no `gitconfig-noguard` file).

- [ ] **Step 3: Make `hooks_dir` optional.** In `render_gitconfig`, change the parameter to `hooks_dir: Option<&Path>` and replace the `[core]` block with:

```rust
    if cfg.ssh_key.is_some() || hooks_dir.is_some() {
        s.push_str("[core]\n");
    }
    if let Some(k) = &cfg.ssh_key {
        let key = fwd(&expand_tilde(k, user_home)).replace('\'', "'\\''");
        s.push_str(&format!(
            "\tsshCommand = {}\n",
            quote(&format!("ssh -i '{key}' -o IdentitiesOnly=yes"))
        ));
    }
    if let Some(h) = hooks_dir {
        s.push_str(&format!("\thooksPath = {}\n", quote(&fwd(h))));
    }
```

Update the doc comment's last sentence to: "`core.hooksPath` points at hydra's hook wrappers when `hooks_dir` is given." In the four existing tests that call `render_gitconfig` directly, wrap the last argument in `Some(..)`.

- [ ] **Step 4: Branch `materialise` and `contribute` on `ctx.guards`.** Add a constant near `MANAGED`:

```rust
/// hydraui's gitconfig: the same settings without the guard's `core.hooksPath`, in its own
/// file so a hydraui launch never switches the guard off for `hydra shell` terminals.
const NOGUARD_FILE: &str = "gitconfig-noguard";
```

Replace the part of `materialise` after the key checks with:

```rust
        let dir = ctx.provider_dir("git");
        let user = self.user_config(ctx);
        let render = |hooks: Option<&Path>| {
            render_gitconfig(
                ctx.name,
                ctx.env.git.as_ref(),
                ctx.env.github.is_some(),
                ctx.user_home,
                &user.files,
                hooks,
            )
        };
        if !ctx.guards {
            std::fs::create_dir_all(&dir)?;
            write_atomic(&dir.join(NOGUARD_FILE), &render(None))?;
            return Ok(());
        }
        let hooks = dir.join("hooks");
        std::fs::create_dir_all(&hooks)?;
        let hydra_exe = std::env::current_exe().map(|p| fwd(&p)).unwrap_or_default();
        let chain_dir = user_hooks_path(&user.files);
        for hook in HOOKS {
            write_atomic(
                &hooks.join(hook),
                &render_hook(ctx.name, hook, &hydra_exe, &chain_dir),
            )?;
        }
        write_atomic(&dir.join("gitconfig"), &render(Some(&hooks)))?;
        Ok(())
```

In `contribute`, replace the first statement with:

```rust
        let file = if ctx.guards { "gitconfig" } else { NOGUARD_FILE };
        let mut c = Contribution::new().path(
            "GIT_CONFIG_GLOBAL",
            &ctx.provider_dir("git").join(file),
        );
```

- [ ] **Step 5: Run the provider tests.** `cargo test -p hydra-providers` — expected: PASS, including every existing gitconfig and hook test (byte-identical output with guards on).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydra-providers/src/git.rs
git commit -m "feat: git provider can leave out the guard hooks

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Claude provider keeps the existing hook without guards

**Files:**
- Modify: `crates/hydra-providers/src/claude_files.rs` (add `guard_hook_command`)
- Modify: `crates/hydra-providers/src/claude.rs` (`keep_guard_hook`, branch in `materialise`)
- Test: unit tests in both files

**Interfaces:**
- Consumes: `Ctx.guards`, `Ctx.warnings`, `with_guard_hook(text, &Warnings)` (Task 2).
- Produces: `claude_files::guard_hook_command(settings_json: &str) -> Option<String>`.

- [ ] **Step 1: Write the failing tests.** In `claude_files.rs` tests:

```rust
    #[test]
    fn finds_the_guard_hook_command() {
        let with = add_guard_hook("{}", "\"C:/x/hydra.exe\" guard claude").unwrap();
        assert_eq!(
            guard_hook_command(&with).as_deref(),
            Some("\"C:/x/hydra.exe\" guard claude")
        );
        assert_eq!(guard_hook_command("{}"), None);
        assert_eq!(guard_hook_command("not json"), None);
    }
```

In `claude.rs` tests:

```rust
    #[test]
    fn without_guards_a_fresh_environment_gets_no_guard_hook() {
        let mut f = Fixture::new("[claude]\n");
        f.guards = false;
        Claude.materialise(&f.ctx()).unwrap();
        let text = std::fs::read_to_string(cfg_dir(&f).join("settings.json")).unwrap();
        assert!(!text.contains("guard claude"), "{text}");
    }

    #[test]
    fn without_guards_the_hook_a_cli_launch_wrote_is_kept() {
        let mut f = Fixture::new("[claude]\n");
        Claude.materialise(&f.ctx()).unwrap();
        let before = std::fs::read_to_string(cfg_dir(&f).join("settings.json")).unwrap();
        f.guards = false;
        Claude.materialise(&f.ctx()).unwrap();
        let after = std::fs::read_to_string(cfg_dir(&f).join("settings.json")).unwrap();
        assert_eq!(after, before);
        assert!(!cfg_dir(&f).join("settings.json.bak").exists());
    }
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydra-providers guard` — expected: compile error (`guard_hook_command` not found), then the first claude test fails (hook present).

- [ ] **Step 3: Add `guard_hook_command`.** In `claude_files.rs`, after `add_guard_hook`:

```rust
/// The command of the guard hook already in the settings JSON, if any.
pub fn guard_hook_command(settings_json: &str) -> Option<String> {
    let root: Value = serde_json::from_str(settings_json).ok()?;
    root["hooks"]["PreToolUse"]
        .as_array()?
        .iter()
        .flat_map(|e| e["hooks"].as_array().into_iter().flatten())
        .filter_map(|h| h["command"].as_str())
        .find(|c| c.contains(" guard claude"))
        .map(str::to_string)
}
```

- [ ] **Step 4: Branch in `materialise`.** In `claude.rs`, import `guard_hook_command`, add next to `with_guard_hook`:

```rust
/// hydraui (guards off): keep the guard hook a `hydra shell` launch put in the current
/// settings.json, so hydraui neither adds nor removes it.
fn keep_guard_hook(text: String, current: Option<&str>, warnings: &Warnings) -> String {
    let Some(command) = current.and_then(guard_hook_command) else {
        return text;
    };
    match add_guard_hook(&text, &command) {
        Ok(with_hook) => with_hook,
        Err(e) => {
            warnings.push(format!("hydra: claude: guard hook not kept ({e})"));
            text
        }
    }
}
```

and replace `let settings = with_guard_hook(merged.text, ctx.warnings);` with:

```rust
        let settings = if ctx.guards {
            with_guard_hook(merged.text, ctx.warnings)
        } else {
            let current = read_opt(&l.dir.join("settings.json"))?;
            keep_guard_hook(merged.text, current.as_deref(), ctx.warnings)
        };
```

- [ ] **Step 5: Run the provider tests.** `cargo test -p hydra-providers` — expected: PASS.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydra-providers/src/claude.rs crates/hydra-providers/src/claude_files.rs
git commit -m "feat: claude provider keeps the existing guard hook without guards

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `LaunchEnv::env_changes`

**Files:**
- Modify: `crates/hydra-core/src/resolve.rs`
- Test: unit tests in `resolve.rs`

**Interfaces:**
- Produces: `hydra_core::resolve::EnvChange { Remove(String), Set(String, OsString) }` (`Debug, Clone, PartialEq, Eq`) and `LaunchEnv::env_changes(&self, current_path: Option<OsString>) -> Vec<EnvChange>`. `apply` is built on it. Plan 2's PTY spawn uses it.

- [ ] **Step 1: Write the failing tests.**

```rust
    fn sample_launch() -> LaunchEnv {
        LaunchEnv {
            name: EnvName::parse("work").unwrap(),
            config: EnvConfig::default(),
            set: BTreeMap::from([("A".to_string(), "1".to_string())]),
            unset: BTreeSet::from(["GH_TOKEN".to_string()]),
            path_drop: vec![PathBuf::from("C:\\old")],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn env_changes_lists_removals_then_values_then_path() {
        let path = std::env::join_paths(["C:\\old", "C:\\keep"]).unwrap();
        assert_eq!(
            sample_launch().env_changes(Some(path)),
            vec![
                EnvChange::Remove("GH_TOKEN".into()),
                EnvChange::Set("A".into(), "1".into()),
                EnvChange::Set("PATH".into(), "C:\\keep".into()),
            ]
        );
    }

    #[test]
    fn apply_does_what_env_changes_says() {
        let launch = sample_launch();
        let mut cmd = Command::new("x");
        launch.apply(&mut cmd);
        let mut got: Vec<(OsString, Option<OsString>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
            .collect();
        let mut want: Vec<(OsString, Option<OsString>)> = launch
            .env_changes(std::env::var_os("PATH"))
            .into_iter()
            .map(|c| match c {
                EnvChange::Remove(k) => (k.into(), None),
                EnvChange::Set(k, v) => (k.into(), Some(v)),
            })
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydra-core env_changes` — expected: compile error (`EnvChange` not found).

- [ ] **Step 3: Implement.** Above `impl LaunchEnv`:

```rust
/// One change `apply` makes to a child's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvChange {
    Remove(String),
    Set(String, OsString),
}
```

Replace `apply` with:

```rust
    pub fn apply(&self, cmd: &mut Command) {
        for change in self.env_changes(std::env::var_os("PATH")) {
            match change {
                EnvChange::Remove(k) => {
                    cmd.env_remove(k);
                }
                EnvChange::Set(k, v) => {
                    cmd.env(k, v);
                }
            }
        }
    }

    /// What `apply` does, for callers without a `std::process::Command` (hydraui's PTY):
    /// removals, then values, then PATH built from `current_path`.
    pub fn env_changes(&self, current_path: Option<OsString>) -> Vec<EnvChange> {
        let mut out: Vec<EnvChange> = self
            .unset
            .iter()
            .map(|k| EnvChange::Remove(k.clone()))
            .collect();
        out.extend(
            self.set
                .iter()
                .map(|(k, v)| EnvChange::Set(k.clone(), v.into())),
        );
        out.push(EnvChange::Set(
            "PATH".into(),
            self.path_value(current_path),
        ));
        out
    }
```

- [ ] **Step 4: Run the tests.** `cargo test -p hydra-core` — expected: PASS.

- [ ] **Step 5: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydra-core/src/resolve.rs
git commit -m "feat: LaunchEnv::env_changes for non-Command callers

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Shell integration and startup command

**Files:**
- Modify: `crates/hydra-platform/src/shell.rs`
- Modify: `crates/hydra-cli/src/commands/launch.rs` (`integration: false`)
- Modify: `docs/superpowers/specs/2026-10-17-hydraui-design.md` §4.5 (the API name)
- Test: unit tests in `shell.rs`

**Interfaces:**
- Produces:
  - `PromptStyle { name, rgb, pub integration: bool }`
  - `render_init(kind: ShellKind, style: &PromptStyle, startup: Option<&str>) -> String`
  - `shell_launch(kind: ShellKind, init: &Path) -> (Vec<OsString>, Vec<(&'static str, &'static str)>)` — arguments and extra variables; `shell_command` is built on it.
  - Emitted sequences: OSC 7 `ESC ] 7 ; file://<host>/<path> BEL`; OSC 133 `A`, `B`, `C`, `D;<code>` each `ESC ] 133 ; X BEL`.

- [ ] **Step 1: Write the failing tests.** In `shell.rs` tests, add `integration: false` to the `TEAL` const, then add:

```rust
    const TEAL_UI: PromptStyle = PromptStyle {
        name: "work",
        rgb: None,
        integration: true,
    };

    #[test]
    fn integration_off_leaves_the_cli_scripts_alone() {
        assert_eq!(
            render_init(ShellKind::Pwsh, &TEAL, None),
            render_pwsh_init(&TEAL)
        );
        assert_eq!(
            render_init(ShellKind::Bash, &TEAL, None),
            render_bash_init(&TEAL)
        );
    }

    #[test]
    fn pwsh_startup_command_runs_last_and_is_marked_as_running() {
        let s = render_init(ShellKind::Pwsh, &TEAL_UI, Some("npm run dev"));
        assert!(s.contains("PSConsoleHostReadLine"), "{s}");
        assert!(
            s.ends_with(
                "$global:__hydraui_ran = $true; [Console]::Write(\"$([char]27)]133;C$([char]7)\")\nnpm run dev\n"
            ),
            "{s}"
        );
    }

    #[test]
    fn bash_startup_command_runs_last_and_is_marked_as_running() {
        let s = render_init(ShellKind::Bash, &TEAL_UI, Some("npm run dev"));
        assert!(s.contains("trap '__hydraui_preexec' DEBUG"), "{s}");
        assert!(
            s.ends_with("__hydraui_ran=1; printf '\\e]133;C\\a'\nnpm run dev\n"),
            "{s}"
        );
    }

    #[test]
    fn shell_launch_matches_shell_command() {
        let init = Path::new("C:\\h\\init.ps1");
        let (args, vars) = shell_launch(ShellKind::Pwsh, init);
        let cmd = shell_command(ShellKind::Pwsh, Path::new("pwsh.exe"), init);
        assert_eq!(args, cmd.get_args().map(|a| a.to_owned()).collect::<Vec<_>>());
        assert!(vars.is_empty());
        let (_, vars) = shell_launch(ShellKind::Bash, Path::new("C:\\h\\init.bash"));
        assert_eq!(vars, vec![("CHERE_INVOKING", "1")]);
    }

    /// A folder whose name needs quoting in both shells.
    fn awkward_folder() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("it's here");
        std::fs::create_dir_all(&f).unwrap();
        (d, f)
    }

    #[test]
    fn pwsh_integration_marks_prompt_exit_code_and_folder() {
        let Some(exe) = find_shell(ShellKind::Pwsh, None) else {
            eprintln!("skipped: pwsh isn't installed");
            return;
        };
        let (d, folder) = awkward_folder();
        let script = format!(
            "function global:prompt {{ 'PS> ' }}\n$function:__hydra_prompt = $function:prompt\n{}Set-Location -LiteralPath '{}'\n$global:__hydraui_ran = $true\ncmd /c exit 3\nprompt\n",
            pwsh_integration("[work] "),
            folder.to_string_lossy().replace('\'', "''")
        );
        let file = d.path().join("t.ps1");
        std::fs::write(&file, script).unwrap();
        let out = Command::new(exe)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(&file)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("\x1b]133;D;3\x07"), "{text:?}");
        assert!(text.contains("\x1b]7;file://"), "{text:?}");
        assert!(text.contains("/it's here\x07"), "{text:?}");
        assert!(
            text.contains("\x1b]133;A\x07[work] PS> \x1b]133;B\x07"),
            "{text:?}"
        );
    }

    #[test]
    fn bash_integration_marks_prompt_exit_code_and_folder() {
        let Some(exe) = find_shell(ShellKind::Bash, None) else {
            eprintln!("skipped: Git Bash isn't installed");
            return;
        };
        let (_d, folder) = awkward_folder();
        let script = format!("{BASH_INTEGRATION}__hydraui_ran=1\n(exit 4)\n__hydraui_prompt\n");
        let out = Command::new(exe)
            .args(["--norc", "--noprofile", "-c", &script])
            .current_dir(&folder)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("\x1b]133;D;4\x07"), "{text:?}");
        assert!(text.contains("\x1b]7;file://"), "{text:?}");
        assert!(text.contains("/it's here\x07"), "{text:?}");
        assert!(text.ends_with("\x1b]133;A\x07"), "{text:?}");
    }
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydra-platform shell` — expected: compile errors (`integration`, `render_init`, `shell_launch`, `pwsh_integration`, `BASH_INTEGRATION` missing).

- [ ] **Step 3: Implement.** In `shell.rs`:

Add `use std::ffi::OsString;`. Add to `PromptStyle`:

```rust
    /// hydraui: also mark the prompt (OSC 133) and the folder (OSC 7).
    pub integration: bool,
```

In `render_pwsh_init`, replace the final `function global:prompt ..` push with:

```rust
    if style.integration {
        s.push_str(&pwsh_integration(&tag));
    } else {
        s.push_str(&format!(
            "function global:prompt {{ \"{tag}\" + (& $function:__hydra_prompt) }}\n"
        ));
    }
```

In `render_bash_init`, replace the final `PS1=` push with:

```rust
    if style.integration {
        s.push_str(&format!(
            "PS1={}\"$PS1\"'\\[\\e]133;B\\a\\]'\n",
            sh_quote(&tag)
        ));
        s.push_str(BASH_INTEGRATION);
    } else {
        s.push_str(&format!("PS1={}\"$PS1\"\n", sh_quote(&tag)));
    }
```

Add:

```rust
/// hydraui's pwsh prompt: OSC 133 D (last exit code, only after a command), OSC 7 (folder),
/// A, the tagged prompt, B; and OSC 133 C when a command line is accepted. Expects
/// `$function:__hydra_prompt` to hold the user's prompt.
fn pwsh_integration(tag: &str) -> String {
    let mut s = String::from(
        r#"function global:prompt {
  $ok = $global:?
  $code = if ($ok) { 0 } elseif ($global:LASTEXITCODE) { $global:LASTEXITCODE } else { 1 }
  $e = [char]27; $b = [char]7; $out = ''
  if ($global:__hydraui_ran) { $out += "$e]133;D;$code$b" }
  $global:__hydraui_ran = $false
  $loc = $executionContext.SessionState.Path.CurrentLocation
  if ($loc.Provider.Name -eq 'FileSystem') { $out += "$e]7;file://$env:COMPUTERNAME/$($loc.ProviderPath -replace '\\', '/')$b" }
"#,
    );
    s.push_str(&format!(
        "  $out + \"$e]133;A$b\" + \"{tag}\" + (& $function:__hydra_prompt) + \"$e]133;B$b\"\n}}\n"
    ));
    s.push_str(
        r#"Import-Module PSReadLine -ErrorAction SilentlyContinue
if (Test-Path Function:\PSConsoleHostReadLine) {
  $global:__hydraui_readline = $function:PSConsoleHostReadLine
  function global:PSConsoleHostReadLine { $line = & $global:__hydraui_readline; $global:__hydraui_ran = $true; [Console]::Write("$([char]27)]133;C$([char]7)"); $line }
}
"#,
    );
    s
}

/// hydraui's bash prompt marks, the same sequence as pwsh's. The DEBUG trap fires C only for
/// the first command after a prompt: `__hydraui_armed` is set by the last PROMPT_COMMAND entry.
const BASH_INTEGRATION: &str = r#"__hydraui_prompt() {
  local ec=$?
  if [ -n "$__hydraui_ran" ]; then printf '\e]133;D;%s\a' "$ec"; fi
  __hydraui_ran=
  printf '\e]7;file://%s/%s\a' "$HOSTNAME" "$(pwd -W 2>/dev/null || pwd)"
  printf '\e]133;A\a'
  return $ec
}
__hydraui_preexec() {
  if [ -n "$__hydraui_armed" ]; then
    __hydraui_armed=
    __hydraui_ran=1
    printf '\e]133;C\a'
  fi
}
__hydraui_pc=${PROMPT_COMMAND%;}
PROMPT_COMMAND="__hydraui_prompt${__hydraui_pc:+;$__hydraui_pc};__hydraui_armed=1"
trap '__hydraui_preexec' DEBUG
"#;

/// The init script for `kind`, then `startup` (shell text, run as typed) when given. With
/// integration on, the startup command is marked as running so its exit code is reported.
pub fn render_init(kind: ShellKind, style: &PromptStyle, startup: Option<&str>) -> String {
    let mut s = match kind {
        ShellKind::Pwsh => render_pwsh_init(style),
        ShellKind::Bash => render_bash_init(style),
    };
    if let Some(cmd) = startup {
        if style.integration {
            s.push_str(match kind {
                ShellKind::Pwsh => {
                    "$global:__hydraui_ran = $true; [Console]::Write(\"$([char]27)]133;C$([char]7)\")\n"
                }
                ShellKind::Bash => "__hydraui_ran=1; printf '\\e]133;C\\a'\n",
            });
        }
        s.push_str(cmd);
        s.push('\n');
    }
    s
}

/// Arguments and extra variables that start `kind` running `init`, for callers that don't
/// use `std::process::Command` (hydraui's PTY).
pub fn shell_launch(kind: ShellKind, init: &Path) -> (Vec<OsString>, Vec<(&'static str, &'static str)>) {
    match kind {
        ShellKind::Pwsh => (
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NoExit".into(),
                "-Command".into(),
                format!(". '{}'", init.to_string_lossy().replace('\'', "''")).into(),
            ],
            vec![],
        ),
        ShellKind::Bash => (
            vec!["--rcfile".into(), to_msys(init).into(), "-i".into()],
            // Stops Git Bash's /etc/profile from cd-ing to ~.
            vec![("CHERE_INVOKING", "1")],
        ),
    }
}
```

Replace `shell_command`'s body with:

```rust
    let mut cmd = Command::new(exe);
    let (args, vars) = shell_launch(kind, init);
    cmd.args(args);
    for (k, v) in vars {
        cmd.env(k, v);
    }
    cmd
```

Make `write_init` call `render_init(kind, style, None)` instead of the two render functions.

- [ ] **Step 4: Keep the CLI on the old scripts.** In `crates/hydra-cli/src/commands/launch.rs`, add `integration: false,` to the `PromptStyle` literal.

- [ ] **Step 5: Run the tests.** `cargo test -p hydra-platform` and `cargo test --workspace` — expected: PASS (the pwsh real-shell test prints "skipped" if pwsh is missing; it must pass once pwsh is installed — run it again then).

- [ ] **Step 6: Align the spec.** In the spec, replace §4.5's first sentence "`write_init` takes `startup: Option<&str>`, appended after the prompt setup." with "`render_init(kind, style, startup)` renders an init script with an optional startup command appended after the prompt setup; `write_init` is unchanged for the CLI."

- [ ] **Step 7: fmt, clippy, commit.**

```bash
git add crates/hydra-platform/src/shell.rs crates/hydra-cli/src/commands/launch.rs docs/superpowers/specs/2026-10-17-hydraui-design.md
git commit -m "feat: shell integration marks and startup command in init scripts

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: `hydraui-sets` crate: set file model

**Files:**
- Create: `crates/hydraui-sets/Cargo.toml`, `crates/hydraui-sets/src/lib.rs`, `crates/hydraui-sets/src/file.rs`
- Test: unit tests in `file.rs`

**Interfaces:**
- Produces (used by Task 8): `hydraui_sets::file::{SetFile, TabFile, NodeFile}`, `hydraui_sets::file::parse(text: &str) -> Result<SetFile, toml::de::Error>`.

- [ ] **Step 1: Create the crate.** `crates/hydraui-sets/Cargo.toml`:

```toml
[package]
name = "hydraui-sets"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
hydra-core.workspace = true
hydra-platform.workspace = true
serde.workspace = true
thiserror.workspace = true
toml.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/hydraui-sets/src/lib.rs`:

```rust
//! hydraui's terminal sets: `<HYDRA_HOME>/hydraui/sets/<name>.toml` files resolved into panes,
//! plus which sets are loaded and the session file. No UI.
pub mod file;
```

(`file` is public: it is the format as written, and keeping it public means it isn't dead code before Task 8 uses it.)

- [ ] **Step 2: Write the failing tests.** `crates/hydraui-sets/src/file.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
env = "work"
cwd = "E:/work/client-a"

[[tab]]
title = "api"
cwd = "api"
layout = { split = "right", ratio = 0.6, panes = [
  { command = "claude" },
  { split = "down", panes = [ { command = "npm run dev" }, { } ] },
] }

[[tab]]
title = "infra"
env = "personal"
command = "terraform plan"
"#;

    #[test]
    fn reads_the_spec_example() {
        let s = parse(SAMPLE).unwrap();
        assert_eq!(s.env.as_deref(), Some("work"));
        assert_eq!(s.tabs.len(), 2);
        let layout = s.tabs[0].layout.as_ref().unwrap();
        assert_eq!(layout.split.as_deref(), Some("right"));
        assert_eq!(layout.ratio, Some(0.6));
        let panes = layout.panes.as_ref().unwrap();
        assert_eq!(panes[0].command.as_deref(), Some("claude"));
        assert_eq!(panes[1].panes.as_ref().unwrap().len(), 2);
        assert_eq!(s.tabs[1].command.as_deref(), Some("terraform plan"));
    }

    #[test]
    fn notepad_bom_and_crlf_read_the_same() {
        let crlf = format!("\u{feff}{}", SAMPLE.replace('\n', "\r\n"));
        let a = parse(SAMPLE).unwrap();
        let b = parse(&crlf).unwrap();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn unknown_keys_are_errors_with_a_line() {
        let e = parse("env = \"work\"\n\n[[tab]]\ntitel = \"x\"\n").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("titel"), "{msg}");
        assert!(msg.contains("line 4"), "{msg}");
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui-sets` — expected: compile error (`parse` not found).

- [ ] **Step 4: Implement the model.** Above the tests in `file.rs`:

```rust
//! The set file as written: every field optional, checked later by `resolve`.
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetFile {
    pub env: Option<String>,
    pub cwd: Option<String>,
    pub shell: Option<String>,
    #[serde(default, rename = "tab")]
    pub tabs: Vec<TabFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabFile {
    pub title: Option<String>,
    pub env: Option<String>,
    pub cwd: Option<String>,
    pub shell: Option<String>,
    pub command: Option<String>,
    pub layout: Option<NodeFile>,
}

/// A layout node: a split when `split` is set, otherwise a pane. One struct (not an untagged
/// enum) so a typo gets serde's "unknown field" error instead of "no variant matched".
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeFile {
    pub split: Option<String>,
    pub ratio: Option<f32>,
    pub panes: Option<Vec<NodeFile>>,
    pub env: Option<String>,
    pub cwd: Option<String>,
    pub shell: Option<String>,
    pub command: Option<String>,
}

/// Parses a set file, dropping a UTF-8 BOM (Notepad adds one).
pub fn parse(text: &str) -> Result<SetFile, toml::de::Error> {
    toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui-sets` — expected: PASS. (If `toml`'s message words the line differently, keep the assertion on the line number by matching what it prints, e.g. `"line 4"`; don't drop it.)

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui-sets Cargo.lock
git commit -m "feat: hydraui-sets crate with the set file model

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: `hydraui-sets`: resolution and validation

**Files:**
- Create: `crates/hydraui-sets/src/spec.rs`, `crates/hydraui-sets/src/resolve.rs`
- Modify: `crates/hydraui-sets/src/lib.rs`
- Test: unit tests in `resolve.rs`

**Interfaces:**
- Consumes: `file::parse`, `file::{SetFile, TabFile, NodeFile}`; `hydra_core::bindings::resolve(dir, rules) -> Option<Binding { env: String, .. }>`; `hydra_core::paths::{HydraPaths, expand_tilde}`; `hydra_core::name::EnvName`; `hydra_platform::shell::ShellKind`.
- Produces (re-exported from the crate root):
  - `SetSpec { name: String, file: PathBuf, tabs: Vec<TabSpec> }`
  - `TabSpec { title: String, root: Node }`
  - `Node::Split { dir: SplitDir, ratio: f32, first: Box<Node>, second: Box<Node> }`, `Node::Pane(PaneSpec)`, `Node::panes(&self) -> Vec<&PaneSpec>`
  - `SplitDir::{Right, Down}`
  - `PaneSpec { env: EnvName, cwd: PathBuf, shell: ShellKind, command: Option<String>, bound_elsewhere: Option<String> }`
  - `Context<'a> { paths: &'a HydraPaths, user_home: &'a Path, bindings: &'a BTreeMap<String, String>, default_shell: ShellKind }`
  - `SetError::{Name(String), Io { path, source }, Toml { path, message }, Invalid { path, problems: Vec<String> }}`
  - `sets_dir(&HydraPaths) -> PathBuf`, `set_file(&HydraPaths, &str) -> PathBuf`, `list_sets(&HydraPaths) -> std::io::Result<Vec<String>>`, `load_set(&Context, &str) -> Result<SetSpec, SetError>`, `parse_set(&Context, name: &str, file: &Path, text: &str) -> Result<SetSpec, SetError>`

- [ ] **Step 1: Write the resolved types.** `crates/hydraui-sets/src/spec.rs`:

```rust
//! A set after resolution: every pane has an environment, an existing folder and a shell.
use std::path::PathBuf;

use hydra_core::name::EnvName;
use hydra_platform::shell::ShellKind;

#[derive(Debug, Clone, PartialEq)]
pub struct SetSpec {
    pub name: String,
    pub file: PathBuf,
    pub tabs: Vec<TabSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabSpec {
    pub title: String,
    pub root: Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Side by side; `ratio` is the left pane's share.
    Right,
    /// Stacked; `ratio` is the top pane's share.
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
    Pane(PaneSpec),
}

impl Node {
    /// Panes left to right / top to bottom (depth first).
    pub fn panes(&self) -> Vec<&PaneSpec> {
        match self {
            Node::Pane(p) => vec![p],
            Node::Split { first, second, .. } => {
                let mut v = first.panes();
                v.extend(second.panes());
                v
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneSpec {
    pub env: EnvName,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// Shell text run after the shell starts.
    pub command: Option<String>,
    /// The folder's bound environment, when an explicit `env` differs from it.
    pub bound_elsewhere: Option<String>,
}
```

Update `lib.rs`:

```rust
//! hydraui's terminal sets: `<HYDRA_HOME>/hydraui/sets/<name>.toml` files resolved into panes,
//! plus which sets are loaded and the session file. No UI.
pub mod file;
mod resolve;
mod spec;

pub use resolve::{Context, SetError, list_sets, load_set, parse_set, set_file, sets_dir};
pub use spec::{Node, PaneSpec, SetSpec, SplitDir, TabSpec};
```

- [ ] **Step 2: Write the failing tests.** `crates/hydraui-sets/src/resolve.rs` (tests module only for now, plus `use` lines the implementation will need):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        _d: tempfile::TempDir,
        paths: HydraPaths,
        home: PathBuf,
        root: PathBuf,
        bindings: BTreeMap<String, String>,
    }

    fn fwd(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    impl Fx {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            let paths = HydraPaths::new(d.path().join(".hydra"));
            for env in ["work", "personal"] {
                let n = EnvName::parse(env).unwrap();
                std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
                std::fs::write(paths.env_file(&n), "").unwrap();
            }
            let home = d.path().join("home");
            let root = d.path().join("client-a");
            for dir in ["api", "web", "bound", "it's here"] {
                std::fs::create_dir_all(root.join(dir)).unwrap();
            }
            std::fs::create_dir_all(&home).unwrap();
            let bindings =
                BTreeMap::from([(format!("{}/bound/**", fwd(&root)), "personal".to_string())]);
            Self {
                _d: d,
                paths,
                home,
                root,
                bindings,
            }
        }

        fn ctx(&self) -> Context<'_> {
            Context {
                paths: &self.paths,
                user_home: &self.home,
                bindings: &self.bindings,
                default_shell: ShellKind::Pwsh,
            }
        }

        /// Parses `text` with ROOT replaced by the fixture's set folder.
        fn parse(&self, text: &str) -> Result<SetSpec, SetError> {
            let text = text.replace("ROOT", &fwd(&self.root));
            parse_set(&self.ctx(), "client-a", Path::new("client-a.toml"), &text)
        }

        /// `sub` under ROOT as the set file spells it (forward slashes), for expected messages.
        fn written(&self, sub: &str) -> PathBuf {
            Path::new(&fwd(&self.root)).join(sub)
        }

        fn problems(&self, text: &str) -> Vec<String> {
            match self.parse(text) {
                Err(SetError::Invalid { problems, .. }) => problems,
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn single_pane_tab_inherits_from_the_set() {
        let fx = Fx::new();
        let s = fx
            .parse("env = \"work\"\ncwd = \"ROOT\"\n[[tab]]\ncwd = \"api\"\n")
            .unwrap();
        assert_eq!(s.name, "client-a");
        let tab = &s.tabs[0];
        assert_eq!(tab.title, "api");
        let Node::Pane(p) = &tab.root else { panic!() };
        assert_eq!(p.env.as_str(), "work");
        assert_eq!(p.cwd, fx.root.join("api"));
        assert_eq!(p.shell, ShellKind::Pwsh);
        assert_eq!(p.command, None);
        assert_eq!(p.bound_elsewhere, None);
    }

    #[test]
    fn layout_resolves_every_pane() {
        let fx = Fx::new();
        let s = fx
            .parse(
                r#"env = "work"
cwd = "ROOT"
[[tab]]
cwd = "api"
layout = { split = "right", ratio = 0.6, panes = [
  { command = "claude" },
  { split = "down", panes = [ { command = "npm run dev", cwd = "../web" }, { shell = "bash" } ] },
] }
"#,
            )
            .unwrap();
        let tab = &s.tabs[0];
        assert_eq!(tab.title, "claude");
        let Node::Split { dir, ratio, second, .. } = &tab.root else { panic!() };
        assert_eq!((*dir, *ratio), (SplitDir::Right, 0.6));
        let Node::Split { dir, ratio, .. } = second.as_ref() else { panic!() };
        assert_eq!((*dir, *ratio), (SplitDir::Down, 0.5));
        let panes = tab.root.panes();
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].command.as_deref(), Some("claude"));
        assert_eq!(panes[1].cwd, fx.root.join("api").join("../web"));
        assert_eq!(panes[2].shell, ShellKind::Bash);
        assert_eq!(panes[2].cwd, fx.root.join("api"));
    }

    #[test]
    fn pane_env_beats_tab_env_beats_set_env() {
        let fx = Fx::new();
        let s = fx
            .parse(
                r#"env = "work"
cwd = "ROOT/api"
[[tab]]
env = "personal"
layout = { split = "right", panes = [ { }, { env = "work" } ] }
"#,
            )
            .unwrap();
        let envs: Vec<&str> = s.tabs[0].root.panes().iter().map(|p| p.env.as_str()).collect();
        assert_eq!(envs, vec!["personal", "work"]);
    }

    #[test]
    fn folder_binding_supplies_a_missing_env() {
        let fx = Fx::new();
        let s = fx.parse("[[tab]]\ncwd = \"ROOT/bound\"\n").unwrap();
        let Node::Pane(p) = &s.tabs[0].root else { panic!() };
        assert_eq!(p.env.as_str(), "personal");
        assert_eq!(p.bound_elsewhere, None);
    }

    #[test]
    fn explicit_env_on_a_folder_bound_elsewhere_loads_with_a_note() {
        let fx = Fx::new();
        let s = fx
            .parse("env = \"work\"\n[[tab]]\ncwd = \"ROOT/bound\"\n")
            .unwrap();
        let Node::Pane(p) = &s.tabs[0].root else { panic!() };
        assert_eq!(p.env.as_str(), "work");
        assert_eq!(p.bound_elsewhere.as_deref(), Some("personal"));
    }

    #[test]
    fn tilde_and_awkward_folders_resolve() {
        let fx = Fx::new();
        let s = fx
            .parse("env = \"work\"\n[[tab]]\ncwd = \"~\"\n[[tab]]\ncwd = \"ROOT/it's here\"\n")
            .unwrap();
        assert_eq!(s.tabs[0].root.panes()[0].cwd, fx.home);
        assert_eq!(s.tabs[1].root.panes()[0].cwd, fx.root.join("it's here"));
    }

    #[test]
    fn unbound_folder_without_env_is_a_problem() {
        let fx = Fx::new();
        let problems = fx.problems(
            "cwd = \"ROOT\"\n[[tab]]\ntitle = \"api\"\nlayout = { split = \"down\", panes = [ { env = \"work\" }, { cwd = \"api\" } ] }\n",
        );
        assert_eq!(
            problems,
            vec![format!(
                "pane 2 of tab \"api\" has no environment and {} isn't bound",
                fx.written("api").display()
            )]
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let fx = Fx::new();
        let problems = fx.problems(
            r#"env = "work"
cwd = "ROOT"
[[tab]]
title = "a"
env = "nobody"
[[tab]]
title = "b"
shell = "fish"
[[tab]]
title = "c"
cwd = "missing"
[[tab]]
title = "d"
command = "x"
layout = { split = "right", panes = [ { }, { } ] }
[[tab]]
title = "e"
layout = { split = "right", ratio = 0.95, panes = [ { }, { } ] }
[[tab]]
title = "f"
layout = { split = "down", panes = [ { }, { }, { } ] }
[[tab]]
title = "g"
layout = { split = "sideways", panes = [ { }, { } ] }
[[tab]]
title = "h"
layout = { split = "right", command = "x", panes = [ { }, { } ] }
"#,
        );
        assert_eq!(
            problems,
            vec![
                "tab \"a\": environment nobody doesn't exist".to_string(),
                "tab \"b\": unknown shell \"fish\"; use pwsh or bash".to_string(),
                format!("tab \"c\": folder {} doesn't exist", fx.written("missing").display()),
                "tab \"d\": command goes on a pane when the tab has a layout".to_string(),
                "a split in tab \"e\": ratio 0.95 must be between 0.1 and 0.9".to_string(),
                "a split in tab \"f\": needs exactly 2 panes, has 3".to_string(),
                "a split in tab \"g\": split must be \"right\" or \"down\", not \"sideways\"".to_string(),
                "a split in tab \"h\": env, cwd, shell and command go on its panes".to_string(),
            ]
        );
    }

    #[test]
    fn a_set_needs_a_tab() {
        let fx = Fx::new();
        assert_eq!(fx.problems("env = \"work\"\n"), vec!["the set has no [[tab]]"]);
    }

    #[test]
    fn toml_errors_name_the_file() {
        let fx = Fx::new();
        let Err(SetError::Toml { path, message }) = fx.parse("[[tab]]\ntitel = 1\n") else {
            panic!()
        };
        assert_eq!(path, PathBuf::from("client-a.toml"));
        assert!(message.contains("titel"), "{message}");
    }

    #[test]
    fn load_and_list_from_the_sets_folder() {
        let fx = Fx::new();
        let dir = sets_dir(&fx.paths);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("client-a.toml"),
            format!("env = \"work\"\n[[tab]]\ncwd = \"{}\"\n", fwd(&fx.root)),
        )
        .unwrap();
        std::fs::write(dir.join("b.toml"), "").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(list_sets(&fx.paths).unwrap(), vec!["b", "client-a"]);
        let s = load_set(&fx.ctx(), "client-a").unwrap();
        assert_eq!(s.file, set_file(&fx.paths, "client-a"));
        assert!(matches!(load_set(&fx.ctx(), "Bad Name"), Err(SetError::Name(_))));
        assert!(matches!(load_set(&fx.ctx(), "absent"), Err(SetError::Io { .. })));
    }

    #[test]
    fn no_sets_folder_means_no_sets() {
        let fx = Fx::new();
        assert!(list_sets(&fx.paths).unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui-sets` — expected: compile errors (`parse_set`, `Context`, … not found).

- [ ] **Step 4: Implement.** At the top of `resolve.rs`, above the tests:

```rust
//! Set file → `SetSpec`: inheritance (pane → tab → set → folder binding), folder and shell
//! checks. Collects every problem before failing, so one save shows them all.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hydra_core::bindings;
use hydra_core::name::EnvName;
use hydra_core::paths::{HydraPaths, expand_tilde};
use hydra_platform::shell::ShellKind;

use crate::file::{self, NodeFile, SetFile, TabFile};
use crate::spec::{Node, PaneSpec, SetSpec, SplitDir, TabSpec};

/// What resolution needs from outside the set file.
pub struct Context<'a> {
    pub paths: &'a HydraPaths,
    pub user_home: &'a Path,
    /// hydra's `[bindings]` from config.toml.
    pub bindings: &'a BTreeMap<String, String>,
    /// From hydraui's config.toml; pwsh when unset.
    pub default_shell: ShellKind,
}

#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error(
        "invalid set name {0:?}: use 1-32 lowercase letters, digits and '-', starting with a letter or digit"
    )]
    Name(String),
    #[error("can't read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Toml { path: PathBuf, message: String },
    #[error("{}:\n  {}", path.display(), problems.join("\n  "))]
    Invalid { path: PathBuf, problems: Vec<String> },
}

pub fn sets_dir(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("sets")
}

pub fn set_file(paths: &HydraPaths, name: &str) -> PathBuf {
    sets_dir(paths).join(format!("{name}.toml"))
}

/// Names of the `*.toml` files in the sets folder, sorted. No folder means no sets.
pub fn list_sets(paths: &HydraPaths) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(sets_dir(paths)) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let p = entry?.path();
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            && let Some(stem) = p.file_stem()
        {
            names.push(stem.to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Reads and resolves `sets/<name>.toml`. Set names follow environment name rules.
pub fn load_set(ctx: &Context, name: &str) -> Result<SetSpec, SetError> {
    EnvName::parse(name).map_err(|_| SetError::Name(name.to_string()))?;
    let file = set_file(ctx.paths, name);
    let text = std::fs::read_to_string(&file).map_err(|source| SetError::Io {
        path: file.clone(),
        source,
    })?;
    parse_set(ctx, name, &file, &text)
}

pub fn parse_set(ctx: &Context, name: &str, file: &Path, text: &str) -> Result<SetSpec, SetError> {
    let raw = file::parse(text).map_err(|e| SetError::Toml {
        path: file.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut r = Resolver {
        ctx,
        problems: Vec::new(),
    };
    let tabs = r.set(&raw);
    if r.problems.is_empty() {
        Ok(SetSpec {
            name: name.to_string(),
            file: file.to_path_buf(),
            tabs,
        })
    } else {
        Err(SetError::Invalid {
            path: file.to_path_buf(),
            problems: r.problems,
        })
    }
}

/// Values handed down from set to tab to pane.
#[derive(Clone)]
struct Inherited {
    env: Option<String>,
    cwd: PathBuf,
    shell: Option<String>,
}

struct Resolver<'a> {
    ctx: &'a Context<'a>,
    problems: Vec<String>,
}

impl Resolver<'_> {
    fn problem(&mut self, p: String) {
        if !self.problems.contains(&p) {
            self.problems.push(p);
        }
    }

    /// `raw` relative to `base`; `~` is the user's home.
    fn cwd(&self, raw: Option<&str>, base: &Path) -> PathBuf {
        match raw {
            None => base.to_path_buf(),
            Some(r) => {
                let p = expand_tilde(r, self.ctx.user_home);
                if p.is_absolute() { p } else { base.join(p) }
            }
        }
    }

    fn set(&mut self, s: &SetFile) -> Vec<TabSpec> {
        if s.tabs.is_empty() {
            self.problem("the set has no [[tab]]".into());
        }
        let inh = Inherited {
            env: s.env.clone(),
            cwd: self.cwd(s.cwd.as_deref(), self.ctx.user_home),
            shell: s.shell.clone(),
        };
        s.tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| self.tab(i + 1, t, &inh))
            .collect()
    }

    fn tab(&mut self, n: usize, t: &TabFile, set: &Inherited) -> Option<TabSpec> {
        let loc = match &t.title {
            Some(title) => format!("tab {title:?}"),
            None => format!("tab {n}"),
        };
        let inh = Inherited {
            env: t.env.clone().or_else(|| set.env.clone()),
            cwd: self.cwd(t.cwd.as_deref(), &set.cwd),
            shell: t.shell.clone().or_else(|| set.shell.clone()),
        };
        let root = match &t.layout {
            None => Node::Pane(self.pane(&loc, &inh, &NodeFile {
                command: t.command.clone(),
                ..Default::default()
            })?),
            Some(layout) => {
                if t.command.is_some() {
                    self.problem(format!(
                        "{loc}: command goes on a pane when the tab has a layout"
                    ));
                    return None;
                }
                let mut count = 0;
                self.node(&loc, layout, &inh, &mut count)?
            }
        };
        let title = t.title.clone().unwrap_or_else(|| default_title(&root));
        Some(TabSpec { title, root })
    }

    fn node(&mut self, tab: &str, n: &NodeFile, inh: &Inherited, count: &mut usize) -> Option<Node> {
        let Some(dir) = &n.split else {
            *count += 1;
            let loc = format!("pane {count} of {tab}");
            if n.panes.is_some() || n.ratio.is_some() {
                self.problem(format!(
                    "{loc}: panes and ratio need split = \"right\" or \"down\""
                ));
                return None;
            }
            return self.pane(&loc, inh, n).map(Node::Pane);
        };
        let loc = format!("a split in {tab}");
        let dir = match dir.as_str() {
            "right" => SplitDir::Right,
            "down" => SplitDir::Down,
            other => {
                self.problem(format!(
                    "{loc}: split must be \"right\" or \"down\", not {other:?}"
                ));
                return None;
            }
        };
        if n.env.is_some() || n.cwd.is_some() || n.shell.is_some() || n.command.is_some() {
            self.problem(format!("{loc}: env, cwd, shell and command go on its panes"));
            return None;
        }
        let ratio = n.ratio.unwrap_or(0.5);
        if !(0.1..=0.9).contains(&ratio) {
            self.problem(format!("{loc}: ratio {ratio} must be between 0.1 and 0.9"));
            return None;
        }
        let panes = n.panes.as_deref().unwrap_or(&[]);
        if panes.len() != 2 {
            self.problem(format!(
                "{loc}: needs exactly 2 panes, has {}",
                panes.len()
            ));
            return None;
        }
        // Both sides, so problems in the second are found even when the first has some.
        let first = self.node(tab, &panes[0], inh, count);
        let second = self.node(tab, &panes[1], inh, count);
        Some(Node::Split {
            dir,
            ratio,
            first: Box::new(first?),
            second: Box::new(second?),
        })
    }

    /// A leaf: `n`'s own env/cwd/shell/command over `inh`.
    fn pane(&mut self, loc: &str, inh: &Inherited, n: &NodeFile) -> Option<PaneSpec> {
        let cwd = self.cwd(n.cwd.as_deref(), &inh.cwd);
        let shell = match n.shell.as_deref().or(inh.shell.as_deref()) {
            None => self.ctx.default_shell,
            Some(s) => match ShellKind::parse(s) {
                Some(k) => k,
                None => {
                    self.problem(format!("{loc}: unknown shell {s:?}; use pwsh or bash"));
                    return None;
                }
            },
        };
        let explicit = n.env.as_deref().or(inh.env.as_deref());
        let binding = if cwd.is_dir() {
            bindings::resolve(&cwd, self.ctx.bindings)
        } else {
            None
        };
        let env_name = match (explicit, &binding) {
            (Some(e), _) => e.to_string(),
            (None, Some(b)) => b.env.clone(),
            (None, None) if !cwd.is_dir() => String::new(),
            (None, None) => {
                self.problem(format!(
                    "{loc} has no environment and {} isn't bound",
                    cwd.display()
                ));
                return None;
            }
        };
        let env = if env_name.is_empty() {
            None
        } else {
            self.env(loc, &env_name)
        };
        if !cwd.is_dir() {
            self.problem(format!("{loc}: folder {} doesn't exist", cwd.display()));
            return None;
        }
        let env = env?;
        let bound_elsewhere = binding
            .filter(|b| explicit.is_some() && b.env != env.as_str())
            .map(|b| b.env);
        Some(PaneSpec {
            env,
            cwd,
            shell,
            command: n.command.clone(),
            bound_elsewhere,
        })
    }

    fn env(&mut self, loc: &str, name: &str) -> Option<EnvName> {
        match EnvName::parse(name) {
            Err(e) => {
                self.problem(format!("{loc}: {e}"));
                None
            }
            Ok(n) if !self.ctx.paths.env_file(&n).is_file() => {
                self.problem(format!("{loc}: environment {n} doesn't exist"));
                None
            }
            Ok(n) => Some(n),
        }
    }
}

/// A tab without a title: its first pane's program, else its folder's name.
fn default_title(root: &Node) -> String {
    let p = root.panes()[0];
    if let Some(word) = p.command.as_deref().and_then(|c| c.split_whitespace().next()) {
        return word.to_string();
    }
    p.cwd
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shell".into())
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui-sets` — expected: PASS. If `every_problem_is_reported_at_once` differs only in order, fix the code (tabs are checked in file order, one problem each), not the test.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui-sets
git commit -m "feat: hydraui-sets resolves and validates set files

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: `hydraui-sets`: loaded sets and the session file

**Files:**
- Create: `crates/hydraui-sets/src/loaded.rs`, `crates/hydraui-sets/src/session.rs`
- Modify: `crates/hydraui-sets/src/lib.rs`
- Test: unit tests in both new files

**Interfaces:**
- Consumes: `set_file(&HydraPaths, &str)` (Task 8).
- Produces (re-exported):
  - `type TabId = u64`
  - `LoadedSet { name: String, tabs: Vec<TabId>, modified: Option<SystemTime> }`
  - `LoadedSets` (`Default`) with `get(&self, &str) -> Option<&LoadedSet>`, `insert(&mut self, LoadedSet)`, `remove(&mut self, &str) -> Option<LoadedSet>`, `set_of(&self, TabId) -> Option<&str>`, `tab_closed(&mut self, TabId) -> Option<String>`, `names(&self) -> Vec<String>`, `changed(&self, &HydraPaths) -> Vec<String>`
  - `file_modified(&Path) -> Option<SystemTime>`
  - `Session { sets: Vec<String> }`, `session_file(&HydraPaths) -> PathBuf`, `read_session(&HydraPaths) -> Session`, `write_session(&HydraPaths, &Session) -> std::io::Result<()>`

- [ ] **Step 1: Write the failing tests.** `crates/hydraui-sets/src/loaded.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn set(name: &str, tabs: &[TabId]) -> LoadedSet {
        LoadedSet {
            name: name.into(),
            tabs: tabs.to_vec(),
            modified: None,
        }
    }

    #[test]
    fn tracks_sets_and_their_tabs_in_load_order() {
        let mut l = LoadedSets::default();
        l.insert(set("client-a", &[1, 2]));
        l.insert(set("personal", &[3]));
        assert_eq!(l.names(), vec!["client-a", "personal"]);
        assert_eq!(l.set_of(2), Some("client-a"));
        assert_eq!(l.set_of(9), None);
        assert_eq!(l.get("personal").unwrap().tabs, vec![3]);
    }

    #[test]
    fn reloading_replaces_in_place() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1]));
        l.insert(set("b", &[2]));
        l.insert(set("a", &[5, 6]));
        assert_eq!(l.names(), vec!["a", "b"]);
        assert_eq!(l.get("a").unwrap().tabs, vec![5, 6]);
    }

    #[test]
    fn closing_the_last_tab_unloads_the_set() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1, 2]));
        assert_eq!(l.tab_closed(1), None);
        assert_eq!(l.tab_closed(2), Some("a".to_string()));
        assert!(l.get("a").is_none());
        assert_eq!(l.tab_closed(7), None);
    }

    #[test]
    fn remove_returns_the_tabs_to_close() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1, 2]));
        assert_eq!(l.remove("a").unwrap().tabs, vec![1, 2]);
        assert!(l.remove("a").is_none());
    }

    #[test]
    fn changed_finds_edited_and_deleted_set_files() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::create_dir_all(crate::sets_dir(&paths)).unwrap();
        for n in ["same", "edited"] {
            std::fs::write(set_file(&paths, n), "").unwrap();
        }
        let mut l = LoadedSets::default();
        l.insert(LoadedSet {
            modified: file_modified(&set_file(&paths, "same")),
            ..set("same", &[1])
        });
        l.insert(LoadedSet {
            modified: Some(UNIX_EPOCH),
            ..set("edited", &[2])
        });
        l.insert(LoadedSet {
            modified: Some(UNIX_EPOCH),
            ..set("gone", &[3])
        });
        assert_eq!(l.changed(&paths), vec!["edited", "gone"]);
    }
}
```

`crates/hydraui-sets/src/session.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let s = Session {
            sets: vec!["client-a".into(), "personal".into()],
        };
        write_session(&paths, &s).unwrap();
        assert_eq!(read_session(&paths), s);
    }

    #[test]
    fn missing_or_corrupt_session_means_nothing_loaded() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert_eq!(read_session(&paths), Session::default());
        std::fs::create_dir_all(session_file(&paths).parent().unwrap()).unwrap();
        for junk in ["sets = 3\n", "\u{0}\u{1}garbage", "[[sets]]\n", "extra = 1\n"] {
            std::fs::write(session_file(&paths), junk).unwrap();
            assert_eq!(read_session(&paths), Session::default(), "{junk:?}");
        }
    }
}
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydraui-sets` — expected: compile errors.

- [ ] **Step 3: Implement `loaded.rs`** (above its tests):

```rust
//! Which sets are open, and which tabs each one opened.
use std::path::Path;
use std::time::SystemTime;

use hydra_core::paths::HydraPaths;

use crate::set_file;

/// The app's id for a tab.
pub type TabId = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSet {
    pub name: String,
    pub tabs: Vec<TabId>,
    /// The set file's modification time when it was loaded.
    pub modified: Option<SystemTime>,
}

/// Loaded sets in load order (the tab strip's group order).
#[derive(Debug, Default)]
pub struct LoadedSets {
    sets: Vec<LoadedSet>,
}

pub fn file_modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl LoadedSets {
    pub fn get(&self, name: &str) -> Option<&LoadedSet> {
        self.sets.iter().find(|s| s.name == name)
    }

    /// Adds a set, or replaces it in place when reloading.
    pub fn insert(&mut self, set: LoadedSet) {
        match self.sets.iter_mut().find(|s| s.name == set.name) {
            Some(existing) => *existing = set,
            None => self.sets.push(set),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<LoadedSet> {
        let i = self.sets.iter().position(|s| s.name == name)?;
        Some(self.sets.remove(i))
    }

    pub fn set_of(&self, tab: TabId) -> Option<&str> {
        self.sets
            .iter()
            .find(|s| s.tabs.contains(&tab))
            .map(|s| s.name.as_str())
    }

    /// Forgets a tab closed by hand. Returns the set's name when it was the set's last tab;
    /// the set is then unloaded.
    pub fn tab_closed(&mut self, tab: TabId) -> Option<String> {
        let set = self.sets.iter_mut().find(|s| s.tabs.contains(&tab))?;
        set.tabs.retain(|t| *t != tab);
        if !set.tabs.is_empty() {
            return None;
        }
        let name = set.name.clone();
        self.remove(&name);
        Some(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.sets.iter().map(|s| s.name.clone()).collect()
    }

    /// Loaded sets whose file was edited or deleted since loading.
    pub fn changed(&self, paths: &HydraPaths) -> Vec<String> {
        self.sets
            .iter()
            .filter(|s| file_modified(&set_file(paths, &s.name)) != s.modified)
            .map(|s| s.name.clone())
            .collect()
    }
}
```

- [ ] **Step 4: Implement `session.rs`** (above its tests):

```rust
//! `<HYDRA_HOME>/hydraui/session.toml`: the sets loaded at last exit, reloaded at start.
use std::path::PathBuf;

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    #[serde(default)]
    pub sets: Vec<String>,
}

pub fn session_file(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("session.toml")
}

/// The saved session; empty when the file is missing or unreadable, so a bad session file
/// never stops hydraui starting.
pub fn read_session(paths: &HydraPaths) -> Session {
    std::fs::read_to_string(session_file(paths))
        .ok()
        .and_then(|t| toml::from_str(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok())
        .unwrap_or_default()
}

/// Writes through a temporary file, so a crash mid-write leaves the old session.
pub fn write_session(paths: &HydraPaths, session: &Session) -> std::io::Result<()> {
    let path = session_file(paths);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(session).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}
```

- [ ] **Step 5: Export them.** In `lib.rs` add `mod loaded;` and `mod session;`, and:

```rust
pub use loaded::{LoadedSet, LoadedSets, TabId, file_modified};
pub use session::{Session, read_session, session_file, write_session};
```

- [ ] **Step 6: Run the tests.** `cargo test -p hydraui-sets` — expected: PASS.

- [ ] **Step 7: fmt, clippy, full workspace tests, commit.**

```bash
git add crates/hydraui-sets
git commit -m "feat: hydraui-sets tracks loaded sets and the session file

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 8: Close out the plan.** Run the full gate once more (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`) and check `hydra shell` by hand in a real terminal: the prompt, `git config --global --list` (still shows `core.hookspath`), and a Claude launch's `settings.json` (still has the guard hook) are unchanged.
