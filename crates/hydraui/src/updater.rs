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
    let pids = running().map_err(|why| {
        anyhow::anyhow!("can't check whether hydra is running ({why}) - not updating")
    })?;
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
        assert_eq!(
            check_against(&gh.latest_url(), "0.6.0").unwrap(),
            Some(v("0.7.0"))
        );
        assert_eq!(check_against(&gh.latest_url(), "0.7.0").unwrap(), None);
        assert_eq!(check_against(&gh.latest_url(), "0.6.0-dev").unwrap(), None);
    }

    #[test]
    fn an_unreachable_github_is_an_error_not_a_panic() {
        // Port 9 (discard) on localhost: nothing listens, the connection is refused.
        assert!(
            check_against(
                "http://127.0.0.1:9/repos/davojc/hydra/releases/latest",
                "0.6.0"
            )
            .is_err()
        );
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
        let gh = FakeGitHub::serve(
            "v0.7.0",
            &[(HYDRAUI_ASSET, b"new ui"), (HYDRA_ASSET, b"new cli")],
        );
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
        let gh = FakeGitHub::serve(
            "v0.7.0",
            &[(HYDRAUI_ASSET, b"new ui"), (HYDRA_ASSET, b"new cli")],
        );
        let d = tempfile::tempdir().unwrap();
        let ui = d.path().join("hydraui.exe");
        std::fs::write(&ui, "old ui").unwrap();
        let done = install(&gh.latest_url(), &ui, None, &none_running).unwrap();
        assert!(!done.hydra_updated);
        assert_eq!(std::fs::read(&ui).unwrap(), b"new ui");
    }

    #[test]
    fn a_running_hydra_stops_the_update_and_is_named() {
        let gh = FakeGitHub::serve(
            "v0.7.0",
            &[(HYDRAUI_ASSET, b"new ui"), (HYDRA_ASSET, b"new cli")],
        );
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
        assert_eq!(
            std::fs::read(&ui).unwrap(),
            b"old ui",
            "hydraui untouched too"
        );
        assert_eq!(std::fs::read(&cli).unwrap(), b"old cli");
        let left: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
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
