# hydraui Plan 2: Terminal Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `hydraui-term`, a UI-free crate that starts a shell in a ConPTY inside a hydra environment, feeds an `alacritty_terminal` grid, reports shell-integration events (folder, prompt state, exit), and ends the whole process tree when the pane closes.

**Architecture:** `Pane::spawn(SpawnSpec)` checks the folder, finds the shell, takes hydra's shared shell lock, writes a per-pane init script (integration on), builds a `portable_pty::CommandBuilder` with the same environment `hydra shell` gives, starts it in a ConPTY and puts it in a Windows Job Object. A reader thread feeds PTY bytes to the grid and to a small OSC 7/133 scanner, and sends `TermEvent`s on a channel (one `Wakeup` until the app renders). A waiter thread reports the exit code. Dropping the pane closes the job (killing the tree), then the PTY, then releases the lock.

**Tech Stack:** Rust 2024 workspace; hydra-core, hydra-platform; `alacritty_terminal` 0.26.0, `portable-pty` 0.9.0 (both pinned by the plan 1 spike), `windows-sys` 0.61; thiserror 2; tempfile for tests.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` §6 and §9 (roadmap: `docs/superpowers/plans/2026-10-17-hydraui-roadmap.md`; spike: `docs/superpowers/notes/2026-10-17-hydraui-spike.md`).

## Global Constraints

- **Prerequisites:** PowerShell 7 (`pwsh.exe`) on PATH. Git for Windows for the bash tests; without it they print `skipped: bash isn't installed` and pass.
- **Windows only.** `hydraui-term` uses ConPTY and Job Objects, like hydraui itself (spec §2). CI runs on `windows-latest`.
- **Dependencies:** `hydraui-term` → `hydra-core`, `hydra-platform` and third-party crates only; never `hydraui-sets` or any UI crate (spec §3).
- **Pinned versions:** `alacritty_terminal = "0.26.0"`, `portable-pty = "0.9.0"` (spike note). `windows-sys = "0.61"` (already in `Cargo.lock`).
- **The CLI must not change.** This plan edits no hydra crate.
- **Per-pane init scripts** live at `state/<env>/shell/hydraui/<pane-id>.ps1|.bash` (spec §4.5); pane ids are `<pid>-<n>`.
- **ConPTY must get its replies.** The `alacritty_terminal` listener writes `Event::PtyWrite` back to the PTY, or pwsh stalls at startup (spike note).
- **Tests** never touch the real `~/.hydra`, `~/.claude`, `~/.claude.json` or global git config: use a temp `HydraPaths`. (The pwsh init script loads the developer's own `$PROFILE`, as `hydra shell` does; the bash tests point `HOME` at a temp folder.)
- **Commits:**
  - Repo identity (davojc). No `--no-verify`. No employer names.
  - Every commit message ends with:
    ```
    Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
    ```
- **Before every commit, these must pass:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace` (the task's tests plus no new failures; `keyring_store::tests::round_trip_through_credential_manager` is a known rare flake — rerun once before treating it as a failure)

## Review Focus

1. **An inherited `Path` (Windows' usual spelling) next to hydra's `PATH`:** the shell gets exactly one PATH variable, with `path_drop` applied. Pinned in Task 2.
2. **A flood of output (a 20 000-line listing):** the app gets one `Wakeup` until it next renders, not one per read. Pinned in Task 4.
3. **A pane whose folder was deleted after its set loaded:** spawning fails with an error naming the folder, instead of portable-pty silently starting the shell in the user's home. Pinned in Task 4.
4. **Closing a pane while a long command runs (`npm run dev` → `node`):** every process in the tree ends, and dropping the pane returns within a few seconds instead of hanging on ConPTY. Pinned in Task 4.
5. **A startup command with quotes and non-ASCII text** (`Write-Output "… 'héllo' wörld"`): it runs exactly as written and the text appears intact in the grid. Pinned in Task 4.

---

### Task 1: `hydraui-term` crate and the OSC scanner

**Files:**
- Create: `crates/hydraui-term/Cargo.toml`, `crates/hydraui-term/src/lib.rs`, `crates/hydraui-term/src/osc.rs`
- Test: unit tests in `osc.rs`

**Interfaces:**
- Produces (used by Task 4): `hydraui_term::osc::{OscScanner, Osc, PromptState, parse_osc7}`, re-exported `hydraui_term::PromptState`.
  - `PromptState::{AtPrompt, CommandRunning, CommandDone(Option<i32>)}` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `Osc::{Cwd(PathBuf), Prompt(PromptState)}` (`Debug, Clone, PartialEq, Eq`)
  - `OscScanner` (`Default`) with `fn feed(&mut self, bytes: &[u8]) -> Vec<Osc>`
  - `fn parse_osc7(url: &str) -> Option<PathBuf>`

- [ ] **Step 1: Create the crate.** `crates/hydraui-term/Cargo.toml`:

```toml
[package]
name = "hydraui-term"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
```

`crates/hydraui-term/src/lib.rs`:

```rust
//! hydraui's terminal core: a shell in a ConPTY running in a hydra environment, its
//! `alacritty_terminal` grid, and the shell-integration events. Windows only; no UI.
pub mod osc;

pub use osc::PromptState;
```

(`osc` is public: the scanner is useful on its own and is tested at that boundary; keeping it public also means it isn't dead code before Task 4 uses it.)

- [ ] **Step 2: Write the failing tests.** `crates/hydraui-term/src/osc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// What hydraui's pwsh prompt prints after `cmd /c exit 3` in `C:\work\it's here`.
    const PROMPT: &[u8] = b"\x1b]133;D;3\x07\x1b]7;file://PC/C:/work/it's here\x07\x1b]133;A\x07PS> \x1b]133;B\x07";

    fn expected() -> Vec<Osc> {
        vec![
            Osc::Prompt(PromptState::CommandDone(Some(3))),
            Osc::Cwd(PathBuf::from("C:\\work\\it's here")),
            Osc::Prompt(PromptState::AtPrompt),
        ]
    }

    #[test]
    fn finds_a_prompts_marks_in_order() {
        assert_eq!(OscScanner::default().feed(PROMPT), expected());
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_found() {
        for cut in 0..=PROMPT.len() {
            let mut s = OscScanner::default();
            let mut got = s.feed(&PROMPT[..cut]);
            got.extend(s.feed(&PROMPT[cut..]));
            assert_eq!(got, expected(), "cut at {cut}");
        }
    }

    #[test]
    fn one_byte_at_a_time_works_too() {
        let mut s = OscScanner::default();
        let got: Vec<Osc> = PROMPT
            .iter()
            .flat_map(|b| s.feed(std::slice::from_ref(b)))
            .collect();
        assert_eq!(got, expected());
    }

    #[test]
    fn string_terminator_ends_a_sequence_like_bel() {
        let got = OscScanner::default().feed(b"\x1b]133;C\x1b\\");
        assert_eq!(got, vec![Osc::Prompt(PromptState::CommandRunning)]);
    }

    #[test]
    fn exit_codes_and_extra_parameters() {
        let got = OscScanner::default()
            .feed(b"\x1b]133;D\x07\x1b]133;D;oops\x07\x1b]133;D;-1\x07\x1b]133;A;cl=m\x07");
        assert_eq!(
            got,
            vec![
                Osc::Prompt(PromptState::CommandDone(None)),
                Osc::Prompt(PromptState::CommandDone(None)),
                Osc::Prompt(PromptState::CommandDone(Some(-1))),
                Osc::Prompt(PromptState::AtPrompt),
            ]
        );
    }

    #[test]
    fn other_sequences_and_text_are_ignored() {
        let got = OscScanner::default().feed(
            b"plain \x1b[31mred\x1b[0m \x1b]0;title\x07 \x1b]133;B\x07 \x1b]8;;http://x\x07link\x1b]8;;\x07",
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn malformed_input_is_dropped_and_scanning_recovers() {
        let mut input = Vec::new();
        // ESC followed by something other than `\` abandons the sequence.
        input.extend_from_slice(b"\x1b]133;A\x1bX");
        // Not UTF-8.
        input.extend_from_slice(b"\x1b]7;file://h/C:/\xff\x07");
        // Too long.
        input.extend_from_slice(b"\x1b]7;");
        input.extend(std::iter::repeat_n(b'a', MAX_PAYLOAD + 10));
        input.extend_from_slice(b"\x07");
        input.extend_from_slice(b"\x1b]133;C\x07");
        assert_eq!(
            OscScanner::default().feed(&input),
            vec![Osc::Prompt(PromptState::CommandRunning)]
        );
    }

    #[test]
    fn osc7_paths() {
        assert_eq!(
            parse_osc7("file://PC/C:/Users/me"),
            Some(PathBuf::from("C:\\Users\\me"))
        );
        assert_eq!(parse_osc7("file:///D:/x y"), Some(PathBuf::from("D:\\x y")));
        assert_eq!(parse_osc7("file://PC/C:/"), Some(PathBuf::from("C:\\")));
        // pwsh in \\server\share\dir prints file://PC///server/share/dir.
        assert_eq!(
            parse_osc7("file://PC///server/share/dir"),
            Some(PathBuf::from("\\\\server\\share\\dir"))
        );
        assert_eq!(parse_osc7("file://PC/home/me"), None);
        assert_eq!(parse_osc7("http://PC/C:/x"), None);
        assert_eq!(parse_osc7("file://PC"), None);
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui-term` — expected: compile errors (`OscScanner`, `Osc`, `parse_osc7`, `MAX_PAYLOAD` not found).

- [ ] **Step 4: Implement.** Above the tests in `osc.rs`:

```rust
//! Finds the shell-integration sequences in PTY output: OSC 7 (current folder) and OSC 133
//! (prompt marks). Everything else is left to `alacritty_terminal`, which sees the same bytes.
use std::path::PathBuf;

/// Where the shell is, from OSC 133.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptState {
    /// `A`: the prompt is being drawn; the shell waits for input.
    AtPrompt,
    /// `C`: a command line was accepted and is running.
    CommandRunning,
    /// `D;<code>`: the command ended; `None` when the shell gave no numeric code.
    CommandDone(Option<i32>),
}

/// A sequence hydraui acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc {
    Cwd(PathBuf),
    Prompt(PromptState),
}

/// Longest OSC payload kept. Longer ones (a huge title, an inline image) are skipped.
const MAX_PAYLOAD: usize = 4096;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After ESC.
    Esc,
    /// Inside `ESC ]`, collecting the payload.
    Osc,
    /// ESC inside an OSC: `\` ends it (ST), `]` starts a new one, anything else abandons it.
    OscEsc,
}

/// Scans PTY output for OSC sequences, keeping its state between reads so a sequence split
/// across two reads is still found. Only 7-bit introducers and terminators are recognised:
/// the C1 bytes 0x9c/0x9d also occur inside UTF-8 text.
#[derive(Debug, Default)]
pub struct OscScanner {
    state: State,
    payload: Vec<u8>,
}

impl OscScanner {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Osc> {
        let mut found = Vec::new();
        for &b in bytes {
            self.state = match (self.state, b) {
                (State::Ground, 0x1b) => State::Esc,
                (State::Ground, _) => State::Ground,
                (State::Esc, b']') => {
                    self.payload.clear();
                    State::Osc
                }
                (State::Esc, 0x1b) => State::Esc,
                (State::Esc, _) => State::Ground,
                (State::Osc, 0x07) => {
                    found.extend(interpret(&self.payload));
                    State::Ground
                }
                (State::Osc, 0x1b) => State::OscEsc,
                (State::Osc, _) if self.payload.len() >= MAX_PAYLOAD => State::Ground,
                (State::Osc, _) => {
                    self.payload.push(b);
                    State::Osc
                }
                (State::OscEsc, b'\\') => {
                    found.extend(interpret(&self.payload));
                    State::Ground
                }
                (State::OscEsc, b']') => {
                    self.payload.clear();
                    State::Osc
                }
                (State::OscEsc, _) => State::Ground,
            };
        }
        found
    }
}

/// What a complete OSC payload means to hydraui, if anything.
fn interpret(payload: &[u8]) -> Option<Osc> {
    let text = std::str::from_utf8(payload).ok()?;
    let (code, rest) = text.split_once(';')?;
    match code {
        "7" => parse_osc7(rest).map(Osc::Cwd),
        "133" => {
            let mut parts = rest.split(';');
            let state = match parts.next()? {
                "A" => PromptState::AtPrompt,
                "C" => PromptState::CommandRunning,
                "D" => PromptState::CommandDone(parts.next().and_then(|c| c.parse().ok())),
                _ => return None,
            };
            Some(Osc::Prompt(state))
        }
        _ => None,
    }
}

/// The folder in an OSC 7 `file://<host>/<path>` URL, as a Windows path; the host is
/// ignored. hydraui's own scripts don't percent-encode, so the path is taken as written.
/// `file://h/C:/x` → `C:\x`; `file://h///server/share` → `\\server\share`; else `None`.
pub fn parse_osc7(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')? + 1..];
    let b = path.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let unc = path.len() > 2 && path.starts_with("//");
    if !drive && !unc {
        return None;
    }
    Some(PathBuf::from(path.replace('/', "\\")))
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui-term` — expected: PASS (8 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui-term Cargo.lock
git commit -m "feat: hydraui-term crate with the OSC 7/133 scanner

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Per-pane init scripts and the shell command

**Files:**
- Modify: `Cargo.toml` (workspace: add `portable-pty`)
- Modify: `crates/hydraui-term/Cargo.toml`, `crates/hydraui-term/src/lib.rs`
- Create: `crates/hydraui-term/src/init.rs`, `crates/hydraui-term/src/command.rs`
- Test: unit tests in both new files

**Interfaces:**
- Consumes: `hydra_platform::shell::{render_init, shell_launch, PromptStyle, ShellKind}`; `hydra_core::resolve::{LaunchEnv, EnvChange}` (`LaunchEnv::env_changes(current_path: Option<OsString>) -> Vec<EnvChange>`); `hydra_core::paths::HydraPaths` (`state_dir`, `state_root`).
- Produces (used by Task 4):
  - `hydraui_term::init::next_pane_id() -> String`
  - `hydraui_term::init::write_pane_init(paths: &HydraPaths, env: &EnvName, rgb: Option<(u8, u8, u8)>, kind: ShellKind, startup: Option<&str>, pane_id: &str) -> std::io::Result<PathBuf>`
  - `hydraui_term::init::clear_pane_inits(paths: &HydraPaths)` (re-exported at the crate root; the app calls it at start)
  - `hydraui_term::command::pane_command(kind: ShellKind, exe: &Path, init: &Path, cwd: &Path, launch: &LaunchEnv, inherited: &[(OsString, OsString)]) -> portable_pty::CommandBuilder`

- [ ] **Step 1: Add the dependencies.** In the root `Cargo.toml`, add to `[workspace.dependencies]` (after `ureq`):

```toml
portable-pty = "0.9.0"
```

Replace `crates/hydraui-term/Cargo.toml` with:

```toml
[package]
name = "hydraui-term"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
hydra-core.workspace = true
hydra-platform.workspace = true
portable-pty.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

Replace `crates/hydraui-term/src/lib.rs` with:

```rust
//! hydraui's terminal core: a shell in a ConPTY running in a hydra environment, its
//! `alacritty_terminal` grid, and the shell-integration events. Windows only; no UI.
pub mod command;
pub mod init;
pub mod osc;

pub use init::clear_pane_inits;
pub use osc::PromptState;
```

- [ ] **Step 2: Write the failing tests.** `crates/hydraui-term/src/init.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> EnvName {
        EnvName::parse("work").unwrap()
    }

    #[test]
    fn pane_ids_differ_and_name_the_process() {
        assert_ne!(next_pane_id(), next_pane_id());
        assert!(next_pane_id().starts_with(&format!("{}-", std::process::id())));
    }

    #[test]
    fn each_pane_gets_its_own_script_beside_the_clis() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let a = write_pane_init(
            &paths,
            &work(),
            None,
            ShellKind::Pwsh,
            Some("npm run dev"),
            "1-1",
        )
        .unwrap();
        let b = write_pane_init(&paths, &work(), None, ShellKind::Bash, None, "1-2").unwrap();
        let dir = paths.state_dir(&work()).join("shell");
        assert_eq!(a, dir.join("hydraui").join("1-1.ps1"));
        assert_eq!(b, dir.join("hydraui").join("1-2.bash"));
        let text = std::fs::read_to_string(&a).unwrap();
        assert!(text.contains("133;A"), "{text}");
        assert!(text.ends_with("npm run dev\n"), "{text}");
        assert!(!dir.join("init.ps1").exists());
    }

    #[test]
    fn clearing_removes_every_environments_pane_scripts_only() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let personal = EnvName::parse("personal").unwrap();
        let a = write_pane_init(&paths, &work(), None, ShellKind::Pwsh, None, "1-1").unwrap();
        let b = write_pane_init(&paths, &personal, None, ShellKind::Pwsh, None, "1-2").unwrap();
        let cli = paths.state_dir(&work()).join("shell").join("init.ps1");
        std::fs::write(&cli, "cli").unwrap();
        clear_pane_inits(&paths);
        assert!(!a.exists() && !b.exists());
        assert!(cli.exists());
        // No state folder yet: nothing to do, no panic.
        clear_pane_inits(&HydraPaths::new(d.path().join("missing")));
    }
}
```

`crates/hydraui-term/src/command.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use hydra_core::config::EnvConfig;
    use hydra_core::name::EnvName;

    use super::*;

    fn launch(set: &[(&str, &str)]) -> LaunchEnv {
        LaunchEnv {
            name: EnvName::parse("work").unwrap(),
            config: EnvConfig::default(),
            set: set
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            unset: BTreeSet::from(["GH_TOKEN".to_string()]),
            path_drop: vec![PathBuf::from("C:\\old")],
            warnings: Vec::new(),
        }
    }

    /// Windows spells it `Path`; hydra writes `PATH`.
    fn inherited() -> Vec<(OsString, OsString)> {
        vec![
            (
                "Path".into(),
                std::env::join_paths(["C:\\old", "C:\\keep"]).unwrap(),
            ),
            ("GH_TOKEN".into(), "leaked".into()),
            ("SystemRoot".into(), "C:\\Windows".into()),
        ]
    }

    fn pwsh(launch: &LaunchEnv) -> CommandBuilder {
        pane_command(
            ShellKind::Pwsh,
            Path::new("C:\\pwsh\\pwsh.exe"),
            Path::new("C:\\s\\1-1.ps1"),
            Path::new("C:\\work"),
            launch,
            &inherited(),
        )
    }

    #[test]
    fn environment_is_hydras_on_top_of_the_inherited_one() {
        let cmd = pwsh(&launch(&[("FOO", "bar")]));
        assert_eq!(cmd.get_env("FOO"), Some(OsStr::new("bar")));
        assert_eq!(cmd.get_env("GH_TOKEN"), None);
        assert_eq!(cmd.get_env("SystemRoot"), Some(OsStr::new("C:\\Windows")));
        assert_eq!(cmd.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(cmd.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(
            cmd.get_cwd().map(PathBuf::from),
            Some(PathBuf::from("C:\\work"))
        );
    }

    #[test]
    fn inherited_path_in_any_case_becomes_one_path_without_the_dropped_folder() {
        let cmd = pwsh(&launch(&[]));
        let paths: Vec<(&str, &str)> = cmd
            .iter_full_env_as_str()
            .filter(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert_eq!(paths[0].1, "C:\\keep");
    }

    #[test]
    fn the_environments_own_term_wins() {
        let cmd = pwsh(&launch(&[("TERM", "dumb")]));
        assert_eq!(cmd.get_env("TERM"), Some(OsStr::new("dumb")));
    }

    #[test]
    fn arguments_match_hydra_shell() {
        let init = Path::new("C:\\s\\1-2.bash");
        let cmd = pane_command(
            ShellKind::Bash,
            Path::new("C:\\Git\\bin\\bash.exe"),
            init,
            Path::new("C:\\work"),
            &launch(&[]),
            &inherited(),
        );
        let mut want = vec![OsString::from("C:\\Git\\bin\\bash.exe")];
        want.extend(shell_launch(ShellKind::Bash, init).0);
        assert_eq!(cmd.get_argv(), &want);
        assert_eq!(cmd.get_env("CHERE_INVOKING"), Some(OsStr::new("1")));
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui-term` — expected: compile errors (`next_pane_id`, `write_pane_init`, `clear_pane_inits`, `pane_command` not found).

- [ ] **Step 4: Implement `init.rs`** (above its tests):

```rust
//! Per-pane init scripts: `state/<env>/shell/hydraui/<pane-id>.ps1|.bash`, so panes never
//! overwrite each other or the CLI's `init.ps1` (spec §4.5).
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_platform::shell::{PromptStyle, ShellKind, render_init};

/// A pane id unique across hydraui processes: `<pid>-<n>`.
pub fn next_pane_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Writes the pane's init script (shell integration on, `startup` last) and returns its path.
pub fn write_pane_init(
    paths: &HydraPaths,
    env: &EnvName,
    rgb: Option<(u8, u8, u8)>,
    kind: ShellKind,
    startup: Option<&str>,
    pane_id: &str,
) -> std::io::Result<PathBuf> {
    let dir = paths.state_dir(env).join("shell").join("hydraui");
    std::fs::create_dir_all(&dir)?;
    let ext = match kind {
        ShellKind::Pwsh => "ps1",
        ShellKind::Bash => "bash",
    };
    let path = dir.join(format!("{pane_id}.{ext}"));
    let style = PromptStyle {
        name: env.as_str(),
        rgb,
        integration: true,
    };
    std::fs::write(&path, render_init(kind, &style, startup))?;
    Ok(path)
}

/// Deletes every environment's pane init scripts; hydraui calls it at start. A script is
/// only read when its shell starts, so deleting one a running pane used is harmless.
pub fn clear_pane_inits(paths: &HydraPaths) {
    let Ok(envs) = std::fs::read_dir(paths.state_root()) else {
        return;
    };
    for env in envs.flatten() {
        // Best effort: a folder that can't be removed now is tried again next start.
        let _ = std::fs::remove_dir_all(env.path().join("shell").join("hydraui"));
    }
}
```

- [ ] **Step 5: Implement `command.rs`** (above its tests):

```rust
//! The shell's command line and environment, built as `hydra shell` builds them, for
//! portable-pty.
use std::ffi::OsString;
use std::path::Path;

use hydra_core::resolve::{EnvChange, LaunchEnv};
use hydra_platform::shell::{ShellKind, shell_launch};
use portable_pty::CommandBuilder;

/// What hydraui's grid emulates. Set before the environment's own `[env]`, which can
/// override them.
const TERMINAL_VARS: [(&str, &str); 2] = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// `exe` running `init` in `cwd`, starting from `inherited` (normally `std::env::vars_os()`)
/// changed exactly as `LaunchEnv::apply` changes a `hydra shell` child. portable-pty
/// compares names case-insensitively, so an inherited `Path` and hydra's `PATH` are one
/// variable.
pub fn pane_command(
    kind: ShellKind,
    exe: &Path,
    init: &Path,
    cwd: &Path,
    launch: &LaunchEnv,
    inherited: &[(OsString, OsString)],
) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(exe);
    let (args, vars) = shell_launch(kind, init);
    cmd.args(args);
    cmd.env_clear();
    let mut path = None;
    for (k, v) in inherited {
        if k.eq_ignore_ascii_case("PATH") {
            path = Some(v.clone());
        }
        cmd.env(k, v);
    }
    for (k, v) in TERMINAL_VARS.into_iter().chain(vars) {
        cmd.env(k, v);
    }
    for change in launch.env_changes(path) {
        match change {
            EnvChange::Remove(k) => cmd.env_remove(k),
            EnvChange::Set(k, v) => cmd.env(k, v),
        }
    }
    cmd.cwd(cwd);
    cmd
}
```

- [ ] **Step 6: Run the tests.** `cargo test -p hydraui-term` — expected: PASS (15 tests).

- [ ] **Step 7: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui-term
git commit -m "feat: hydraui-term per-pane init scripts and shell command

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Job Object

**Files:**
- Modify: `Cargo.toml` (workspace: add `windows-sys`)
- Modify: `crates/hydraui-term/Cargo.toml`, `crates/hydraui-term/src/lib.rs`
- Create: `crates/hydraui-term/src/job.rs`
- Test: unit test in `job.rs`

**Interfaces:**
- Produces (used by Task 4): `hydraui_term::job::Job` (`Debug`, `Send + Sync`) with `fn new() -> std::io::Result<Job>` and `fn assign(&self, process: std::os::windows::io::RawHandle) -> std::io::Result<()>`. Dropping it kills every process in the job.

- [ ] **Step 1: Add the dependency.** In the root `Cargo.toml` `[workspace.dependencies]`, after `portable-pty`:

```toml
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_Security", "Win32_System_JobObjects", "Win32_System_Threading"] }
```

In `crates/hydraui-term/Cargo.toml`, add to `[dependencies]` (after `portable-pty`):

```toml
windows-sys.workspace = true
```

In `crates/hydraui-term/src/lib.rs`, add `pub mod job;` between `pub mod init;` and `pub mod osc;`.

- [ ] **Step 2: Write the failing test.** `crates/hydraui-term/src/job.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn closing_the_job_ends_its_processes() {
        let mut child = Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let job = Job::new().unwrap();
        job.assign(child.as_raw_handle()).unwrap();
        assert!(child.try_wait().unwrap().is_none(), "ping ended early");
        drop(job);
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "ping outlived its job"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
```

- [ ] **Step 3: Run it to see it fail.** `cargo test -p hydraui-term job` — expected: compile error (`Job` not found).

- [ ] **Step 4: Implement.** Above the test in `job.rs`:

```rust
//! A Windows Job Object that ends a pane's whole process tree when it closes (spec §6.1).
use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

/// Dropping it kills every process in it. Processes a member starts join it too.
#[derive(Debug)]
pub struct Job(HANDLE);

// SAFETY: a job handle is a kernel handle, usable from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> std::io::Result<Self> {
        // SAFETY: no name and default security; the returned Job owns the handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = Job(handle);
        let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: `info` is the struct this information class expects, with its size.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts a running process, and everything it starts from now on, in the job.
    pub fn assign(&self, process: RawHandle) -> std::io::Result<()> {
        // SAFETY: `process` is a live process handle the caller owns.
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}
```

- [ ] **Step 5: Run the test.** `cargo test -p hydraui-term job` — expected: PASS.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui-term
git commit -m "feat: hydraui-term job object that ends a process tree

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `Pane`: spawn, events, input, resize and close (pwsh)

**Files:**
- Modify: `Cargo.toml` (workspace: add `alacritty_terminal`)
- Modify: `crates/hydraui-term/Cargo.toml`, `crates/hydraui-term/src/lib.rs`
- Create: `crates/hydraui-term/src/event.rs`, `crates/hydraui-term/src/pane.rs`
- Create: `crates/hydraui-term/tests/common/mod.rs`, `crates/hydraui-term/tests/pwsh.rs`
- Modify: `docs/superpowers/specs/2026-10-17-hydraui-design.md` §6.1, §6.2
- Test: unit tests in `event.rs` and `pane.rs`; integration tests in `tests/pwsh.rs`

**Interfaces:**
- Consumes: `osc::{OscScanner, Osc, PromptState}` (Task 1); `init::{next_pane_id, write_pane_init}`, `command::pane_command` (Task 2); `job::Job` (Task 3); `hydra_core::lock::{hold_shared, ShellLock}`; `hydra_platform::shell::find_shell`.
- Produces (plan 3 builds the app on these; all re-exported at the crate root):
  - `TermEvent::{Wakeup, CwdChanged(PathBuf), Prompt(PromptState), Title(String), Bell, Exited(Option<i32>)}` (`Debug, Clone, PartialEq, Eq`)
  - `Listener` (the `alacritty_terminal::event::EventListener` in `Term<Listener>`)
  - `GridSize { cols: u16, rows: u16 }` (`Debug, Clone, Copy, PartialEq, Eq`; implements `alacritty_terminal::grid::Dimensions`)
  - `SpawnSpec { paths: HydraPaths, launch: LaunchEnv, cwd: PathBuf, shell: ShellKind, git_bash: Option<PathBuf>, command: Option<String>, size: GridSize, inherited: Vec<(OsString, OsString)> }`
  - `SpawnError::{NoFolder(PathBuf), NoPwsh, NoBash, Io { what: &'static str, source: std::io::Error }, Pty(String)}`
  - `Pane::spawn(SpawnSpec) -> Result<(Pane, std::sync::mpsc::Receiver<TermEvent>), SpawnError>`
  - `Pane::id(&self) -> &str`, `Pane::write(&self, &[u8]) -> std::io::Result<()>`, `Pane::resize(&self, GridSize) -> std::io::Result<()>`, `Pane::with_term<R>(&self, impl FnOnce(&mut Term<Listener>) -> R) -> R`, `Pane::screen_text(&self) -> String`; `Drop` closes the pane
  - `pub use alacritty_terminal;` so the app renders with the same version

- [ ] **Step 1: Add the dependencies.** In the root `Cargo.toml` `[workspace.dependencies]`, after `windows-sys`:

```toml
alacritty_terminal = "0.26.0"
```

Replace `crates/hydraui-term/Cargo.toml` with:

```toml
[package]
name = "hydraui-term"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
alacritty_terminal.workspace = true
hydra-core.workspace = true
hydra-platform.workspace = true
portable-pty.workspace = true
thiserror.workspace = true
windows-sys.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

Replace `crates/hydraui-term/src/lib.rs` with:

```rust
//! hydraui's terminal core: a shell in a ConPTY running in a hydra environment, its
//! `alacritty_terminal` grid, and the shell-integration events. Windows only; no UI.
pub mod command;
mod event;
pub mod init;
pub mod job;
pub mod osc;
mod pane;

/// The emulator, re-exported so the app renders with the same version.
pub use alacritty_terminal;
pub use event::{Listener, TermEvent};
pub use init::clear_pane_inits;
pub use osc::PromptState;
pub use pane::{GridSize, Pane, SpawnError, SpawnSpec};
```

- [ ] **Step 2: Write the failing unit tests.** `crates/hydraui-term/src/event.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A writer the test can read back.
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn replies_go_to_the_pty_and_title_and_bell_to_the_app() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let l = Listener {
            writer: Arc::new(Mutex::new(Box::new(Shared(written.clone())))),
            events: tx,
        };
        l.send_event(Event::PtyWrite("\x1b[1;1R".into()));
        l.send_event(Event::Title("vim".into()));
        l.send_event(Event::ResetTitle);
        l.send_event(Event::Bell);
        l.send_event(Event::MouseCursorDirty);
        assert_eq!(&*written.lock().unwrap(), b"\x1b[1;1R");
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                TermEvent::Title("vim".into()),
                TermEvent::Title(String::new()),
                TermEvent::Bell,
            ]
        );
    }
}
```

`crates/hydraui-term/src/pane.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use hydra_core::config::EnvConfig;
    use hydra_core::name::EnvName;

    use super::*;

    #[test]
    fn a_missing_folder_is_an_error_not_a_start_in_home() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("gone");
        let spec = SpawnSpec {
            paths: HydraPaths::new(d.path().join(".hydra")),
            launch: LaunchEnv {
                name: EnvName::parse("work").unwrap(),
                config: EnvConfig::default(),
                set: BTreeMap::new(),
                unset: BTreeSet::new(),
                path_drop: Vec::new(),
                warnings: Vec::new(),
            },
            cwd: missing.clone(),
            shell: ShellKind::Pwsh,
            git_bash: None,
            command: None,
            size: GridSize { cols: 80, rows: 24 },
            inherited: Vec::new(),
        };
        match Pane::spawn(spec) {
            Err(SpawnError::NoFolder(p)) => assert_eq!(p, missing),
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("spawned in a missing folder"),
        }
        assert!(
            !d.path().join(".hydra").exists(),
            "nothing is written before the folder check"
        );
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui-term --lib` — expected: compile errors (`Listener`, `TermEvent`, `Pane`, `SpawnSpec`, … not found).

- [ ] **Step 4: Implement `event.rs`** (above its tests):

```rust
//! What a pane reports to the app, and the `alacritty_terminal` listener that produces part
//! of it.
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};

use crate::osc::PromptState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    /// New output is in the grid. Sent once until the app next calls `Pane::with_term`.
    Wakeup,
    /// OSC 7: the shell's current folder.
    CwdChanged(PathBuf),
    /// OSC 133.
    Prompt(PromptState),
    /// The window title a program set; empty when it reset it.
    Title(String),
    Bell,
    /// The shell process ended, with its exit code when known.
    Exited(Option<i32>),
}

pub(crate) type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// Receives `Term`'s events: writes the terminal's replies back to the PTY (ConPTY waits for
/// the cursor-position reply at startup) and forwards title and bell to the app.
#[derive(Clone)]
pub struct Listener {
    pub(crate) writer: Writer,
    pub(crate) events: Sender<TermEvent>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let forward = match event {
            Event::PtyWrite(s) => {
                if let Ok(mut w) = self.writer.lock() {
                    let _ = w.write_all(s.as_bytes());
                    let _ = w.flush();
                }
                return;
            }
            Event::Title(t) => TermEvent::Title(t),
            Event::ResetTitle => TermEvent::Title(String::new()),
            Event::Bell => TermEvent::Bell,
            _ => return,
        };
        let _ = self.events.send(forward);
    }
}
```

- [ ] **Step 5: Implement `pane.rs`** (above its tests):

```rust
//! A shell in a ConPTY, in a hydra environment, feeding an `alacritty_terminal` grid
//! (spec §6).
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi::Processor;
use hydra_core::lock::{ShellLock, hold_shared};
use hydra_core::paths::HydraPaths;
use hydra_core::resolve::LaunchEnv;
use hydra_platform::shell::{ShellKind, find_shell};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::command::pane_command;
use crate::event::{Listener, TermEvent, Writer};
use crate::init::{next_pane_id, write_pane_init};
use crate::job::Job;
use crate::osc::{Osc, OscScanner};

/// A pane's size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl GridSize {
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Everything needed to start a pane. The app runs `prepare` (with `no_guards: true`) first.
pub struct SpawnSpec {
    pub paths: HydraPaths,
    pub launch: LaunchEnv,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// hydra's `git_bash` setting from config.toml, if any.
    pub git_bash: Option<PathBuf>,
    /// Shell text run after the shell starts.
    pub command: Option<String>,
    pub size: GridSize,
    /// The environment the shell starts from before hydra's changes; normally
    /// `std::env::vars_os().collect()`.
    pub inherited: Vec<(OsString, OsString)>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("folder {} doesn't exist", .0.display())]
    NoFolder(PathBuf),
    #[error("pwsh.exe isn't on PATH; install PowerShell 7")]
    NoPwsh,
    #[error("Git Bash wasn't found; set git_bash in config.toml")]
    NoBash,
    #[error("can't {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("can't start the terminal: {0}")]
    Pty(String),
}

fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> SpawnError {
    move |source| SpawnError::Io { what, source }
}

fn pty_err(e: impl std::fmt::Display) -> SpawnError {
    SpawnError::Pty(e.to_string())
}

/// A running shell. Dropping it ends the shell's whole process tree, closes the PTY and
/// releases the environment's shell lock, in that order (spec §6.4).
pub struct Pane {
    id: String,
    term: Arc<FairMutex<Term<Listener>>>,
    writer: Writer,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    wake: Arc<AtomicBool>,
    init: PathBuf,
    job: Option<Job>,
    /// Last field: released after everything else is gone.
    _lock: ShellLock,
}

impl Pane {
    /// Starts the shell (spec §6.1) and returns the pane with its events.
    pub fn spawn(spec: SpawnSpec) -> Result<(Pane, Receiver<TermEvent>), SpawnError> {
        // portable-pty would quietly start in the user's home instead.
        if !spec.cwd.is_dir() {
            return Err(SpawnError::NoFolder(spec.cwd));
        }
        let exe = find_shell(spec.shell, spec.git_bash.as_deref()).ok_or(match spec.shell {
            ShellKind::Pwsh => SpawnError::NoPwsh,
            ShellKind::Bash => SpawnError::NoBash,
        })?;
        let env = &spec.launch.name;
        let lock = hold_shared(&spec.paths, env).map_err(io("lock the environment"))?;
        let id = next_pane_id();
        let rgb = spec.launch.config.rgb().map(|c| (c.0, c.1, c.2));
        let init = write_pane_init(
            &spec.paths,
            env,
            rgb,
            spec.shell,
            spec.command.as_deref(),
            &id,
        )
        .map_err(io("write the init script"))?;
        let cmd = pane_command(
            spec.shell,
            &exe,
            &init,
            &spec.cwd,
            &spec.launch,
            &spec.inherited,
        );
        let (started, events) = match start(cmd, spec.size) {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(&init);
                return Err(e);
            }
        };
        let pane = Pane {
            id,
            term: started.term,
            writer: started.writer,
            master: Mutex::new(started.master),
            killer: started.killer,
            wake: started.wake,
            init,
            job: Some(started.job),
            _lock: lock,
        };
        Ok((pane, events))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Sends input (keys, pasted text) to the shell.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("the pane's writer is poisoned"))?;
        w.write_all(bytes)?;
        w.flush()
    }

    /// Resizes the PTY and the grid together.
    pub fn resize(&self, size: GridSize) -> std::io::Result<()> {
        self.master
            .lock()
            .map_err(|_| std::io::Error::other("the pane's PTY is poisoned"))?
            .resize(size.pty())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.term.lock().resize(size);
        Ok(())
    }

    /// Runs `f` on the grid, for rendering. Clears the pending `Wakeup` first, so output
    /// that arrives during or after `f` sends a new one.
    pub fn with_term<R>(&self, f: impl FnOnce(&mut Term<Listener>) -> R) -> R {
        self.wake.store(false, Ordering::Release);
        f(&mut self.term.lock())
    }

    /// The visible screen as text, one line per row with trailing spaces trimmed. For tests
    /// and diagnostics; it doesn't count as rendering.
    pub fn screen_text(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        (0..grid.screen_lines())
            .map(|l| {
                let row = &grid[Line(l as i32)];
                let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
                text.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // The job first: it ends the whole tree (npm → node), not just the shell.
        drop(self.job.take());
        let _ = self.killer.kill();
        let _ = std::fs::remove_file(&self.init);
        // Then the fields: the PTY closes (the reader thread sees EOF), the lock goes last.
    }
}

/// What `start` hands back to `spawn`.
struct Started {
    term: Arc<FairMutex<Term<Listener>>>,
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    wake: Arc<AtomicBool>,
    job: Job,
}

/// Opens the ConPTY, starts `cmd` in it inside a new job, and starts the reader and waiter
/// threads.
fn start(
    cmd: CommandBuilder,
    size: GridSize,
) -> Result<(Started, Receiver<TermEvent>), SpawnError> {
    let pair = native_pty_system().openpty(size.pty()).map_err(pty_err)?;
    let mut child = pair.slave.spawn_command(cmd).map_err(pty_err)?;
    drop(pair.slave);
    let killer = child.clone_killer();
    let job = Job::new().and_then(|job| {
        let process = child
            .as_raw_handle()
            .ok_or_else(|| std::io::Error::other("the shell has no process handle"))?;
        job.assign(process)?;
        Ok(job)
    });
    let job = match job {
        Ok(job) => job,
        Err(source) => {
            let _ = child.kill();
            return Err(SpawnError::Io {
                what: "put the shell in a job object",
                source,
            });
        }
    };
    // From here on an early return drops `job`, which ends the shell.
    let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(pty_err)?));
    let reader = pair.master.try_clone_reader().map_err(pty_err)?;
    let (tx, rx) = channel();
    let listener = Listener {
        writer: writer.clone(),
        events: tx.clone(),
    };
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &size,
        listener,
    )));
    let wake = Arc::new(AtomicBool::new(false));
    {
        let (term, wake, tx) = (term.clone(), wake.clone(), tx.clone());
        std::thread::Builder::new()
            .name("hydraui-pty-read".into())
            .spawn(move || read_loop(reader, &term, &wake, &tx))
            .map_err(io("start the reader thread"))?;
    }
    std::thread::Builder::new()
        .name("hydraui-pty-wait".into())
        .spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            let _ = tx.send(TermEvent::Exited(code));
        })
        .map_err(io("start the waiter thread"))?;
    Ok((
        Started {
            term,
            writer,
            master: pair.master,
            killer,
            wake,
            job,
        },
        rx,
    ))
}

/// Feeds PTY output to the grid and the OSC scanner until the PTY closes.
fn read_loop(
    mut reader: Box<dyn Read + Send>,
    term: &FairMutex<Term<Listener>>,
    wake: &AtomicBool,
    tx: &Sender<TermEvent>,
) {
    let mut parser: Processor = Processor::new();
    let mut osc = OscScanner::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let found = osc.feed(&buf[..n]);
        parser.advance(&mut *term.lock(), &buf[..n]);
        for o in found {
            let _ = tx.send(match o {
                Osc::Cwd(p) => TermEvent::CwdChanged(p),
                Osc::Prompt(s) => TermEvent::Prompt(s),
            });
        }
        // One Wakeup until the app renders (`with_term` clears the flag), so heavy output
        // can't flood the channel.
        if !wake.swap(true, Ordering::AcqRel) {
            let _ = tx.send(TermEvent::Wakeup);
        }
    }
}
```

- [ ] **Step 6: Run the unit tests.** `cargo test -p hydraui-term --lib` — expected: PASS (18 tests).

- [ ] **Step 7: Write the real-shell tests.** `crates/hydraui-term/tests/common/mod.rs`:

```rust
//! Shared helpers for the real-shell tests: a temp HYDRA_HOME, a spawned pane, and waits.
#![allow(dead_code)] // each test file uses a different subset

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use hydra_core::config::EnvConfig;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::resolve::LaunchEnv;
use hydra_platform::shell::{ShellKind, find_shell};
use hydraui_term::{GridSize, Pane, PromptState, SpawnSpec, TermEvent};

pub const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Fx {
    pub dir: tempfile::TempDir,
    pub paths: HydraPaths,
    /// The pane's start folder. It holds `it's here`, a name both shells must quote.
    pub start: PathBuf,
}

impl Fx {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path().join(".hydra"));
        let start = dir.path().join("start here");
        std::fs::create_dir_all(start.join("it's here")).unwrap();
        Self { dir, paths, start }
    }

    pub fn env(&self) -> EnvName {
        EnvName::parse("work").unwrap()
    }

    /// A pane in `work` with FOO=bar set and GH_TOKEN removed, starting from the test's own
    /// environment plus an inherited GH_TOKEN. `None` (after a message) when the shell
    /// isn't installed.
    pub fn spawn(
        &self,
        shell: ShellKind,
        command: Option<&str>,
    ) -> Option<(Pane, Receiver<TermEvent>)> {
        if find_shell(shell, None).is_none() {
            eprintln!("skipped: {} isn't installed", shell.as_str());
            return None;
        }
        let mut inherited: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("GH_TOKEN") && !k.eq_ignore_ascii_case("HOME"))
            .collect();
        inherited.push(("GH_TOKEN".into(), "leaked".into()));
        if shell == ShellKind::Bash {
            // Keeps the developer's ~/.bashrc out of the test.
            inherited.push(("HOME".into(), self.dir.path().into()));
        }
        let spec = SpawnSpec {
            paths: self.paths.clone(),
            launch: LaunchEnv {
                name: self.env(),
                config: EnvConfig::default(),
                set: BTreeMap::from([("FOO".to_string(), "bar".to_string())]),
                unset: BTreeSet::from(["GH_TOKEN".to_string()]),
                path_drop: Vec::new(),
                warnings: Vec::new(),
            },
            cwd: self.start.clone(),
            shell,
            git_bash: None,
            command: command.map(str::to_string),
            size: GridSize {
                cols: 120,
                rows: 30,
            },
            inherited,
        };
        Some(Pane::spawn(spec).unwrap())
    }

    /// How many pane init scripts exist for `work`.
    pub fn scripts(&self) -> usize {
        std::fs::read_dir(self.paths.state_dir(&self.env()).join("shell").join("hydraui"))
            .map(|d| d.count())
            .unwrap_or(0)
    }
}

pub fn at_prompt(e: &TermEvent) -> bool {
    *e == TermEvent::Prompt(PromptState::AtPrompt)
}

/// Receives events until `pred` matches one; returns everything received, that one last.
pub fn wait_for(
    rx: &Receiver<TermEvent>,
    what: &str,
    mut pred: impl FnMut(&TermEvent) -> bool,
) -> Vec<TermEvent> {
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(e) => {
                let done = pred(&e);
                seen.push(e);
                if done {
                    return seen;
                }
            }
            Err(_) => panic!("timed out waiting for {what}; got {seen:?}"),
        }
    }
}

/// Just the prompt states in `events`, in order.
pub fn prompts(events: &[TermEvent]) -> Vec<PromptState> {
    events
        .iter()
        .filter_map(|e| match e {
            TermEvent::Prompt(p) => Some(*p),
            _ => None,
        })
        .collect()
}

/// Polls the screen until it contains `needle`.
pub fn wait_text(pane: &Pane, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !pane.screen_text().contains(needle) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; screen:\n{}",
            pane.screen_text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The same folder, however each side spells it (short names, case, `\\?\`).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `events` include a `CwdChanged` to `dir`.
pub fn reached(events: &[TermEvent], dir: &Path) -> bool {
    events
        .iter()
        .any(|e| matches!(e, TermEvent::CwdChanged(p) if same_dir(p, dir)))
}
```

`crates/hydraui-term/tests/pwsh.rs`:

```rust
//! pwsh in a real ConPTY. Each test prints "skipped" and passes when pwsh isn't installed.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Fx, at_prompt, prompts, reached, wait_for, wait_text};
use hydra_core::lock::is_running;
use hydra_platform::shell::ShellKind;
use hydraui_term::alacritty_terminal::grid::Dimensions;
use hydraui_term::{GridSize, PromptState, TermEvent};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

const PWSH: ShellKind = ShellKind::Pwsh;

#[test]
fn startup_command_sees_the_environment_and_reports_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(
        PWSH,
        Some("Write-Output \"FOO=[$env:FOO] GH=[$env:GH_TOKEN] TERM=[$env:TERM] 'héllo' wörld\"; cmd /c exit 5"),
    ) else {
        return;
    };
    let events = wait_for(&rx, "the first prompt", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(5)),
            PromptState::AtPrompt,
        ]
    );
    assert!(reached(&events, &fx.start), "{events:?}");
    wait_text(&pane, "FOO=[bar] GH=[] TERM=[xterm-256color] 'héllo' wörld");
}

#[test]
fn cd_reports_the_new_folder() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.write(b"Set-Location -LiteralPath \"it's here\"\r").unwrap();
    let target = fx.start.join("it's here");
    let events = wait_for(&rx, "the next prompt", at_prompt);
    assert!(reached(&events, &target), "{events:?}");
}

#[test]
fn a_command_goes_running_then_done_with_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.write(b"cmd /c exit 3\r").unwrap();
    let events = wait_for(&rx, "the next prompt", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(3)),
            PromptState::AtPrompt,
        ]
    );
}

#[test]
fn exit_reports_the_code_and_a_new_pane_starts_after() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.write(b"exit 7\r").unwrap();
    wait_for(&rx, "the exit", |e| *e == TermEvent::Exited(Some(7)));
    drop(pane);
    // Restart is a fresh spawn (the app runs prepare again first).
    let (_again, rx) = fx.spawn(PWSH, None).unwrap();
    wait_for(&rx, "the restarted prompt", at_prompt);
}

#[test]
fn the_pane_holds_the_shell_lock_and_its_script_until_dropped() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    assert!(is_running(&fx.paths, &fx.env()).unwrap());
    assert_eq!(fx.scripts(), 1);
    drop(pane);
    assert!(!is_running(&fx.paths, &fx.env()).unwrap());
    assert_eq!(fx.scripts(), 0);
}

#[test]
fn resize_reaches_the_shell_and_the_grid() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.resize(GridSize { cols: 80, rows: 20 }).unwrap();
    assert_eq!(
        pane.with_term(|t| (t.columns(), t.screen_lines())),
        (80, 20)
    );
    pane.write(
        b"Write-Output \"size=$($Host.UI.RawUI.WindowSize.Width)x$($Host.UI.RawUI.WindowSize.Height)\"\r",
    )
    .unwrap();
    wait_text(&pane, "size=80x20");
}

#[test]
fn a_flood_of_output_sends_one_wakeup_until_the_app_renders() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, Some("1..20000 | % { \"line $_\" }")) else {
        return;
    };
    let mut events = wait_for(&rx, "the first prompt", at_prompt);
    wait_text(&pane, "line 20000");
    events.extend(rx.try_iter());
    let wakeups = events.iter().filter(|e| **e == TermEvent::Wakeup).count();
    assert_eq!(wakeups, 1, "{events:?}");
    pane.with_term(|_| ());
    pane.write(b"Write-Output again\r").unwrap();
    wait_for(&rx, "a new wakeup after rendering", |e| *e == TermEvent::Wakeup);
}

#[test]
fn dropping_a_busy_pane_ends_the_whole_tree_promptly() {
    let fx = Fx::new();
    let pid_file = fx.dir.path().join("pid.txt");
    // ping is the shell's child, started from the startup command; Start-Sleep keeps the
    // shell busy so the pane is closed mid-command.
    let command = format!(
        "$p = [System.Diagnostics.Process]::Start('ping.exe', '-n 120 127.0.0.1'); Set-Content -LiteralPath '{}' $p.Id; Start-Sleep 120",
        pid_file.display()
    );
    let Some((pane, _rx)) = fx.spawn(PWSH, Some(&command)) else {
        return;
    };
    let ping = open_process(wait_for_pid(&pid_file));
    assert!(!exited(ping, 0), "ping should still be running");
    let start = Instant::now();
    drop(pane);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "closing took {:?}",
        start.elapsed()
    );
    assert!(exited(ping, 10_000), "ping outlived its pane");
    // SAFETY: the handle came from `open_process` and is closed once.
    unsafe { CloseHandle(ping) };
}

fn wait_for_pid(file: &Path) -> u32 {
    let deadline = Instant::now() + common::TIMEOUT;
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the startup command never wrote {}",
            file.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn open_process(pid: u32) -> HANDLE {
    // SAFETY: a plain call; the result is checked.
    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!h.is_null(), "can't open process {pid}");
    h
}

/// Whether the process ended within `ms` milliseconds.
fn exited(process: HANDLE, ms: u32) -> bool {
    // SAFETY: `process` is a live handle from `open_process`.
    unsafe { WaitForSingleObject(process, ms) == WAIT_OBJECT_0 }
}
```

- [ ] **Step 8: Run the real-shell tests.** `cargo test -p hydraui-term --test pwsh` — expected: PASS (8 tests), none printing "skipped".

  If a test fails, look at what it got before changing it:
  - **No `CommandRunning` after typing a command** (`a_command_goes_running…` shows `[CommandDone(..), AtPrompt]` without it): pwsh didn't call the wrapped `PSConsoleHostReadLine`. Fix it in `hydra-platform`'s `pwsh_integration` (switch to wrapping PSReadLine's Enter key handler: `Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock { …emit C…; [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine() }`, the scheme in spec §6.3), keeping plan 1's `shell.rs` tests passing. Don't weaken this test.
  - **pwsh stalls with an empty screen:** the listener isn't writing `PtyWrite` replies back (spike note).
  - **Garbled `héllo`:** compare the init script's bytes (UTF-8, no BOM) with what pwsh shows; pwsh 7 reads scripts as UTF-8.

- [ ] **Step 9: Align the spec.** In `docs/superpowers/specs/2026-10-17-hydraui-design.md` §6.1, replace:

  > `Pane::spawn(PaneSpec) -> Result<Pane, SpawnError>`, where `PaneSpec` holds
  > the env name, cwd, shell kind, startup command and the `LaunchEnv` already
  > prepared by the app:

  with:

  > `Pane::spawn(SpawnSpec) -> Result<(Pane, Receiver<TermEvent>), SpawnError>`,
  > where `SpawnSpec` (named apart from `hydraui-sets`' `PaneSpec`) holds hydra's
  > paths, the `LaunchEnv` already prepared by the app, the cwd (which must
  > exist), shell kind, `git_bash` setting, startup command, grid size and the
  > inherited environment:

  and in step 3 of §6.1 replace "applying `env_changes()`." with "applying `env_changes()` on top of `TERM=xterm-256color` and `COLORTERM=truecolor` (an environment's `[env]` can override them)."

  In §6.2, after "The UI renders from the shared grid at most once per frame on `Wakeup`, so heavy output never blocks input." add: "A pane sends one `Wakeup` until the app next reads the grid (`Pane::with_term`), so heavy output never floods the channel either. `Title("")` means the title was reset."

- [ ] **Step 10: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui-term docs/superpowers/specs/2026-10-17-hydraui-design.md
git commit -m "feat: hydraui-term pane with ConPTY, grid, events and job object

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Git Bash panes

**Files:**
- Create: `crates/hydraui-term/tests/bash.rs`
- Test: integration tests in `tests/bash.rs`

**Interfaces:**
- Consumes: `tests/common/mod.rs` helpers (`Fx`, `at_prompt`, `prompts`, `reached`, `wait_for`, `wait_text`) and the `Pane` API (Task 4).
- Produces: nothing new; proves the Task 4 pane works with Git Bash (`--rcfile`, `CHERE_INVOKING`, `pwd -W` in OSC 7, the DEBUG-trap `C` mark).

- [ ] **Step 1: Write the tests.** `crates/hydraui-term/tests/bash.rs`:

```rust
//! Git Bash in a real ConPTY. Each test prints "skipped" and passes when Git Bash isn't
//! installed.
mod common;

use common::{Fx, at_prompt, prompts, reached, wait_for, wait_text};
use hydra_platform::shell::ShellKind;
use hydraui_term::{PromptState, TermEvent};

const BASH: ShellKind = ShellKind::Bash;

#[test]
fn startup_command_sees_the_environment_and_reports_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(
        BASH,
        Some("echo \"FOO=[$FOO] GH=[$GH_TOKEN] TERM=[$TERM]\"; false"),
    ) else {
        return;
    };
    let events = wait_for(&rx, "the first prompt", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(1)),
            PromptState::AtPrompt,
        ]
    );
    assert!(reached(&events, &fx.start), "{events:?}");
    wait_text(&pane, "FOO=[bar] GH=[] TERM=[xterm-256color]");
}

#[test]
fn cd_commands_and_exit() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(BASH, None) else {
        return;
    };
    let events = wait_for(&rx, "the first prompt", at_prompt);
    assert_eq!(prompts(&events), vec![PromptState::AtPrompt]);

    pane.write(b"cd \"it's here\"\r").unwrap();
    let events = wait_for(&rx, "the prompt after cd", at_prompt);
    assert!(reached(&events, &fx.start.join("it's here")), "{events:?}");

    pane.write(b"(exit 4)\r").unwrap();
    let events = wait_for(&rx, "the prompt after (exit 4)", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(4)),
            PromptState::AtPrompt,
        ]
    );

    pane.write(b"exit 6\r").unwrap();
    wait_for(&rx, "the exit", |e| *e == TermEvent::Exited(Some(6)));
}
```

- [ ] **Step 2: Run them.** `cargo test -p hydraui-term --test bash` — expected: PASS (2 tests), none printing "skipped" on a machine with Git for Windows.

  If a test fails, look at what it got before changing it:
  - **No `CwdChanged`, or a path like `\c\Users\…`:** `pwd -W` didn't run; check the bash `__hydraui_prompt` in `hydra-platform`'s `BASH_INTEGRATION`.
  - **Two `CommandRunning` per command, or none:** the DEBUG trap's `__hydraui_armed` guard in `BASH_INTEGRATION`; fix it there and keep plan 1's `shell.rs` tests passing.
  - **`TERM=[cygwin]` or similar:** something in `/etc/profile` overrides `TERM`; report it rather than dropping the assertion — hydraui's colours depend on it.

  A fix to `hydra-platform` goes in this task's commit; the CLI's scripts (integration off) must stay byte-identical (plan 1's `integration_off_leaves_the_cli_scripts_alone`).

- [ ] **Step 3: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui-term/tests/bash.rs
git commit -m "test: hydraui-term with Git Bash panes

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 4: Close out the plan.** Run the full gate once more (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`) and confirm every `hydraui-term` real-shell test ran rather than skipped (`cargo test -p hydraui-term -- --nocapture 2>&1 | grep skipped` prints nothing).
