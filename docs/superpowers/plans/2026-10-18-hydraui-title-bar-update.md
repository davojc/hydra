# hydraui Title Bar and Update Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** hydraui draws its own title bar, and shows an **Update hydra <version>** button there that updates `hydraui.exe` and `hydra.exe` from GitHub and restarts on request.

**Architecture:** The release check, download, checksum and exe swap move out of `hydra-cli` into a new `hydra-update` crate (with an all-or-nothing swap of several exes), used unchanged by `hydra update` and by hydraui. hydraui gets a runtime `VERSION`, a UI-free `updater` module (offer rules, finding `hydra.exe`, install, clean-up) and a title bar module that renders gpui-component's `TitleBar` with the button. CI checks hydraui's stamped version.

**Tech Stack:** Rust 2024 workspace; `ureq` 2, `sha2`, `semver`, `serde_json`, `anyhow`; GPUI (`gpui-pre` 0.3.9) and `gpui-component` =0.7.3 (`TitleBar`); GitHub Actions (windows-latest).

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` §7.11 (title bar and updates), §7.7 (packaging), §3 (crates).

## Global Constraints

- Release asset names: `hydra-x86_64-pc-windows-msvc.exe` and `hydraui-x86_64-pc-windows-msvc.exe`, each with a `.sha256` (first token, 64 hex chars).
- Latest-release URL: `{HYDRA_UPDATE_API or https://api.github.com}/repos/davojc/hydra/releases/latest`.
- Version: `HYDRA_VERSION` at compile time in release builds, else `<Cargo version>-dev`; a `-dev` build never shows the button.
- Checks at start and every 6 hours, off the UI thread; no network or an unreadable answer shows nothing and says nothing.
- Refuse while any `hydra.exe` or `hydra.old.exe` runs; the toast names the processes.
- `hydra.exe` is the one next to `hydraui.exe`, else the first on `PATH`; with none, only hydraui is updated and the toast says so.
- Swap: current exe renamed `<name>.old.exe`, new one takes its name; everything downloaded and checked before anything is renamed; a failed rename puts back what was already swapped.
- Button states: **Update hydra <version>** → "Updating…" → **Restart to finish**; a failure is a toast with the reason and the button stays.
- Restart saves the session, starts the new `hydraui.exe`, closes this window; never automatic.
- At start, delete a left-over `hydraui.old.exe` next to hydraui and `hydra.old.exe` next to the found `hydra.exe`, ignoring failures.
- `hydra update`'s behaviour and output don't change.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`.
- Tests never touch the real `~/.hydra`, the real install folder, global git config or Credential Manager; network tests use a local fake server.
- Gate before each commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (check its own exit status), `cargo test --workspace`.

## Review Focus

1. A release that lacks the hydraui asset (or its `.sha256`) must fail with a clear message and change nothing. Pinned in Task 2 (`a_release_without_hydraui_changes_nothing`).
2. A rename failing on the second exe (hydra.exe locked or read-only) must put the first exe back. Pinned in Task 1 (`a_failed_second_swap_puts_the_first_back`).
3. A `-dev` build must never be offered an update, even though `0.6.0-dev < 0.6.0`. Pinned in Task 2 (`dev_builds_are_never_offered_an_update`).
4. Clicking Update again while it is updating, or after it finished, must not start a second install. Pinned in Task 2 (`only_an_offer_can_start_an_install`).
5. GitHub unreachable must be an `Err` the UI drops silently, not a panic or a hang. Pinned in Task 2 (`an_unreachable_github_is_an_error_not_a_panic`).

---

### Task 1: The `hydra-update` crate

Move the update logic out of `hydra-cli` so hydraui can use it, add an all-or-nothing swap of several exes, and keep `hydra update` exactly as it is.

**Files:**
- Create: `crates/hydra-update/Cargo.toml`, `crates/hydra-update/src/lib.rs`
- Move: `crates/hydra-cli/src/release.rs` → `crates/hydra-update/src/release.rs` (`git mv`)
- Modify: `Cargo.toml` (workspace dependency), `crates/hydra-cli/Cargo.toml`, `crates/hydra-cli/src/main.rs` (drop `mod release;`), `crates/hydra-cli/src/commands/update.rs`
- Test: `crates/hydra-update/src/lib.rs`, `crates/hydra-update/src/release.rs` (moved tests), `crates/hydra-cli/tests/update_cli.rs` (unchanged, must pass)

**Interfaces:**
- Produces (in `hydra_update`):
  - `pub mod release` — `Release { tag, assets }`, `Asset { name, url }`, `WindowsBuild { exe_url, sha256_url }`, `Release::parse(&str) -> anyhow::Result<Release>`, `Release::version(&self) -> anyhow::Result<semver::Version>`, `Release::windows_build(&self, asset: &str) -> anyhow::Result<WindowsBuild>`, `is_newer(&Version, &Version) -> bool`, `parse_checksum(&str) -> Option<String>`, `hydra_pids_from_tasklist`, `hydra_pids_from_pgrep`, `HYDRA_IMAGES`.
  - `pub const HYDRA_ASSET: &str = "hydra-x86_64-pc-windows-msvc.exe";`
  - `pub const HYDRAUI_ASSET: &str = "hydraui-x86_64-pc-windows-msvc.exe";`
  - `pub fn latest_url() -> String` (honours `HYDRA_UPDATE_API`)
  - `pub fn fetch_latest(url: &str, user_agent: &str) -> anyhow::Result<Release>`
  - `pub fn download_verified(build: &WindowsBuild, new: &Path, user_agent: &str) -> anyhow::Result<()>`
  - `pub struct Swap { pub target: PathBuf, pub new: PathBuf, pub old: PathBuf }`
  - `pub fn replace(swap: &Swap) -> anyhow::Result<()>` (today's single swap, same messages)
  - `pub fn replace_all(swaps: &[Swap]) -> anyhow::Result<()>`
  - `pub fn running_hydra_pids() -> Result<Vec<u32>, String>`

- [ ] **Step 1: Create the crate and move `release.rs`**

```bash
mkdir -p crates/hydra-update/src
git mv crates/hydra-cli/src/release.rs crates/hydra-update/src/release.rs
```

`crates/hydra-update/Cargo.toml`:

```toml
[package]
name = "hydra-update"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
anyhow.workspace = true
semver.workspace = true
serde_json.workspace = true
sha2.workspace = true
ureq.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

Root `Cargo.toml`, under `[workspace.dependencies]` next to the other path crates:

```toml
hydra-update = { path = "crates/hydra-update" }
```

In `crates/hydra-update/src/release.rs`: change the module doc's first line to `//! Pure logic for updating: reading GitHub's release JSON, comparing versions,`; delete `pub const EXE_ASSET`; change `windows_build` to take the asset:

```rust
    /// Download URLs of `asset` and its `.sha256` file.
    pub fn windows_build(&self, asset: &str) -> anyhow::Result<WindowsBuild> {
        let url = |name: &str| {
            self.assets
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.url.clone())
        };
        match (url(asset), url(&format!("{asset}.sha256"))) {
            (Some(exe_url), Some(sha256_url)) => Ok(WindowsBuild {
                exe_url,
                sha256_url,
            }),
            _ => anyhow::bail!("release {} has no {asset}", self.tag),
        }
    }
```

In its tests, replace uses of `EXE_ASSET` with `crate::HYDRA_ASSET` and calls `windows_build()` with `windows_build(crate::HYDRA_ASSET)`. Keep the existing `no Windows build` test's intent: it now asserts the error contains `has no hydra-x86_64-pc-windows-msvc.exe`.

- [ ] **Step 2: Write the failing tests** (`crates/hydra-update/src/lib.rs`, tests module)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn swap_in(dir: &Path, name: &str) -> Swap {
        let stem = name.trim_end_matches(".exe");
        Swap {
            target: dir.join(name),
            new: dir.join(format!("{stem}.new.exe")),
            old: dir.join(format!("{stem}.old.exe")),
        }
    }

    #[test]
    fn failed_install_puts_the_old_version_back() {
        let dir = tempfile::tempdir().unwrap();
        let s = swap_in(dir.path(), "hydra.exe");
        std::fs::write(&s.target, "old").unwrap();
        let err = replace(&s).unwrap_err();
        assert_eq!(std::fs::read_to_string(&s.target).unwrap(), "old");
        assert!(!s.old.exists());
        assert!(
            format!("{err:#}").contains("the old version is back in place"),
            "{err:#}"
        );
    }

    #[test]
    fn every_exe_is_swapped() {
        let dir = tempfile::tempdir().unwrap();
        let swaps = [swap_in(dir.path(), "hydraui.exe"), swap_in(dir.path(), "hydra.exe")];
        for s in &swaps {
            std::fs::write(&s.target, "old").unwrap();
            std::fs::write(&s.new, "new").unwrap();
        }
        replace_all(&swaps).unwrap();
        for s in &swaps {
            assert_eq!(std::fs::read_to_string(&s.target).unwrap(), "new");
            assert!(!s.new.exists());
        }
    }

    #[test]
    fn a_failed_second_swap_puts_the_first_back() {
        let dir = tempfile::tempdir().unwrap();
        let first = swap_in(dir.path(), "hydraui.exe");
        let second = swap_in(dir.path(), "hydra.exe");
        std::fs::write(&first.target, "old ui").unwrap();
        std::fs::write(&first.new, "new ui").unwrap();
        std::fs::write(&second.target, "old cli").unwrap();
        // No second.new: its swap fails after the first succeeded.
        let err = replace_all(&[first.clone(), second.clone()]).unwrap_err();
        assert_eq!(std::fs::read_to_string(&first.target).unwrap(), "old ui");
        assert_eq!(std::fs::read_to_string(&second.target).unwrap(), "old cli");
        assert!(!first.new.exists() && !first.old.exists());
        assert!(format!("{err:#}").contains("nothing changed"), "{err:#}");
    }

    #[test]
    fn this_machines_process_list_can_be_read() {
        // The real tasklist / pgrep output must parse, or every update would be refused.
        running_hydra_pids().unwrap();
    }
}
```

(`Swap` derives `Clone`, `Debug`.)

- [ ] **Step 3: Run to see them fail**

Run: `cargo test -p hydra-update`
Expected: compile errors — `replace`, `replace_all`, `Swap`, `running_hydra_pids` not found.

- [ ] **Step 4: Implement** (`crates/hydra-update/src/lib.rs`, above the tests)

```rust
//! Updating hydra's executables from GitHub releases: shared by `hydra update` and
//! hydraui's Update button (spec §7.11).
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};

pub mod release;

use release::{Release, WindowsBuild};

/// The release assets.
pub const HYDRA_ASSET: &str = "hydra-x86_64-pc-windows-msvc.exe";
pub const HYDRAUI_ASSET: &str = "hydraui-x86_64-pc-windows-msvc.exe";

/// GitHub's latest-release URL; `HYDRA_UPDATE_API` points it elsewhere (tests).
pub fn latest_url() -> String {
    let api =
        std::env::var("HYDRA_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into());
    format!("{}/repos/davojc/hydra/releases/latest", api.trim_end_matches('/'))
}

fn get(url: &str, user_agent: &str) -> anyhow::Result<ureq::Response> {
    match ureq::get(url)
        .set("User-Agent", user_agent)
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(code, _)) => anyhow::bail!("GitHub answered HTTP {code} for {url}"),
        Err(e) => anyhow::bail!("can't reach GitHub: {e}"),
    }
}

/// The latest release's info.
pub fn fetch_latest(url: &str, user_agent: &str) -> anyhow::Result<Release> {
    Release::parse(
        &get(url, user_agent)?
            .into_string()
            .context("can't read GitHub's answer")?,
    )
}

/// Downloads the exe to `new` and checks it against the release's .sha256 file.
pub fn download_verified(build: &WindowsBuild, new: &Path, user_agent: &str) -> anyhow::Result<()> {
    let mut body = get(&build.exe_url, user_agent)?.into_reader();
    let mut file =
        fs::File::create(new).with_context(|| format!("can't write {}", new.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = body
            .read(&mut buf)
            .context("download of the new hydra failed")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("can't write {}", new.display()))?;
    }
    file.sync_all()
        .with_context(|| format!("can't write {}", new.display()))?;
    drop(file);

    let sums = get(&build.sha256_url, user_agent)?
        .into_string()
        .context("download of the checksum failed")?;
    let expected =
        release::parse_checksum(&sums).context("the release's checksum file can't be read")?;
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        anyhow::bail!("checksum mismatch - nothing changed");
    }
    Ok(())
}

/// One exe to replace: `target` moves to `old`, `new` takes its place.
#[derive(Debug, Clone)]
pub struct Swap {
    pub target: PathBuf,
    pub new: PathBuf,
    pub old: PathBuf,
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// target -> old, new -> target; puts the old exe back if the second step fails. The old
/// exe stays at `old` (so `replace_all` can still undo it).
fn swap_keeping_old(swap: &Swap) -> anyhow::Result<()> {
    let Swap { target, new, old } = swap;
    let _ = fs::remove_file(old);
    if let Err(e) = fs::rename(target, old) {
        let _ = fs::remove_file(new);
        return Err(e)
            .with_context(|| format!("can't move {} aside - nothing changed", target.display()));
    }
    if let Err(e) = fs::rename(new, target) {
        let restored = fs::rename(old, target);
        let _ = fs::remove_file(new);
        let what = if restored.is_ok() {
            "the old version is back in place".to_string()
        } else {
            format!("the old version is {}", file_name(old))
        };
        return Err(e).with_context(|| {
            format!(
                "can't install the new {} at {} ({what})",
                file_name(target).trim_end_matches(".exe"),
                target.display()
            )
        });
    }
    Ok(())
}

/// One exe: swap it, then delete the old one (which fails while it is still running; the
/// next start removes it).
pub fn replace(swap: &Swap) -> anyhow::Result<()> {
    swap_keeping_old(swap)?;
    let _ = fs::remove_file(&swap.old);
    Ok(())
}

/// Every swap, or none: if one fails, those already done are put back.
pub fn replace_all(swaps: &[Swap]) -> anyhow::Result<()> {
    for (i, swap) in swaps.iter().enumerate() {
        if let Err(e) = swap_keeping_old(swap) {
            // An earlier swap's target is the new, never-run exe, so it can be deleted.
            for done in swaps[..i].iter().rev() {
                let _ = fs::remove_file(&done.target);
                let _ = fs::rename(&done.old, &done.target);
            }
            for later in &swaps[i..] {
                let _ = fs::remove_file(&later.new);
            }
            return Err(e).context("nothing changed");
        }
    }
    for swap in swaps {
        let _ = fs::remove_file(&swap.old);
    }
    Ok(())
}
```

Then the process check, moved from `update.rs`:

```rust
/// PIDs of running hydra processes (hydra.exe and hydra.old.exe), or why they can't be
/// listed.
#[cfg(windows)]
pub fn running_hydra_pids() -> Result<Vec<u32>, String> {
    let out = std::process::Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .map_err(|e| format!("tasklist didn't run: {e}"))?;
    if !out.status.success() {
        return Err(format!("tasklist failed with {}", out.status));
    }
    release::hydra_pids_from_tasklist(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
pub fn running_hydra_pids() -> Result<Vec<u32>, String> {
    let out = std::process::Command::new("pgrep")
        .args(["-x", r"hydra|hydra\.old"])
        .output()
        .map_err(|e| format!("pgrep didn't run: {e}"))?;
    release::hydra_pids_from_pgrep(out.status.code(), &String::from_utf8_lossy(&out.stdout))
}
```

Remove the `#[cfg_attr(..., allow(dead_code))]` attributes on `HYDRA_IMAGES`, `hydra_pids_from_tasklist` and `hydra_pids_from_pgrep` only if clippy no longer needs them (they are `pub` in a library now, so it won't).

- [ ] **Step 5: Run to see them pass**

Run: `cargo test -p hydra-update`
Expected: all pass (the moved release tests and the 4 new ones).

- [ ] **Step 6: Point `hydra update` at the crate**

`crates/hydra-cli/Cargo.toml` `[dependencies]`: add `hydra-update.workspace = true`; remove `sha2` and `ureq` from `[dependencies]` only if nothing else in hydra-cli uses them (`grep -rn "ureq\|sha2" crates/hydra-cli/src`). Keep `sha2` in `[dev-dependencies]` (the CLI test uses it).

`crates/hydra-cli/src/main.rs`: delete `mod release;`.

`crates/hydra-cli/src/commands/update.rs` becomes:

```rust
//! `hydra update`: install the latest GitHub release over the running exe.

use std::path::PathBuf;

use anyhow::Context;
use hydra_update::release;
use hydra_update::{HYDRA_ASSET, Swap, download_verified, fetch_latest, latest_url, replace};
use semver::Version;

use crate::VERSION;
use crate::style;

/// The replaced exe. Windows can't delete a running exe, so the next hydra start removes it.
pub const OLD_EXE: &str = "hydra.old.exe";
const NEW_EXE: &str = "hydra.new.exe";

fn user_agent() -> String {
    format!("hydra/{VERSION}")
}

pub fn run(check: bool, force: bool) -> anyhow::Result<i32> {
    let release = fetch_latest(&latest_url(), &user_agent())?;
    // ... unchanged from here to `let build = ...`, except `release::is_newer` stays as is ...
    let build = release.windows_build(HYDRA_ASSET)?;
    let target = match std::env::var_os("HYDRA_UPDATE_EXE") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().context("can't find the running hydra exe")?,
    };
    let dir = target.parent().context("the hydra exe has no folder")?;
    let new = dir.join(NEW_EXE);
    if let Err(e) = download_verified(&build, &new, &user_agent()) {
        let _ = std::fs::remove_file(&new);
        return Err(e);
    }
    replace(&Swap {
        target: target.clone(),
        new,
        old: dir.join(OLD_EXE),
    })?;
    anstream::println!("{}", style::ok(format!("updated to {latest}")));
    Ok(0)
}

/// PIDs of other running hydra processes (hydra.exe and hydra.old.exe). Fails closed: if
/// the processes can't be listed, the update is refused.
/// `HYDRA_UPDATE_SKIP_PROCESS_CHECK=1` skips the check (tests).
fn other_hydra_pids() -> anyhow::Result<Vec<u32>> {
    if std::env::var("HYDRA_UPDATE_SKIP_PROCESS_CHECK").as_deref() == Ok("1") {
        return Ok(Vec::new());
    }
    let pids = hydra_update::running_hydra_pids().map_err(|reason| {
        anyhow::anyhow!("can't check whether hydra is running ({reason}) - not updating")
    })?;
    let me = std::process::id();
    Ok(pids.into_iter().filter(|&p| p != me).collect())
}
```

Keep the rest of `run` (version compare, `--check`, the in-use refusal text) exactly as it is. Delete `get`, `download_verified`, `replace`, both `running_hydra_pids`, and the tests module from `update.rs` (they moved). The old `"the old version is hydra.old.exe"` message is now `format!("the old version is {}", file_name(old))`, which gives the same text for hydra.

- [ ] **Step 7: Run the CLI's tests**

Run: `cargo test -p hydra-cli --test update_cli` then `cargo test --workspace`
Expected: all pass, `update_cli` unchanged.

- [ ] **Step 8: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A Cargo.toml Cargo.lock crates
git commit -m "refactor: hydra-update crate shared by hydra update and hydraui"
```

---

### Task 2: hydraui's version and updater (no UI)

**Files:**
- Create: `crates/hydraui/src/version.rs`, `crates/hydraui/src/updater.rs`, `crates/hydra-update/src/fake.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod updater; pub mod version;`), `crates/hydraui/Cargo.toml` (`hydra-update.workspace = true`, `semver.workspace = true`; dev: `hydra-update = { workspace = true, features = ["fake"] }`), `crates/hydra-update/Cargo.toml` (feature `fake`), `crates/hydra-update/src/lib.rs` (`#[cfg(any(test, feature = "fake"))] pub mod fake;`)
- Test: `crates/hydraui/src/updater.rs`

**Interfaces:**
- Consumes: Task 1's `hydra_update::{fetch_latest, download_verified, replace_all, Swap, HYDRA_ASSET, HYDRAUI_ASSET, release::is_newer}`.
- Produces:
  - `hydraui::version::VERSION: &str`
  - `hydra_update::fake::FakeGitHub` — `FakeGitHub::serve(tag: &str, assets: &[(&str, &[u8])]) -> FakeGitHub` (each asset served with a correct `.sha256` unless the asset list holds an explicit `"<name>.sha256"` entry), `.latest_url(&self) -> String`.
  - `hydraui::updater::offer(current: &str, latest: &semver::Version) -> bool`
  - `hydraui::updater::check(url: &str) -> anyhow::Result<Option<semver::Version>>` (newer release to offer, against `VERSION`)
  - `hydraui::updater::check_against(url: &str, current: &str) -> anyhow::Result<Option<semver::Version>>`
  - `hydraui::updater::find_hydra(hydraui_exe: &Path, path_var: Option<&OsStr>) -> Option<PathBuf>`
  - `pub struct Installed { pub version: semver::Version, pub hydra_updated: bool }`
  - `hydraui::updater::install(url: &str, hydraui_exe: &Path, hydra_exe: Option<&Path>, running: &dyn Fn() -> Result<Vec<u32>, String>) -> anyhow::Result<Installed>`
  - `hydraui::updater::clean_old(hydraui_exe: &Path, hydra_exe: Option<&Path>)`
  - `pub enum UpdateState { Idle, Offered(semver::Version), Updating(semver::Version), Ready(semver::Version) }` with `fn can_start(&self) -> bool`.

- [ ] **Step 1: The runtime version** (`crates/hydraui/src/version.rs`)

```rust
//! hydraui's version (spec §7.11): the release's in release builds, else `<Cargo>-dev`.

/// `HYDRA_VERSION` at compile time (release builds), else `<Cargo version>-dev`.
pub const VERSION: &str = match option_env!("HYDRA_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};
```

`crates/hydraui/build.rs` already has `cargo:rerun-if-env-changed=HYDRA_VERSION`.

- [ ] **Step 2: The fake GitHub server** (`crates/hydra-update/src/fake.rs`)

`crates/hydra-update/Cargo.toml`:

```toml
[features]
fake = []
```

```rust
//! A local stand-in for GitHub's release API, for tests (feature `fake`).
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sha2::{Digest, Sha256};

pub struct FakeGitHub {
    base: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl FakeGitHub {
    /// Serves a latest release `tag` with `assets` (name, bytes). Each asset gets a correct
    /// `<name>.sha256` unless `assets` names one itself.
    pub fn serve(tag: &str, assets: &[(&str, &[u8])]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut routes: HashMap<String, Vec<u8>> = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        for (name, bytes) in assets {
            routes.insert(format!("/dl/{name}"), bytes.to_vec());
            names.push(name.to_string());
        }
        for (name, bytes) in assets {
            let sha = format!("{name}.sha256");
            if !name.ends_with(".sha256") && !routes.contains_key(&format!("/dl/{sha}")) {
                routes.insert(
                    format!("/dl/{sha}"),
                    format!("{}  {name}\n", sha256_hex(bytes)).into_bytes(),
                );
                names.push(sha);
            }
        }
        let list: Vec<String> = names
            .iter()
            .map(|n| format!(r#"{{"name":"{n}","browser_download_url":"{base}/dl/{n}"}}"#))
            .collect();
        let json = format!(r#"{{"tag_name":"{tag}","assets":[{}]}}"#, list.join(","));
        routes.insert("/repos/davojc/hydra/releases/latest".into(), json.into_bytes());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream, &routes);
            }
        });
        Self { base }
    }

    /// The latest-release URL to pass to `fetch_latest`.
    pub fn latest_url(&self) -> String {
        format!("{}/repos/davojc/hydra/releases/latest", self.base)
    }
}

fn answer(mut stream: TcpStream, routes: &HashMap<String, Vec<u8>>) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match routes.get(path) {
        Some(b) => ("200 OK", b.clone()),
        None => ("404 Not Found", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}
```

- [ ] **Step 3: Write the failing tests** (`crates/hydraui/src/updater.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use hydra_update::fake::FakeGitHub;
    use hydra_update::{HYDRA_ASSET, HYDRAUI_ASSET};
    use semver::Version;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn none_running() -> Result<Vec<u32>, String> {
        Ok(Vec::new())
    }

    #[test]
    fn dev_builds_are_never_offered_an_update() {
        assert!(!offer("0.6.0-dev", &v("0.6.0")));
        assert!(!offer("0.6.0-dev", &v("0.9.0")));
        assert!(offer("0.6.0", &v("0.6.1")));
        assert!(!offer("0.6.1", &v("0.6.1")));
        assert!(!offer("0.7.0", &v("0.6.1")));
        assert!(!offer("not a version", &v("0.6.1")));
    }

    #[test]
    fn a_newer_release_is_offered() {
        let gh = FakeGitHub::serve("v0.7.0", &[(HYDRAUI_ASSET, b"ui")]);
        assert_eq!(check_against(&gh.latest_url(), "0.6.0").unwrap(), Some(v("0.7.0")));
        assert_eq!(check_against(&gh.latest_url(), "0.7.0").unwrap(), None);
        assert_eq!(check_against(&gh.latest_url(), "0.6.0-dev").unwrap(), None);
    }

    #[test]
    fn an_unreachable_github_is_an_error_not_a_panic() {
        // Port 9 (discard) on localhost: nothing listens, the connection is refused.
        assert!(check_against("http://127.0.0.1:9/repos/davojc/hydra/releases/latest", "0.6.0").is_err());
    }

    #[test]
    fn hydra_is_found_next_to_hydraui_then_on_path() {
        let d = tempfile::tempdir().unwrap();
        let (app, tools) = (d.path().join("app"), d.path().join("tools"));
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&tools).unwrap();
        let ui = app.join("hydraui.exe");
        std::fs::write(&ui, "").unwrap();
        std::fs::write(tools.join("hydra.exe"), "").unwrap();
        let path = std::env::join_paths([d.path().join("nowhere"), tools.clone()]).unwrap();
        assert_eq!(find_hydra(&ui, Some(&path)), Some(tools.join("hydra.exe")));
        std::fs::write(app.join("hydra.exe"), "").unwrap();
        assert_eq!(find_hydra(&ui, Some(&path)), Some(app.join("hydra.exe")));
        std::fs::remove_file(app.join("hydra.exe")).unwrap();
        assert_eq!(find_hydra(&ui, None), None);
    }

    #[test]
    fn both_exes_are_updated() {
        let gh = FakeGitHub::serve("v0.7.0", &[(HYDRAUI_ASSET, b"new ui"), (HYDRA_ASSET, b"new cli")]);
        let d = tempfile::tempdir().unwrap();
        let (ui, cli) = (d.path().join("hydraui.exe"), d.path().join("hydra.exe"));
        std::fs::write(&ui, "old ui").unwrap();
        std::fs::write(&cli, "old cli").unwrap();
        let done = install(&gh.latest_url(), &ui, Some(&cli), &none_running).unwrap();
        assert_eq!(done.version, v("0.7.0"));
        assert!(done.hydra_updated);
        assert_eq!(std::fs::read(&ui).unwrap(), b"new ui");
        assert_eq!(std::fs::read(&cli).unwrap(), b"new cli");
    }

    #[test]
    fn without_hydra_only_hydraui_is_updated() {
        let gh = FakeGitHub::serve("v0.7.0", &[(HYDRAUI_ASSET, b"new ui"), (HYDRA_ASSET, b"new cli")]);
        let d = tempfile::tempdir().unwrap();
        let ui = d.path().join("hydraui.exe");
        std::fs::write(&ui, "old ui").unwrap();
        let done = install(&gh.latest_url(), &ui, None, &none_running).unwrap();
        assert!(!done.hydra_updated);
        assert_eq!(std::fs::read(&ui).unwrap(), b"new ui");
    }

    #[test]
    fn a_running_hydra_stops_the_update_and_is_named() {
        let gh = FakeGitHub::serve("v0.7.0", &[(HYDRAUI_ASSET, b"new ui"), (HYDRA_ASSET, b"new cli")]);
        let d = tempfile::tempdir().unwrap();
        let ui = d.path().join("hydraui.exe");
        std::fs::write(&ui, "old ui").unwrap();
        let err = install(&gh.latest_url(), &ui, None, &|| Ok(vec![4242])).unwrap_err();
        assert!(format!("{err:#}").contains("4242"), "{err:#}");
        assert_eq!(std::fs::read(&ui).unwrap(), b"old ui");
        let err = install(&gh.latest_url(), &ui, None, &|| Err("no tasklist".into())).unwrap_err();
        assert!(format!("{err:#}").contains("no tasklist"), "{err:#}");
    }

    #[test]
    fn a_bad_checksum_changes_nothing() {
        let bad = format!("{}  {HYDRA_ASSET}\n", "0".repeat(64));
        let gh = FakeGitHub::serve(
            "v0.7.0",
            &[
                (HYDRAUI_ASSET, b"new ui"),
                (HYDRA_ASSET, b"new cli"),
                (&format!("{HYDRA_ASSET}.sha256"), bad.as_bytes()),
            ],
        );
        let d = tempfile::tempdir().unwrap();
        let (ui, cli) = (d.path().join("hydraui.exe"), d.path().join("hydra.exe"));
        std::fs::write(&ui, "old ui").unwrap();
        std::fs::write(&cli, "old cli").unwrap();
        let err = install(&gh.latest_url(), &ui, Some(&cli), &none_running).unwrap_err();
        assert!(format!("{err:#}").contains("checksum"), "{err:#}");
        assert_eq!(std::fs::read(&ui).unwrap(), b"old ui", "hydraui untouched too");
        assert_eq!(std::fs::read(&cli).unwrap(), b"old cli");
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "no .new.exe left behind: {left:?}");
    }

    #[test]
    fn a_release_without_hydraui_changes_nothing() {
        let gh = FakeGitHub::serve("v0.7.0", &[(HYDRA_ASSET, b"new cli")]);
        let d = tempfile::tempdir().unwrap();
        let ui = d.path().join("hydraui.exe");
        std::fs::write(&ui, "old ui").unwrap();
        let err = install(&gh.latest_url(), &ui, None, &none_running).unwrap_err();
        assert!(format!("{err:#}").contains(HYDRAUI_ASSET), "{err:#}");
        assert_eq!(std::fs::read(&ui).unwrap(), b"old ui");
    }

    #[test]
    fn old_exes_are_cleaned_up() {
        let d = tempfile::tempdir().unwrap();
        let (ui, cli) = (d.path().join("hydraui.exe"), d.path().join("hydra.exe"));
        std::fs::write(d.path().join("hydraui.old.exe"), "").unwrap();
        std::fs::write(d.path().join("hydra.old.exe"), "").unwrap();
        clean_old(&ui, Some(&cli));
        assert!(!d.path().join("hydraui.old.exe").exists());
        assert!(!d.path().join("hydra.old.exe").exists());
        clean_old(&ui, None); // nothing there: no panic
    }

    #[test]
    fn only_an_offer_can_start_an_install() {
        assert!(UpdateState::Offered(v("0.7.0")).can_start());
        assert!(!UpdateState::Idle.can_start());
        assert!(!UpdateState::Updating(v("0.7.0")).can_start());
        assert!(!UpdateState::Ready(v("0.7.0")).can_start());
    }
}
```

`find_hydra` with `None` for PATH in the last assert: `app` no longer has `hydra.exe`, so it returns `None`.

- [ ] **Step 4: Run to see them fail**

Run: `cargo test -p hydraui --lib updater`
Expected: compile errors — `offer`, `check_against`, `find_hydra`, `install`, `clean_old`, `UpdateState` not found.

- [ ] **Step 5: Implement** (`crates/hydraui/src/updater.rs`, above the tests)

```rust
//! hydraui's updates (spec §7.11): whether to offer one, finding hydra.exe, installing
//! both exes, and cleaning up after. No UI.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use hydra_update::release::is_newer;
use hydra_update::{
    HYDRA_ASSET, HYDRAUI_ASSET, Swap, download_verified, fetch_latest, replace_all,
};
use semver::Version;

use crate::version::VERSION;

fn user_agent() -> String {
    format!("hydraui/{VERSION}")
}

/// Whether a build at `current` should be offered `latest`: never a `-dev` build (or one
/// whose version can't be read).
pub fn offer(current: &str, latest: &Version) -> bool {
    match Version::parse(current) {
        Ok(c) => c.pre.is_empty() && is_newer(latest, &c),
        Err(_) => false,
    }
}

/// The release to offer a build at `current`, if any.
pub fn check_against(url: &str, current: &str) -> anyhow::Result<Option<Version>> {
    let latest = fetch_latest(url, &user_agent())?.version()?;
    Ok(offer(current, &latest).then_some(latest))
}

/// The release to offer this build, if any.
pub fn check(url: &str) -> anyhow::Result<Option<Version>> {
    check_against(url, VERSION)
}

/// The hydra.exe to update alongside hydraui: next to it, else the first on `path_var`.
pub fn find_hydra(hydraui_exe: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let beside = hydraui_exe.parent()?.join("hydra.exe");
    if beside.is_file() {
        return Some(beside);
    }
    std::env::split_paths(path_var?)
        .map(|dir| dir.join("hydra.exe"))
        .find(|p| p.is_file())
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: Version,
    pub hydra_updated: bool,
}

/// The swap for `exe`: `<stem>.new.exe` takes its place, it moves to `<stem>.old.exe`.
fn swap_for(exe: &Path) -> Swap {
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Swap {
        target: exe.to_path_buf(),
        new: dir.join(format!("{stem}.new.exe")),
        old: dir.join(format!("{stem}.old.exe")),
    }
}

/// Updates hydraui (and hydra, when given) to the latest release: refused while hydra runs
/// (`running` lists its processes); everything is downloaded and checked before anything
/// is replaced, and a failed replace puts back what was done.
pub fn install(
    url: &str,
    hydraui_exe: &Path,
    hydra_exe: Option<&Path>,
    running: &dyn Fn() -> Result<Vec<u32>, String>,
) -> anyhow::Result<Installed> {
    let pids = running()
        .map_err(|why| anyhow::anyhow!("can't check whether hydra is running ({why}) - not updating"))?;
    if !pids.is_empty() {
        let list: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
        anyhow::bail!(
            "hydra is in use - not updating. Close these first: pid {}",
            list.join(", pid ")
        );
    }
    let release = fetch_latest(url, &user_agent())?;
    let version = release.version()?;
    let mut planned = vec![(release.windows_build(HYDRAUI_ASSET)?, swap_for(hydraui_exe))];
    if let Some(cli) = hydra_exe {
        planned.push((release.windows_build(HYDRA_ASSET)?, swap_for(cli)));
    }
    for (build, swap) in &planned {
        if let Err(e) = download_verified(build, &swap.new, &user_agent()) {
            for (_, s) in &planned {
                let _ = std::fs::remove_file(&s.new);
            }
            return Err(e);
        }
    }
    let swaps: Vec<Swap> = planned.into_iter().map(|(_, s)| s).collect();
    replace_all(&swaps).context("the update couldn't be installed")?;
    Ok(Installed {
        version,
        hydra_updated: hydra_exe.is_some(),
    })
}

/// Deletes the exes a previous update replaced; they may still be running, so failures are
/// ignored.
pub fn clean_old(hydraui_exe: &Path, hydra_exe: Option<&Path>) {
    for exe in std::iter::once(hydraui_exe).chain(hydra_exe) {
        let _ = std::fs::remove_file(swap_for(exe).old);
    }
}

/// Where the title bar's Update button is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateState {
    /// Nothing to offer (or not checked yet).
    #[default]
    Idle,
    Offered(Version),
    Updating(Version),
    /// Installed; a restart finishes it.
    Ready(Version),
}

impl UpdateState {
    /// Only an offer starts an install: not a second click while one runs or after it.
    pub fn can_start(&self) -> bool {
        matches!(self, UpdateState::Offered(_))
    }
}
```

`lib.rs`: add `pub mod updater;` and `pub mod version;` in alphabetical order with the others.

- [ ] **Step 6: Run to see them pass**

Run: `cargo test -p hydraui --lib updater`
Expected: 11 passed.

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A crates Cargo.lock
git commit -m "feat: hydraui updater: version, offer, install both exes, clean up"
```

---

### Task 3: The title bar and its Update button

**Files:**
- Create: `crates/hydraui/src/workspace/title_bar.rs`
- Modify: `crates/hydraui/src/main.rs` (window options, `clean_old` at start), `crates/hydraui/src/workspace.rs` (`mod title_bar;`, state fields, start the check loop in `new`, render the title bar above the tab strip)
- Test: manual smoke (UI); logic is covered by Task 2

**Interfaces:**
- Consumes: Task 2's `updater::{check, install, find_hydra, UpdateState, Installed}`, `hydra_update::{latest_url, running_hydra_pids}`, `version::VERSION`.
- Produces: `Workspace::render_title_bar(&self, cx) -> impl IntoElement`; `Workspace::start_update_checks(&mut self, cx)`; `Workspace::start_update(&mut self, window, cx)`; `Workspace::restart(&mut self, window, cx)`.

- [ ] **Step 1: Window options and clean-up** (`crates/hydraui/src/main.rs`)

Replace the `options` block:

```rust
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(320.))),
                // hydraui draws its own title bar (spec §7.11).
                ..gpui_component::TitleBar::window_options()
            };
```

Drop the now-unused `TitlebarOptions` import. Before `gpui_platform::application()`, finish a previous update:

```rust
    // Exes a previous update replaced (spec §7.11).
    if let Ok(exe) = std::env::current_exe() {
        let hydra = hydraui::updater::find_hydra(&exe, std::env::var_os("PATH").as_deref());
        hydraui::updater::clean_old(&exe, hydra.as_deref());
    }
```

- [ ] **Step 2: State on the workspace** (`crates/hydraui/src/workspace.rs`)

Add `mod title_bar;` next to the other `mod` lines. Fields on `Workspace`:

```rust
    /// The title bar's Update button (spec §7.11).
    update: crate::updater::UpdateState,
    _update_checks: Option<Task<()>>,
```

Initialise `update: Default::default(), _update_checks: None,` in `Workspace::new`'s struct literal, and after the session is restored call `this.start_update_checks(cx);` (or `self.` — wherever `new` finishes setting up its other background tasks such as `git_watch`).

- [ ] **Step 3: The title bar module** (`crates/hydraui/src/workspace/title_bar.rs`)

```rust
//! hydraui's own title bar (spec §7.11): the title, the Update button, and the window
//! controls gpui-component draws.
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, Sizable, TitleBar, WindowExt, h_flex};

use super::Workspace;
use crate::updater::{self, UpdateState};

/// How often to ask GitHub again.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

impl Workspace {
    /// Asks GitHub now and every 6 hours, off the UI thread; failures say nothing.
    pub(super) fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        self._update_checks = Some(cx.spawn(async move |this, cx| {
            loop {
                let found = cx
                    .background_spawn(async { updater::check(&hydra_update::latest_url()) })
                    .await;
                let alive = this.update(cx, |ws, cx| {
                    if let Ok(Some(version)) = found
                        && ws.update == UpdateState::Idle
                    {
                        ws.update = UpdateState::Offered(version);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(EVERY).await;
            }
        }));
    }

    /// Updates both exes off the UI thread; the button shows the progress.
    pub(super) fn start_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Offered(version) = self.update.clone() else {
            return;
        };
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        self.update = UpdateState::Updating(version.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let hydra = updater::find_hydra(&exe, std::env::var_os("PATH").as_deref());
                    updater::install(
                        &hydra_update::latest_url(),
                        &exe,
                        hydra.as_deref(),
                        &hydra_update::running_hydra_pids,
                    )
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                match result {
                    Ok(done) => {
                        let mut text = format!("Updated to {}", done.version);
                        if !done.hydra_updated {
                            text.push_str(" (hydraui only: no hydra.exe found)");
                        }
                        window.push_notification(
                            Notification::success(text).title("Update hydra"),
                            cx,
                        );
                        ws.update = UpdateState::Ready(done.version);
                    }
                    Err(e) => {
                        window.push_notification(
                            Notification::error(format!("{e:#}")).title("Update hydra"),
                            cx,
                        );
                        ws.update = UpdateState::Offered(version);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the session, starts the new hydraui and closes this window; unsaved edits are
    /// asked about first, as when closing.
    pub(super) fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(window, cx);
        let relaunch = |window: &mut Window, cx: &mut gpui::App| {
            match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
                Ok(_) => window.remove_window(),
                Err(e) => window.push_notification(
                    Notification::error(format!("can't start the new hydraui: {e}"))
                        .title("Update hydra"),
                    cx,
                ),
            }
        };
        let all: Vec<_> = self.files.keys().copied().collect();
        if self.unsaved_in(&all, cx).is_empty() {
            relaunch(window, cx);
        } else {
            self.confirm_unsaved(all, window, cx, move |_, w, cx| relaunch(w, cx));
        }
    }

    pub(super) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let button = match &self.update {
            UpdateState::Idle => None,
            UpdateState::Offered(v) => Some(
                Button::new("update-hydra")
                    .label(format!("Update hydra {v}"))
                    .primary()
                    .xsmall()
                    .on_click(cx.listener(|ws, _, window, cx| ws.start_update(window, cx))),
            ),
            UpdateState::Updating(_) => Some(
                Button::new("update-hydra")
                    .label("Updating…")
                    .xsmall()
                    .disabled(true),
            ),
            UpdateState::Ready(_) => Some(
                Button::new("update-hydra")
                    .label("Restart to finish")
                    .primary()
                    .xsmall()
                    .on_click(cx.listener(|ws, _, window, cx| ws.restart(window, cx))),
            ),
        };
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .justify_between()
                .child(div().text_sm().text_color(muted).child(self.title.clone()))
                .children(button),
        )
    }
}
```

If `confirm_unsaved`'s closure signature differs (it is `FnOnce(&mut Workspace, &mut Window, &mut App)` or similar — read it in `workspace.rs` before writing `restart`), match it; the call site in `Workspace::new`'s `on_window_should_close` (`ws.confirm_unsaved(all, window, cx, |_, w, _| w.remove_window())`) is the model. `save_session` is `pub(super)` in `workspace/sets.rs`; `unsaved_in` and `confirm_unsaved` are private to `workspace.rs` — make them `pub(super)` if the compiler says so.

- [ ] **Step 4: Render it** (`crates/hydraui/src/workspace.rs`, `impl Render for Workspace`)

The root is `v_flex().size_full().bg(bg)...` with the tab strip as its first child. Add the title bar as the very first child, before the tab strip:

```rust
            .child(self.render_title_bar(cx))
```

(`self.title` already holds `hydraui - <tab>`; keep `window.set_window_title` so the taskbar shows it.)

- [ ] **Step 5: Build and smoke**

Run: `cargo build -p hydraui`, then launch the debug build with the scratch `HYDRA_HOME` (as earlier smoke tests do) and screenshot:
- the title bar row shows `hydraui - <tab>` on the left and minimise/maximise/close on the right, in the theme's colours; the tab strip sits below it;
- no Update button (a debug build is `-dev`).

To see the button, run with `HYDRA_UPDATE_API` pointed at a local fake (a tiny script or `python -m http.server` serving `repos/davojc/hydra/releases/latest` with `{"tag_name":"v9.9.9","assets":[]}`) — and because the debug build is `-dev`, also build once with `HYDRA_VERSION=0.6.0` set (`HYDRA_VERSION=0.6.0 cargo build -p hydraui`) so it isn't `-dev`. Expected: **Update hydra 9.9.9** appears just left of the window controls; clicking it toasts an error naming `hydraui-x86_64-pc-windows-msvc.exe` (the fake has no assets) and the button stays. Rebuild without `HYDRA_VERSION` afterwards.

Dragging and double-click-to-maximise can't be driven by the harness; note them for the user's hands-on check.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A crates
git commit -m "feat: hydraui title bar with an Update hydra button"
```

---

### Task 4: Release checks hydraui's version; docs

**Files:**
- Modify: `.github/workflows/ci.yml` (release job), `README.md` (Updating paragraph)

**Interfaces:** none.

- [ ] **Step 1: CI step** — in the `release` job, after "Check the version", add:

```yaml
      - name: Check hydraui's version
        if: steps.decide.outputs.release == 'true'
        shell: pwsh
        env:
          VERSION: ${{ steps.decide.outputs.version }}
        run: |
          $got = (Get-Item dist/hydraui-x86_64-pc-windows-msvc.exe).VersionInfo.ProductVersion
          Write-Host "hydraui $got"
          if ($got -ne $env:VERSION) {
            Write-Error "expected hydraui $env:VERSION, got '$got'"
            exit 1
          }
```

`crates/hydraui/build.rs` sets ProductVersion from `HYDRA_VERSION` when it is a plain `x.y.z`, which is what `scripts/next-version.sh` produces.

- [ ] **Step 2: Check the workflow parses**

Run: `python -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"` (skip if PyYAML isn't installed; read the step's indentation against its neighbours instead).
Expected: no error.

Locally, confirm the ProductVersion stamping works the way CI reads it:

```bash
HYDRA_VERSION=0.6.9 cargo build --release -p hydraui
pwsh -NoProfile -Command "(Get-Item target/release/hydraui.exe).VersionInfo.ProductVersion"
```

Expected: `0.6.9`. Then rebuild without `HYDRA_VERSION` (`cargo build --release -p hydraui`) so the local exe is a normal `-dev` build again.

- [ ] **Step 3: README** — replace the **Updating:** paragraph (README.md line 34, three sentences) with:

```markdown
**Updating:** `hydra update` installs the latest release after checking its SHA-256 checksum. `hydra update --check` only reports. It won't update while another hydra terminal is open. hydraui shows **Update hydra <version>** in its title bar when a release is out; it updates both `hydraui.exe` and `hydra.exe` the same way, then offers a restart.
```


- [ ] **Step 4: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add .github/workflows/ci.yml README.md
git commit -m "ci: release checks hydraui's version; docs: the Update button"
```
