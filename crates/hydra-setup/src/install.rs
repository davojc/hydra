//! Installing and uninstalling hydraui and hydra (spec §7.7), with no UI: the dialog and the
//! silent mode both drive these.
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use hydra_update::release::WindowsBuild;
use hydra_update::{HYDRA_ASSET, HYDRAUI_ASSET, Swap, copy_verified, download_verified_with};

use crate::paths;
use crate::system::{SettingsEntry, System};

/// Where the two exes come from.
#[derive(Debug, Clone)]
pub enum Source {
    /// `<base>/v<version>/<asset>`, a release's download folder.
    Download { base: String },
    /// A folder holding the assets and their `.sha256` files.
    From(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub dir: PathBuf,
    pub version: String,
    pub source: Source,
    pub desktop: bool,
    /// The Start menu's Programs folder, and the desktop.
    pub start_menu: PathBuf,
    pub desktop_dir: PathBuf,
    /// Where `hydra init` writes `config.toml` (HYDRA_HOME, else `~/.hydra`).
    pub hydra_home: PathBuf,
    /// This setup exe, copied in as the uninstaller.
    pub setup_exe: PathBuf,
    /// The PATH to look through for another `hydra.exe`.
    pub search_path: Option<OsString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The folder of another `hydra.exe` on PATH, left alone.
    pub other_hydra: Option<PathBuf>,
    /// Why `hydra init` failed, when it did (the install still stands).
    pub init_failed: Option<String>,
}

/// How far an install has got, for the progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Downloading {
        asset: &'static str,
        done: u64,
        total: Option<u64>,
    },
    Installing,
}

/// The two exes: release asset and installed name.
const EXES: [(&str, &str); 2] = [(HYDRAUI_ASSET, "hydraui.exe"), (HYDRA_ASSET, "hydra.exe")];

/// The real folders: `%LOCALAPPDATA%\Programs\hydra`, the Start menu, the desktop, HYDRA_HOME.
pub fn default_plan(version: &str, source: Source, desktop: bool) -> Plan {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from).unwrap_or_default();
    let profile = env("USERPROFILE");
    Plan {
        dir: env("LOCALAPPDATA").join("Programs").join("hydra"),
        version: version.to_string(),
        source,
        desktop,
        start_menu: env("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs"),
        desktop_dir: profile.join("Desktop"),
        hydra_home: std::env::var_os("HYDRA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| profile.join(".hydra")),
        setup_exe: std::env::current_exe().unwrap_or_default(),
        search_path: std::env::var_os("PATH"),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    paths::same(&a.to_string_lossy(), &b.to_string_lossy())
}

fn new_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(name.replace(".exe", ".new.exe"))
}

fn swap(dir: &Path, name: &str) -> Swap {
    Swap {
        target: dir.join(name),
        new: new_of(dir, name),
        old: dir.join(name.replace(".exe", ".old.exe")),
    }
}

/// Puts each `.new.exe` in place: running or existing exes are swapped (renamed aside), new
/// ones renamed in. On failure every `.new.exe` left is removed.
fn place_all(swaps: &[Swap]) -> anyhow::Result<()> {
    let (existing, fresh): (Vec<Swap>, Vec<Swap>) =
        swaps.iter().cloned().partition(|s| s.target.exists());
    let clean = |list: &[Swap]| {
        for s in list {
            let _ = fs::remove_file(&s.new);
        }
    };
    if let Err(e) = hydra_update::replace_all(&existing) {
        clean(&fresh);
        return Err(e);
    }
    for (i, s) in fresh.iter().enumerate() {
        if let Err(e) = fs::rename(&s.new, &s.target) {
            clean(&fresh[i..]);
            return Err(e).with_context(|| format!("can't install {}", s.target.display()));
        }
    }
    Ok(())
}

/// Installs (or upgrades) per `plan`. Nothing outside the folder changes until both exes are
/// fetched and checked.
pub fn install(plan: &Plan, sys: &dyn System, progress: &dyn Fn(Step)) -> anyhow::Result<Report> {
    let dir = &plan.dir;
    // Read first: a PATH that can't be read must stop the install before anything changes,
    // never be taken for an empty one and overwritten.
    let current_path = sys
        .user_path()
        .context("can't read your PATH - nothing changed")?;
    fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let mut swaps = Vec::new();
    for (asset, name) in EXES {
        let new = new_of(dir, name);
        let got = match &plan.source {
            Source::From(src) => copy_verified(src, asset, &new),
            Source::Download { base } => {
                let build = WindowsBuild {
                    exe_url: format!("{base}/v{}/{asset}", plan.version),
                    sha256_url: format!("{base}/v{}/{asset}.sha256", plan.version),
                };
                let ua = format!("hydra-setup/{}", plan.version);
                download_verified_with(&build, &new, &ua, &|done, total| {
                    progress(Step::Downloading { asset, done, total })
                })
            }
        };
        if let Err(e) = got {
            let _ = fs::remove_file(&new);
            for s in &swaps {
                let s: &Swap = s;
                let _ = fs::remove_file(&s.new);
            }
            return Err(e);
        }
        swaps.push(swap(dir, name));
    }
    let setup_target = dir.join("hydra-setup.exe");
    if !same_dir(&plan.setup_exe, &setup_target) {
        let new = new_of(dir, "hydra-setup.exe");
        if let Err(e) = fs::copy(&plan.setup_exe, &new) {
            for s in &swaps {
                let _ = fs::remove_file(&s.new);
            }
            return Err(e).context("can't copy the uninstaller");
        }
        swaps.push(swap(dir, "hydra-setup.exe"));
    }
    progress(Step::Installing);
    place_all(&swaps)?;

    sys.create_shortcut(
        &plan.start_menu.join("hydraui.lnk"),
        &dir.join("hydraui.exe"),
    )?;
    if plan.desktop {
        sys.create_shortcut(
            &plan.desktop_dir.join("hydraui.lnk"),
            &dir.join("hydraui.exe"),
        )?;
    }
    let dir_text = dir.display().to_string();
    if let Some(path) = paths::add_entry(current_path.as_deref().unwrap_or(""), &dir_text) {
        sys.set_user_path(Some(&path))?;
        sys.environment_changed();
    }
    let size: u64 = ["hydraui.exe", "hydra.exe", "hydra-setup.exe"]
        .iter()
        .filter_map(|f| fs::metadata(dir.join(f)).ok())
        .map(|m| m.len())
        .sum();
    sys.set_settings_entry(&SettingsEntry {
        version: plan.version.clone(),
        install_location: dir.clone(),
        size_kb: (size / 1024) as u32,
    })?;

    let mut report = Report::default();
    if !plan.hydra_home.join("config.toml").exists()
        && let Err(e) = sys.run_hidden(&dir.join("hydra.exe"), &["init"])
    {
        report.init_failed = Some(format!("{e:#}"));
    }
    report.other_hydra = plan.search_path.as_ref().and_then(|p| {
        std::env::split_paths(p).find(|d| !same_dir(d, dir) && d.join("hydra.exe").is_file())
    });
    Ok(report)
}

/// The folder an uninstall may empty: `--dir` when given, else this exe's folder, but only when
/// it's the installed one (the Settings entry's `InstallLocation`). A copy run from anywhere
/// else (Downloads) must not delete files next to it.
pub fn uninstall_target(
    dir: Option<&Path>,
    exe_dir: &Path,
    entry: Option<&SettingsEntry>,
) -> Result<PathBuf, String> {
    if let Some(d) = dir {
        return Ok(d.to_path_buf());
    }
    match entry {
        Some(e) if same_dir(&e.install_location, exe_dir) => Ok(exe_dir.to_path_buf()),
        Some(e) => Err(format!(
            "This hydra-setup.exe isn't the installed one: hydra is installed in {}. Uninstall it from Settings → Apps.",
            e.install_location.display()
        )),
        None => Err("hydra doesn't look installed (it has no entry in Settings → Apps), so nothing was removed.".into()),
    }
}

/// Whether `exe` is running (a running exe can't be opened for writing).
fn in_use(exe: &Path) -> bool {
    exe.exists() && fs::OpenOptions::new().write(true).open(exe).is_err()
}

/// Removes what `install` added, except `hydra-setup.exe` itself (the running uninstaller
/// deletes it as it exits) and `~/.hydra`.
pub fn uninstall(
    dir: &Path,
    start_menu: &Path,
    desktop_dir: &Path,
    sys: &dyn System,
) -> anyhow::Result<()> {
    let running: Vec<&str> = ["hydraui.exe", "hydra.exe"]
        .into_iter()
        .filter(|n| in_use(&dir.join(n)))
        .collect();
    anyhow::ensure!(
        running.is_empty(),
        "{} is running - close it and try again. Nothing was removed.",
        running.join(" and ")
    );
    let current_path = sys
        .user_path()
        .context("can't read your PATH - nothing was removed")?;
    for name in ["hydraui.exe", "hydra.exe"] {
        let _ = fs::remove_file(dir.join(name));
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for f in entries.flatten() {
            let n = f.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".old.exe") || n.ends_with(".new.exe") {
                let _ = fs::remove_file(f.path());
            }
        }
    }
    let _ = fs::remove_file(start_menu.join("hydraui.lnk"));
    let _ = fs::remove_file(desktop_dir.join("hydraui.lnk"));
    let dir_text = dir.display().to_string();
    if let Some(path) = current_path
        && paths::has_entry(&path, &dir_text)
    {
        let rest = paths::remove_entry(&path, &dir_text);
        sys.set_user_path((!rest.is_empty()).then_some(rest.as_str()))?;
        sys.environment_changed();
    }
    // Only this folder's entry: another install's stays.
    if sys
        .settings_entry()
        .is_some_and(|e| same_dir(&e.install_location, dir))
    {
        sys.remove_settings_entry();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::Fake;
    use hydra_update::{HYDRA_ASSET, HYDRAUI_ASSET, sha256_file};

    /// A release's two assets and their checksums, in `dir`.
    fn write_release(dir: &Path, ui: &[u8], cli: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        for (asset, bytes) in [(HYDRAUI_ASSET, ui), (HYDRA_ASSET, cli)] {
            std::fs::write(dir.join(asset), bytes).unwrap();
            let sum = sha256_file(&dir.join(asset)).unwrap();
            std::fs::write(
                dir.join(format!("{asset}.sha256")),
                format!("{sum}  {asset}\n"),
            )
            .unwrap();
        }
    }

    struct Env {
        _d: tempfile::TempDir,
        plan: Plan,
        src: PathBuf,
    }

    fn env() -> Env {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("release");
        write_release(&src, b"ui-1", b"cli-1");
        let setup = d.path().join("download").join("hydra-setup.exe");
        std::fs::create_dir_all(setup.parent().unwrap()).unwrap();
        std::fs::write(&setup, b"setup-1").unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let plan = Plan {
            dir: d.path().join("app"),
            version: "0.7.0".into(),
            source: Source::From(src.clone()),
            desktop: false,
            start_menu: d.path().join("start"),
            desktop_dir: d.path().join("desk"),
            hydra_home: home,
            setup_exe: setup,
            search_path: None,
        };
        Env { _d: d, plan, src }
    }

    fn read(p: PathBuf) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    #[test]
    fn installs_files_path_shortcut_and_settings_entry() {
        let e = env();
        let sys = Fake::default();
        *sys.path.borrow_mut() = Some(r"C:\other".into());
        install(&e.plan, &sys, &|_| {}).unwrap();
        let dir = &e.plan.dir;
        assert_eq!(read(dir.join("hydraui.exe")), b"ui-1");
        assert_eq!(read(dir.join("hydra.exe")), b"cli-1");
        assert_eq!(read(dir.join("hydra-setup.exe")), b"setup-1");
        let path = sys.path.borrow().clone().unwrap();
        assert_eq!(path, format!(r"C:\other;{}", dir.display()));
        assert_eq!(sys.changed.get(), 1);
        let shortcuts = sys.shortcuts.borrow().clone();
        assert_eq!(
            shortcuts,
            vec![(
                e.plan.start_menu.join("hydraui.lnk"),
                dir.join("hydraui.exe")
            )]
        );
        let entry = sys.entry.borrow().clone().unwrap();
        assert_eq!(entry.version, "0.7.0");
        assert_eq!(&entry.install_location, dir);
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".new.exe") || n.ends_with(".old.exe"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn installing_twice_adds_the_folder_once() {
        let e = env();
        let sys = Fake::default();
        install(&e.plan, &sys, &|_| {}).unwrap();
        write_release(&e.src, b"ui-2", b"cli-2");
        install(&e.plan, &sys, &|_| {}).unwrap();
        let path = sys.path.borrow().clone().unwrap();
        assert_eq!(path, e.plan.dir.display().to_string(), "once");
        assert_eq!(read(e.plan.dir.join("hydraui.exe")), b"ui-2");
        assert_eq!(read(e.plan.dir.join("hydra.exe")), b"cli-2");
    }

    #[test]
    fn an_upgrade_replaces_existing_exes() {
        let e = env();
        std::fs::create_dir_all(&e.plan.dir).unwrap();
        std::fs::write(e.plan.dir.join("hydraui.exe"), b"old").unwrap();
        // Held open, as a running hydraui would be (Rust opens files shareable for rename).
        let _held = std::fs::File::open(e.plan.dir.join("hydraui.exe")).unwrap();
        install(&e.plan, &Fake::default(), &|_| {}).unwrap();
        assert_eq!(read(e.plan.dir.join("hydraui.exe")), b"ui-1");
    }

    #[test]
    fn a_bad_checksum_installs_nothing() {
        let e = env();
        std::fs::write(
            e.src.join(format!("{HYDRA_ASSET}.sha256")),
            format!("{}  {HYDRA_ASSET}\n", "0".repeat(64)),
        )
        .unwrap();
        let sys = Fake::default();
        let err = install(&e.plan, &sys, &|_| {}).unwrap_err();
        assert!(format!("{err:#}").contains("checksum"), "{err:#}");
        let files: Vec<_> = std::fs::read_dir(&e.plan.dir)
            .map(|r| r.flatten().map(|f| f.file_name()).collect())
            .unwrap_or_default();
        assert!(files.is_empty(), "{files:?}");
        assert_eq!(*sys.path.borrow(), None);
        assert_eq!(*sys.entry.borrow(), None);
        assert!(sys.shortcuts.borrow().is_empty());
    }

    #[test]
    fn init_runs_only_without_a_config() {
        let e = env();
        let sys = Fake::default();
        install(&e.plan, &sys, &|_| {}).unwrap();
        assert_eq!(
            sys.runs.borrow().clone(),
            vec![(e.plan.dir.join("hydra.exe"), vec!["init".to_string()])]
        );
        std::fs::write(e.plan.hydra_home.join("config.toml"), "# mine").unwrap();
        let again = Fake::default();
        install(&e.plan, &again, &|_| {}).unwrap();
        assert!(again.runs.borrow().is_empty());
        assert_eq!(
            std::fs::read_to_string(e.plan.hydra_home.join("config.toml")).unwrap(),
            "# mine"
        );
    }

    #[test]
    fn another_hydra_on_path_is_reported() {
        let mut e = env();
        let tools = e._d.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(tools.join("hydra.exe"), b"other").unwrap();
        e.plan.search_path = Some(
            std::env::join_paths([e.plan.dir.clone(), e._d.path().join("none"), tools.clone()])
                .unwrap(),
        );
        let report = install(&e.plan, &Fake::default(), &|_| {}).unwrap();
        assert_eq!(report.other_hydra, Some(tools));
        e.plan.search_path = None;
        let report = install(&e.plan, &Fake::default(), &|_| {}).unwrap();
        assert_eq!(report.other_hydra, None);
    }

    #[test]
    fn an_unreadable_path_stops_the_install() {
        let e = env();
        let sys = Fake::default();
        *sys.path.borrow_mut() = Some(r"C:\mine".into());
        sys.path_error.set(true);
        let err = install(&e.plan, &sys, &|_| {}).unwrap_err();
        assert!(format!("{err:#}").contains("PATH"), "{err:#}");
        assert_eq!(
            sys.path.borrow().as_deref(),
            Some(r"C:\mine"),
            "PATH untouched"
        );
        assert_eq!(*sys.entry.borrow(), None);
        assert!(sys.shortcuts.borrow().is_empty());
        assert!(
            !e.plan.dir.join("hydraui.exe").exists(),
            "nothing installed"
        );
    }

    #[test]
    fn the_uninstaller_works_only_from_the_installed_folder() {
        let installed = Path::new(r"C:\Users\u\AppData\Local\Programs\hydra");
        let entry = SettingsEntry {
            version: "0.7.0".into(),
            install_location: installed.to_path_buf(),
            size_kb: 1,
        };
        assert_eq!(
            uninstall_target(None, installed, Some(&entry)).unwrap(),
            installed
        );
        let downloads = Path::new(r"C:\Users\u\Downloads");
        let err = uninstall_target(None, downloads, Some(&entry)).unwrap_err();
        assert!(err.contains(r"Programs\hydra"), "{err}");
        assert!(
            uninstall_target(None, downloads, None).is_err(),
            "not installed at all"
        );
        assert_eq!(
            uninstall_target(Some(downloads), installed, Some(&entry)).unwrap(),
            downloads,
            "--dir says which folder"
        );
    }

    #[test]
    fn uninstall_keeps_another_installs_settings_entry() {
        let e = env();
        let sys = Fake::default();
        install(&e.plan, &sys, &|_| {}).unwrap();
        let elsewhere = e._d.path().join("elsewhere");
        sys.entry.borrow_mut().as_mut().unwrap().install_location = elsewhere;
        uninstall(&e.plan.dir, &e.plan.start_menu, &e.plan.desktop_dir, &sys).unwrap();
        assert!(
            sys.entry.borrow().is_some(),
            "another install's entry is left alone"
        );
    }

    #[test]
    fn uninstall_refuses_while_hydraui_runs() {
        use std::os::windows::fs::OpenOptionsExt;
        let e = env();
        let sys = Fake::default();
        install(&e.plan, &sys, &|_| {}).unwrap();
        // A running exe can't be opened for writing: hold it exclusively to stand in for that.
        let _running = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(e.plan.dir.join("hydraui.exe"))
            .unwrap();
        let err =
            uninstall(&e.plan.dir, &e.plan.start_menu, &e.plan.desktop_dir, &sys).unwrap_err();
        assert!(format!("{err:#}").contains("hydraui"), "{err:#}");
        assert!(e.plan.dir.join("hydra.exe").exists(), "nothing removed");
        assert!(sys.entry.borrow().is_some());
        assert!(sys.path.borrow().is_some());
    }

    #[test]
    fn uninstall_removes_what_install_added() {
        let mut e = env();
        e.plan.desktop = true;
        let sys = Fake::default();
        *sys.path.borrow_mut() = Some(r"C:\other".into());
        install(&e.plan, &sys, &|_| {}).unwrap();
        let dir = e.plan.dir.clone();
        std::fs::write(dir.join("hydraui.old.exe"), b"x").unwrap();
        std::fs::write(dir.join("hydra.new.exe"), b"x").unwrap();
        assert!(e.plan.desktop_dir.join("hydraui.lnk").exists());
        uninstall(&dir, &e.plan.start_menu, &e.plan.desktop_dir, &sys).unwrap();
        for f in [
            "hydraui.exe",
            "hydra.exe",
            "hydraui.old.exe",
            "hydra.new.exe",
        ] {
            assert!(!dir.join(f).exists(), "{f} left behind");
        }
        assert!(!e.plan.start_menu.join("hydraui.lnk").exists());
        assert!(!e.plan.desktop_dir.join("hydraui.lnk").exists());
        assert_eq!(sys.path.borrow().as_deref(), Some(r"C:\other"));
        assert_eq!(*sys.entry.borrow(), None);
        // A PATH that held only our folder is deleted, not left as "".
        let only = Fake::default();
        install(&e.plan, &only, &|_| {}).unwrap();
        uninstall(&dir, &e.plan.start_menu, &e.plan.desktop_dir, &only).unwrap();
        assert_eq!(*only.path.borrow(), None);
    }
}
