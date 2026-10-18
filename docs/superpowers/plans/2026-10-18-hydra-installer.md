# hydra Installer (hydra-setup) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each release ships `hydra-setup.exe`, a small Rust installer (no third-party installer tool) that installs hydraui and hydra for the current user, tested in CI; the website and README offer it first, with the portable exes beside it.

**Architecture:** A new workspace binary `crates/hydra-setup`. Its core (`paths`, `system`, `install`) is UI-free and tested against a fake `System` (registry, shortcuts, environment broadcast) and temporary folders; the real `System` uses the `windows` crate (already in the build through gpui). Downloads and checksums reuse `hydra-update`, which gains a progress callback and a local-files source. A native task dialog (`dialog.rs`) drives install and uninstall. A PowerShell test script exercises the built exe against the real system (restoring `PATH`), in CI before each release and locally.

**Tech Stack:** Rust 2024; `windows` 0.61 (`Win32_UI_Shell`, `Win32_System_Com`, `Win32_UI_Controls`, `Win32_System_Registry`, `Win32_UI_WindowsAndMessaging`, `Win32_Foundation`); `hydra-update`; `winresource` (icon, version, comctl32 v6 manifest); PowerShell 7 for the system test.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` §7.7 (Installer).

## Global Constraints

- Release asset: `hydra-setup.exe` with `hydra-setup.exe.sha256` (`<lowercase sha256>  hydra-setup.exe`).
- Install folder `%LOCALAPPDATA%\Programs\hydra`: `hydraui.exe`, `hydra.exe`, `hydra-setup.exe` (the uninstaller). Per user, no admin prompt.
- Downloads come from `https://github.com/davojc/hydra/releases/download/v<version>/<asset>` (`HYDRA_SETUP_BASE` overrides the base, for tests), checked against `<asset>.sha256`. `--from <dir>` reads `<dir>/<asset>` and `<dir>/<asset>.sha256` instead. A `-dev` build installs only `--from`.
- User `PATH` (`HKCU\Environment\Path`, `REG_EXPAND_SZ`) gets the folder once (case-insensitive, trailing `\` ignored); uninstall removes exactly that entry; an emptied `PATH` value is deleted, not left as `""`. After a change, broadcast `WM_SETTINGCHANGE` with `"Environment"`.
- Settings entry: `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra` with `DisplayName`=`hydra`, `DisplayVersion`, `Publisher`=`davojc`, `DisplayIcon`=`<dir>\hydraui.exe`, `InstallLocation`=`<dir>`, `UninstallString`=`"<dir>\hydra-setup.exe" --uninstall`, `QuietUninstallString`=`"<dir>\hydra-setup.exe" --uninstall --silent`, `NoModify`=1, `NoRepair`=1, `EstimatedSize` (KB, DWORD).
- Start menu shortcut `%APPDATA%\Microsoft\Windows\Start Menu\Programs\hydraui.lnk`; desktop shortcut `%USERPROFILE%\Desktop\hydraui.lnk` only when ticked.
- `hydra init` runs hidden (the installed `hydra.exe`) only when `<HYDRA_HOME or %USERPROFILE%\.hydra>\config.toml` is missing.
- Uninstall removes the three exes, `*.old.exe`, `*.new.exe`, both shortcuts, the `PATH` entry, the Settings entry, then the folder if empty; `~/.hydra` stays.
- Flags: `--silent`, `--uninstall`, `--dir <folder>`, `--from <folder>`, `--desktop` (silent installs only; the dialog has a tick box).
- Unit tests never touch the real registry, `PATH`, Start menu, `~/.hydra` or the network (fake `System`, temp folders, local fake GitHub). The system test script points `HYDRA_HOME` at a temp folder, installs into a temp `--dir`, and restores the user `PATH` it found.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`. Gate before each commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (its own exit status), `cargo test --workspace`.

## Review Focus

1. Installing twice (an upgrade) must not add the folder to `PATH` twice. Pinned in Task 2 (`installing_twice_adds_the_folder_once`) and Task 5 (system test installs twice).
2. Uninstall must leave every other `PATH` entry untouched, including one differing only by case or trailing `\`. Pinned in Task 2 (`removing_keeps_every_other_entry`).
3. An existing `config.toml` must not be overwritten by `hydra init`. Pinned in Task 3 (`init_runs_only_without_a_config`).
4. A checksum mismatch or a missing asset must leave nothing installed and no `PATH`/Settings change. Pinned in Task 3 (`a_bad_checksum_installs_nothing`).
5. An upgrade while hydraui is running from the install folder must still succeed (running exes are renamed aside). Pinned in Task 3 (`an_upgrade_replaces_existing_exes`, which holds a file open on the target) and the swap logic already tested in `hydra-update`.

---

### Task 1: hydra-update downloads with progress, or from local files

**Files:** Modify `crates/hydra-update/src/lib.rs`. Test: same file.

**Interfaces:**
- Produces: `pub fn download_verified_with(build: &WindowsBuild, new: &Path, user_agent: &str, progress: &dyn Fn(u64, Option<u64>)) -> anyhow::Result<()>` (`progress(bytes_so_far, total_if_known)`; `download_verified` calls it with a no-op); `pub fn copy_verified(src_dir: &Path, asset: &str, new: &Path) -> anyhow::Result<()>` (copies `<src_dir>/<asset>` to `new` after checking it against `<src_dir>/<asset>.sha256`; on mismatch deletes `new` and errors `checksum mismatch - nothing changed`); `pub fn sha256_file(path: &Path) -> anyhow::Result<String>`.

- [ ] **Step 1: Failing tests**

```rust
    #[test]
    fn local_files_are_checked_against_their_checksum() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.exe"), b"bytes").unwrap();
        let sum = sha256_file(&d.path().join("a.exe")).unwrap();
        std::fs::write(d.path().join("a.exe.sha256"), format!("{sum}  a.exe\n")).unwrap();
        let out = d.path().join("out.new.exe");
        copy_verified(d.path(), "a.exe", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"bytes");
        std::fs::write(d.path().join("a.exe.sha256"), format!("{}  a.exe\n", "0".repeat(64))).unwrap();
        let err = copy_verified(d.path(), "a.exe", &out).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(!out.exists(), "a bad copy is removed");
    }

    #[test]
    fn downloads_report_their_progress() {
        let body = vec![7u8; 200_000];
        let gh = fake::FakeGitHub::serve("v1.0.0", &[("a.exe", &body)]);
        let rel = fetch_latest(&gh.latest_url(), "t").unwrap();
        let build = rel.windows_build("a.exe").unwrap();
        let d = tempfile::tempdir().unwrap();
        let seen = std::cell::RefCell::new(Vec::new());
        download_verified_with(&build, &d.path().join("a.new.exe"), "t", &|n, total| {
            seen.borrow_mut().push((n, total))
        })
        .unwrap();
        let seen = seen.into_inner();
        assert_eq!(seen.last(), Some(&(200_000, Some(200_000))));
        assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0), "never goes backwards");
    }
```

- [ ] **Step 2:** `cargo test -p hydra-update` → compile errors (functions missing).

- [ ] **Step 3: Implement.** `download_verified_with` is today's `download_verified` with `let total = resp.header("Content-Length").and_then(|v| v.parse().ok());` before reading, a running `done += n as u64; progress(done, total);` per chunk; `download_verified` becomes `download_verified_with(build, new, ua, &|_, _| {})`. `sha256_file` streams the file through `Sha256` (64 KiB buffer) to lowercase hex. `copy_verified`:

```rust
pub fn copy_verified(src_dir: &Path, asset: &str, new: &Path) -> anyhow::Result<()> {
    let src = src_dir.join(asset);
    let sums = fs::read_to_string(src_dir.join(format!("{asset}.sha256")))
        .with_context(|| format!("can't read {asset}.sha256 in {}", src_dir.display()))?;
    let expected =
        release::parse_checksum(&sums).context("the release's checksum file can't be read")?;
    fs::copy(&src, new).with_context(|| format!("can't copy {}", src.display()))?;
    if sha256_file(new)? != expected {
        let _ = fs::remove_file(new);
        anyhow::bail!("checksum mismatch - nothing changed");
    }
    Ok(())
}
```

- [ ] **Step 4:** `cargo test -p hydra-update` → pass. **Step 5:** gate; commit `feat: hydra-update reports download progress and checks local files`.

---

### Task 2: hydra-setup's PATH editing and System seam

**Files:** Create `crates/hydra-setup/Cargo.toml`, `src/main.rs` (stub `fn main() {}` for now), `src/lib.rs`, `src/paths.rs`, `src/system.rs`. Modify root `Cargo.toml` (`windows = { version = "0.61", features = [...] }` under `[workspace.dependencies]`, `hydra-setup` member is automatic via `crates/*`).

**Interfaces:**
- `paths::add_entry(path: &str, dir: &str) -> Option<String>` (`None` when already there); `paths::remove_entry(path: &str, dir: &str) -> String`; `paths::has_entry(path: &str, dir: &str) -> bool`; `paths::entries(path: &str) -> Vec<&str>`.
- `system::System` trait: `fn user_path(&self) -> Option<String>; fn set_user_path(&self, value: Option<&str>) -> anyhow::Result<()>; fn environment_changed(&self); fn set_settings_entry(&self, entry: &SettingsEntry) -> anyhow::Result<()>; fn remove_settings_entry(&self); fn settings_entry(&self) -> Option<SettingsEntry>; fn create_shortcut(&self, lnk: &Path, target: &Path) -> anyhow::Result<()>; fn run_hidden(&self, exe: &Path, args: &[&str]) -> anyhow::Result<()>;` plus `struct SettingsEntry { version: String, install_location: PathBuf, size_kb: u32 }`. `system::Real` (Windows), `system::Fake` (`#[cfg(test)]`-friendly: `pub` but in-memory, behind `RefCell`s, recording shortcuts and runs).

- [ ] **Step 1: Failing tests** (`paths.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    const DIR: &str = r"C:\Users\u\AppData\Local\Programs\hydra";

    #[test]
    fn installing_twice_adds_the_folder_once() {
        let once = add_entry(r"C:\a;C:\b", DIR).unwrap();
        assert_eq!(once, format!(r"C:\a;C:\b;{DIR}"));
        assert_eq!(add_entry(&once, DIR), None);
        assert_eq!(add_entry(&once, &format!(r"{}\", DIR.to_uppercase())), None, "case and slash");
        assert_eq!(add_entry("", DIR).unwrap(), DIR);
    }

    #[test]
    fn removing_keeps_every_other_entry() {
        let path = format!(r"C:\a;{DIR}\;C:\Users\u\AppData\Local\Programs\hydra-old;;C:\b");
        assert_eq!(remove_entry(&path, DIR), r"C:\a;C:\Users\u\AppData\Local\Programs\hydra-old;C:\b");
        assert_eq!(remove_entry(DIR, DIR), "");
        assert!(has_entry(&path, &DIR.to_lowercase()));
    }
}
```

- [ ] **Step 2:** `cargo test -p hydra-setup` → compile errors.

- [ ] **Step 3: Implement** `paths.rs`:

```rust
//! The user PATH as text: entries split on `;`, compared ignoring case and a trailing `\`.
fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

pub fn entries(path: &str) -> Vec<&str> {
    path.split(';').filter(|e| !e.is_empty()).collect()
}

pub fn has_entry(path: &str, dir: &str) -> bool {
    entries(path).into_iter().any(|e| same(e, dir))
}

/// `path` with `dir` appended, or `None` when it's already there.
pub fn add_entry(path: &str, dir: &str) -> Option<String> {
    if has_entry(path, dir) {
        return None;
    }
    let mut out = entries(path);
    out.push(dir);
    Some(out.join(";"))
}

/// `path` without `dir` (and without empty entries).
pub fn remove_entry(path: &str, dir: &str) -> String {
    entries(path).into_iter().filter(|e| !same(e, dir)).collect::<Vec<_>>().join(";")
}
```

and `system.rs` with the trait, `SettingsEntry`, `Fake` (fields: `path: RefCell<Option<String>>`, `entry: RefCell<Option<SettingsEntry>>`, `shortcuts: RefCell<Vec<(PathBuf, PathBuf)>>`, `runs: RefCell<Vec<(PathBuf, Vec<String>)>>`, `changed: Cell<u32>`; `create_shortcut` also writes an empty file at `lnk` so file-system checks see it), and `Real`:

- `user_path` / `set_user_path`: `RegGetValueW(HKEY_CURRENT_USER, w!("Environment"), w!("Path"), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND, …)`; set with `RegSetKeyValueW(…, REG_EXPAND_SZ, …)`, `None` → `RegDeleteKeyValueW`.
- `environment_changed`: `SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(w!("Environment").as_ptr() as isize), SMTO_ABORTIFHUNG, 2000, None)`.
- Settings entry: `RegCreateKeyExW` on `Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra`, values per Global Constraints; `remove` → `RegDeleteTreeW`; `settings_entry` reads `DisplayVersion` and `InstallLocation`.
- `create_shortcut`: `CoInitializeEx(None, COINIT_APARTMENTTHREADED)`, `let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?; link.SetPath(&HSTRING::from(target))?; link.SetWorkingDirectory(parent)?; link.cast::<IPersistFile>()?.Save(&HSTRING::from(lnk), true)?`.
- `run_hidden`: `std::process::Command` with `CREATE_NO_WINDOW` (`0x0800_0000`), waits, errors on non-zero exit.

- [ ] **Step 4:** `cargo test -p hydra-setup` → pass. **Step 5:** gate; commit `feat: hydra-setup's PATH editing and system seam`.

---

### Task 3: Install and uninstall (no UI)

**Files:** Create `crates/hydra-setup/src/install.rs`. Test: same file.

**Interfaces:**
- `pub enum Source { Download { base: String }, From(PathBuf) }`; `pub struct Plan { pub dir: PathBuf, pub version: String, pub source: Source, pub desktop: bool, pub start_menu: PathBuf, pub desktop_dir: PathBuf, pub hydra_home: PathBuf, pub setup_exe: PathBuf }`; `pub struct Report { pub other_hydra: Option<PathBuf> }`; `pub fn install(plan: &Plan, sys: &dyn System, progress: &dyn Fn(Step)) -> anyhow::Result<Report>`; `pub enum Step { Downloading { asset: &'static str, done: u64, total: Option<u64> }, Installing }`; `pub fn uninstall(dir: &Path, start_menu: &Path, desktop_dir: &Path, sys: &dyn System) -> anyhow::Result<()>`; `pub fn default_plan(version: &str, source: Source, desktop: bool) -> Plan` (real folders from `%LOCALAPPDATA%`, `%APPDATA%`, `%USERPROFILE%`, `HYDRA_HOME`).

- [ ] **Step 1: Failing tests** (each builds a `Plan` into a temp dir with `Source::From(src)` where `src` holds fake `hydraui-x86_64-pc-windows-msvc.exe`, `hydra-x86_64-pc-windows-msvc.exe` and correct `.sha256`s, and `setup_exe` pointing at a fake file):

```rust
    #[test]
    fn installs_files_path_shortcut_and_settings_entry() { /* install → dir has hydraui.exe, hydra.exe, hydra-setup.exe with the fake bytes; sys.path has dir once; sys.changed == 1; Start menu .lnk recorded, no desktop .lnk; settings entry version == plan.version and install_location == dir */ }

    #[test]
    fn installing_twice_adds_the_folder_once() { /* install, install again → path has dir once; files are the second copy's bytes */ }

    #[test]
    fn an_upgrade_replaces_existing_exes() { /* pre-create dir/hydraui.exe with "old" and keep a std::fs::File handle open on it (Windows allows rename of an open file opened without share-delete? use OpenOptions with share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE) via std::os::windows::fs::OpenOptionsExt) → install succeeds; dir/hydraui.exe has the new bytes */ }

    #[test]
    fn a_bad_checksum_installs_nothing() { /* corrupt hydra's .sha256 → Err containing "checksum"; dir has no exes; sys.path None; sys.entry None; no shortcuts */ }

    #[test]
    fn init_runs_only_without_a_config() { /* hydra_home empty → sys.runs has (dir/hydra.exe, ["init"]); with hydra_home/config.toml present → no run */ }

    #[test]
    fn another_hydra_on_path_is_reported() { /* set PATH env for the test via plan field `search_path: Option<OsString>` (add it to Plan) containing a temp dir with hydra.exe → report.other_hydra == that dir */ }

    #[test]
    fn uninstall_removes_what_install_added() { /* install with desktop, then uninstall → exes gone, *.old.exe / *.new.exe planted beforehand gone, .lnk files gone, path None (it held only dir) or the other entries, entry None */ }
```

Write each body out in full in the test (the comments above say exactly what each asserts); add `pub search_path: Option<std::ffi::OsString>` to `Plan` (`default_plan` fills it from `std::env::var_os("PATH")`).

- [ ] **Step 2:** `cargo test -p hydra-setup` → compile errors.

- [ ] **Step 3: Implement** `install`:
  1. `fs::create_dir_all(&plan.dir)`.
  2. For each of `(HYDRAUI_ASSET, "hydraui.exe")`, `(HYDRA_ASSET, "hydra.exe")`: fetch into `<dir>/<stem>.new.exe` — `Source::From(src)` → `hydra_update::copy_verified(src, asset, &new)`; `Source::Download { base }` → `WindowsBuild { exe_url: format!("{base}/v{version}/{asset}"), sha256_url: format!("{base}/v{version}/{asset}.sha256") }` and `download_verified_with(…, &|done, total| progress(Step::Downloading { asset, done, total }))`. On any error delete every `.new.exe` made so far and return it (nothing else has changed).
  3. Copy `plan.setup_exe` to `<dir>/hydra-setup.new.exe` (skip when it already *is* `<dir>/hydra-setup.exe`).
  4. `progress(Step::Installing)`; swap each `.new.exe` into place: when the target exists use `hydra_update::replace_all` over all of them; when it doesn't, `fs::rename(new, target)`. (Write a helper `place_all(&[Swap])` that does a rename for missing targets and `replace_all` for the rest; on error, remove placed new files and return the error.)
  5. `sys.create_shortcut(start_menu/hydraui.lnk, dir/hydraui.exe)`; if `desktop`, also `desktop_dir/hydraui.lnk`.
  6. PATH: `if let Some(p) = paths::add_entry(&sys.user_path().unwrap_or_default(), dir) { sys.set_user_path(Some(&p))?; sys.environment_changed(); }`.
  7. Settings entry with `size_kb` = sum of the three exes' sizes / 1024.
  8. `hydra init` via `sys.run_hidden(dir/hydra.exe, &["init"])` when `!hydra_home.join("config.toml").exists()`; an init failure is logged into the report (`Report { init_failed: Option<String>, .. }` — add the field) but doesn't fail the install.
  9. `other_hydra`: the first `search_path` entry (split with `std::env::split_paths`) that isn't `dir` (compare like `paths::same`) and holds `hydra.exe`.

  `uninstall`: remove `hydraui.exe`, `hydra.exe`, every `*.old.exe` / `*.new.exe` in `dir` (ignore missing); both `.lnk` files; PATH entry (`remove_entry`; empty → `set_user_path(None)`), `environment_changed`; `remove_settings_entry`. Leave `hydra-setup.exe` (the running uninstaller deletes itself in Task 4).

- [ ] **Step 4:** `cargo test -p hydra-setup` → pass. **Step 5:** gate; commit `feat: hydra-setup installs and uninstalls (no UI yet)`.

---

### Task 4: The dialog and the exe

**Files:** Create `crates/hydra-setup/src/dialog.rs`, `crates/hydra-setup/src/args.rs`, `crates/hydra-setup/build.rs`. Modify `src/main.rs`, `Cargo.toml` (`[build-dependencies] winresource = "0.1"`).

**Interfaces:**
- `args::parse(args: &[String]) -> Result<Args, String>` with `Args { silent: bool, uninstall: bool, dir: Option<PathBuf>, from: Option<PathBuf>, desktop: bool }`; unknown flags → `Err("unknown option --x")`.
- `pub const VERSION: &str` (as hydra's: `HYDRA_VERSION` else `<cargo>-dev`).

- [ ] **Step 1: Failing test** (`args.rs`)

```rust
    #[test]
    fn flags_are_read() {
        let a = |v: &[&str]| parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let got = a(&["--silent", "--dir", r"C:\t", "--from", "dist", "--desktop"]).unwrap();
        assert!(got.silent && got.desktop && !got.uninstall);
        assert_eq!(got.dir.as_deref(), Some(std::path::Path::new(r"C:\t")));
        assert!(a(&["--uninstall", "--silent"]).unwrap().uninstall);
        assert!(a(&["--dir"]).is_err(), "a flag missing its value");
        assert!(a(&["--nope"]).is_err());
    }
```

- [ ] **Step 2:** fail; **Step 3:** implement `parse` (a simple loop over the args).

- [ ] **Step 4: The dialog** (`dialog.rs`), with `windows::Win32::UI::Controls::{TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDF_*, TDN_*, TDM_*}`:
  - `confirm_install(version, dir) -> Option<bool /*desktop*/>`: title "hydra setup", main instruction `Install hydra <version>`, content "hydraui and hydra, for you only, in <dir>. hydraui goes in the Start menu and hydra on your PATH.", custom buttons Install (id 100) / Cancel, verification text "Create a desktop shortcut".
  - `run_with_progress(work: impl FnOnce(&dyn Fn(Step)) -> anyhow::Result<Report> + Send)`: a dialog with `TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER`, no buttons enabled; the work runs on a thread updating an `Arc<Mutex<(u32 /*0-1000*/, String, Option<Result<Report, String>>)>>`; the callback (`TDN_TIMER`) sets the bar (`TDM_SET_PROGRESS_BAR_POS`, range 0–1000 via `TDM_SET_PROGRESS_BAR_RANGE` on `TDN_CREATED`), the text (`TDM_UPDATE_ELEMENT_TEXT`), and when the result arrives sends `TDM_CLICK_BUTTON` to close; returns the result. Total progress: hydraui is ~70% of the bytes, hydra ~30% — use `done/total` per asset weighted by each asset's `total` once known.
  - `done(report) -> bool /*launch*/`: "Installed" + "Open a new terminal to use `hydra`." (+ "Another hydra.exe is on your PATH, in <dir>; Windows may run that one. This setup left it alone." when `other_hydra`), verification "Launch hydraui" (checked: `TDF_VERIFICATION_FLAG_CHECKED`), Close.
  - `failed(message)`: error icon, "hydra wasn't installed", the message.
  - `confirm_uninstall() -> bool`, `uninstalled()` ("hydra was removed. Your environments in ~/.hydra are still there.").
- [ ] **Step 5: `main.rs`** (`#![windows_subsystem = "windows"]`): parse args (error → `failed` unless silent, exit 2); `--uninstall`: confirm (unless silent) → `install::uninstall` with the real folders → message (unless silent) → self-delete: spawn hidden `cmd /c ping -n 3 127.0.0.1 >nul & del /f /q "<exe>" & rmdir "<dir>"` and exit. Install: `-dev` without `--from` → error "this is a development build; use --from <folder>"; source = `--from` or `Download { base: HYDRA_SETUP_BASE or "https://github.com/davojc/hydra/releases/download" }`; silent → `install::install` and exit 0/1; else confirm → progress → done (launch hydraui if ticked) / failed.
- [ ] **Step 6: `build.rs`**: like hydraui's (icon `../../assets/hydra.ico`, FileDescription "hydra setup", ProductName "hydra", OriginalFilename "hydra-setup.exe", version from `HYDRA_VERSION`), plus `res.set_manifest(MANIFEST)` with a manifest declaring `Microsoft.Windows.Common-Controls` 6.0.0.0 (TaskDialog needs comctl32 v6), `asInvoker` execution level, and per-monitor DPI awareness.
- [ ] **Step 7: Smoke by hand** — build a release `hydra-setup.exe` and the two exes with `HYDRA_VERSION=0.6.9` into `dist/` (with `.sha256`s), then in the scratch folder run `hydra-setup.exe --from dist --dir <scratch>\inst` with `HYDRA_HOME=<scratch>\home`: screenshot each dialog page; check the folder, then uninstall with `<scratch>\inst\hydra-setup.exe --uninstall` and check it's gone. (The harness can click the dialog's buttons; restore the user PATH afterwards with the value saved before.)
- [ ] **Step 8:** gate; commit `feat: hydra-setup's dialog and exe`.

---

### Task 5: System test, CI, and hydraui's installed version

**Files:** Create `scripts/test-setup.ps1`. Modify `.github/workflows/ci.yml`, `crates/hydraui/src/updater.rs`, `crates/hydraui/src/workspace/title_bar.rs`.

- [ ] **Step 1: The system test** (`scripts/test-setup.ps1 -Setup <exe> -From <dist> -Version <x.y.z>`): saves the user `PATH`; sets `HYDRA_HOME` to a temp folder; plants a neighbour `PATH` entry; runs `<Setup> --silent --from <dist> --dir <temp>\app` twice (waiting for exit, checking exit code 0); checks `hydraui.exe`, `hydra.exe`, `hydra-setup.exe` exist, `hydra --version` == `hydra <Version>`, the folder is on the user `PATH` once, `HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra` has `DisplayVersion` == Version and `InstallLocation` == the folder, the Start menu `hydraui.lnk` exists, `config.toml` was created; then `<temp>\app\hydra-setup.exe --uninstall --silent`, waits ~5 s for the self-delete, and checks the exes, the `.lnk`, the `PATH` entry and the registry key are gone and the neighbour entry remains; finally restores the saved `PATH` and deletes the temp folder. Exits 1 on any failed check (print `ok`/`FAIL` per check, as a list). **Note:** a real install's Start menu shortcut lands in the real Start menu; the script removes it via the uninstall and asserts it's gone.
- [ ] **Step 2: Run it locally** against the Task 4 build: all `ok`.
- [ ] **Step 3: CI** — in the release job: `cargo build --release -p hydra-cli -p hydraui -p hydra-setup` (one build step); in Package also copy `target/release/hydra-setup.exe` to `dist/hydra-setup.exe` and write its `.sha256` like the others; add a step after "Check hydraui's version":

```yaml
      - name: Test the installer
        if: steps.decide.outputs.release == 'true'
        shell: pwsh
        env:
          VERSION: ${{ steps.decide.outputs.version }}
        run: ./scripts/test-setup.ps1 -Setup dist/hydra-setup.exe -From dist -Version $env:VERSION
```

(`gh release create … dist/*` already uploads everything in `dist`.)
- [ ] **Step 4: hydraui's installed version.** In `updater.rs`: `pub const SETTINGS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra";`, `pub fn installed_here(install_location: Option<&str>, exe: &Path) -> bool` (case/slash-insensitive comparison of `install_location` with `exe`'s folder; `None` → false), and `pub fn record_installed_version(exe: &Path, version: &Version)` (reads `InstallLocation` with `reg query SETTINGS_KEY /v InstallLocation`, writes `DisplayVersion` with `reg add … /f` when `installed_here`; hidden windows; failures ignored). Test first:

```rust
    #[test]
    fn only_the_installed_copy_records_its_version() {
        let exe = Path::new(r"C:\Users\u\AppData\Local\Programs\hydra\hydraui.exe");
        assert!(installed_here(Some(r"C:\Users\u\AppData\Local\Programs\hydra\"), exe));
        assert!(installed_here(Some(r"c:\users\U\appdata\local\programs\HYDRA"), exe));
        assert!(!installed_here(Some(r"C:\Users\u\AppData\Local\Programs\hydra"), Path::new(r"C:\Users\u\Downloads\hydraui.exe")));
        assert!(!installed_here(None, exe));
    }
```

  In `title_bar.rs`'s background install closure, after `Ok(done)`, call `updater::record_installed_version(&exe, &done.version)`.
- [ ] **Step 5:** gate; commit `ci: release builds and tests hydra-setup; hydraui records its installed version`.

---

### Task 6: Website and README offer the installer first

**Files:** Modify `site/index.html` (Install), `site/app.html` (hero download, `#start`), `site/cli.html` (hero download, `#install`), `site/style.css` (`.portable`), `README.md`.

- [ ] **Step 1:** Landing Install: main button `Download the installer` (small `hydra + hydraui`) → `releases/latest/download/hydra-setup.exe`; under it `<p class="portable">Portable: <a …hydraui-x86_64-pc-windows-msvc.exe>hydraui.exe</a> · <a …hydra-x86_64-pc-windows-msvc.exe>hydra.exe</a></p>`; list: **The installer** (both apps in `%LOCALAPPDATA%\Programs\hydra` for you alone, no admin prompt: hydraui in the Start menu, hydra on your `PATH`, `hydra init` done; uninstall from Settings → Apps; `~/.hydra` stays), **Portable** (rename and run from anywhere; put `hydra.exe` on `PATH` and run `hydra init` yourself), **Updates** (title bar button updates both; `hydra update` from the command line). CSS: `.portable { margin: 0 0 14px; font-size: 15px; color: var(--muted); }`.
- [ ] **Step 2:** App guide: hero button → installer + portable line; `#start` first paragraph: run the installer (both apps, Start menu, `PATH`), or the portable `hydraui-x86_64-pc-windows-msvc.exe` which runs from anywhere and doesn't need `hydra.exe`; same `~/.hydra` either way.
- [ ] **Step 3:** CLI guide: hero and `#install` button → installer + portable line; the paragraph after it: the installer puts `hydra.exe` (and hydraui) in `%LOCALAPPDATA%\Programs\hydra`, adds it to `PATH` and runs `hydra init` — open a new terminal and `hydra` is there; for the portable `hydra-x86_64-pc-windows-msvc.exe`, rename it, put it on `PATH`, then `hydra init` (the existing transcript stays).
- [ ] **Step 4:** README install section: installer first, then portable exes, then building from source.
- [ ] **Step 5:** Link check (every local `href`/`src`/anchor resolves) and headless Edge renders of `index.html` and `app.html` (desktop, and a 375 px iframe) to look at the Install sections.
- [ ] **Step 6:** commit `docs(site): installer first, portable exes beside it`.
