//! macOS: updating Hydra.app as a whole, so its Info.plist, icon and other resources arrive
//! with the programs instead of staying as the first install left them (spec §7.11).
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::release::Release;

/// The release's zipped Hydra.app (scripts/macos/make-app.sh). Only Apple silicon has one;
/// elsewhere the programs are updated one by one.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const APP_ASSET: Option<&str> = Some("hydra-macos-aarch64.zip");
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub const APP_ASSET: Option<&str> = None;

/// The app inside the zip.
pub const APP_NAME: &str = "Hydra.app";

/// What a Hydra.app must hold to replace the installed one: both programs (the
/// `~/.local/bin/hydra` link names the app's hydra) and the bundle's description.
const REQUIRED: [&str; 3] = [
    "Contents/MacOS/hydraui",
    "Contents/MacOS/hydra",
    "Contents/Info.plist",
];

/// The `.app` that `exe` runs from (`<X.app>/Contents/MacOS/<exe>`), links followed; None for
/// a program that isn't in one.
pub fn app_of(exe: &Path) -> Option<PathBuf> {
    let exe = crate::real_exe(exe.to_path_buf());
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then(|| app.to_path_buf())
}

/// Where an update of `app` works, beside it so the final move is a rename on the same disk.
/// The names start with a dot and don't end in `.app`, so Finder and Launchpad don't list them.
#[derive(Debug, Clone)]
pub struct AppSwap {
    pub target: PathBuf,
    /// The download and the unzipped app; removed when the update ends.
    pub staging: PathBuf,
    /// The replaced app, until the new one is in place.
    pub old: PathBuf,
}

impl AppSwap {
    pub fn for_app(app: &Path) -> Self {
        let dir = app.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = app
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            target: app.to_path_buf(),
            staging: dir.join(format!(".{name}.update")),
            old: dir.join(format!(".{name}.old")),
        }
    }
}

/// Unzips `zip` (made by `ditto -c -k`) into `dest`.
fn unzip(zip: &Path, dest: &Path) -> anyhow::Result<()> {
    let out = Command::new("ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(dest)
        .output()
        .context("ditto didn't run, so the new Hydra.app can't be unzipped - nothing changed")?;
    if !out.status.success() {
        anyhow::bail!(
            "the new Hydra.app can't be unzipped ({}) - nothing changed",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Replaces `app` with the release's Hydra.app (`asset`): downloaded and checked against its
/// .sha256 file, unzipped and checked for both programs before anything is replaced, and a
/// failed replace puts the old app back.
pub fn update_app(
    release: &Release,
    asset: &str,
    app: &Path,
    user_agent: &str,
) -> anyhow::Result<()> {
    let build = release.windows_build(asset)?;
    let swap = AppSwap::for_app(app);
    let _ = fs::remove_dir_all(&swap.staging);
    fs::create_dir_all(&swap.staging)
        .with_context(|| format!("can't write {} - nothing changed", swap.staging.display()))?;
    let result = (|| {
        let zip = swap.staging.join(asset);
        crate::download_verified(&build, &zip, user_agent)?;
        let unzipped = swap.staging.join("unzipped");
        unzip(&zip, &unzipped)?;
        let new = unzipped.join(APP_NAME);
        if let Some(missing) = REQUIRED.iter().find(|p| !new.join(p).is_file()) {
            anyhow::bail!("the release's {APP_NAME} has no {missing} - nothing changed");
        }
        replace_app(&swap.target, &new, &swap.old)
    })();
    let _ = fs::remove_dir_all(&swap.staging);
    result
}

/// target -> old, new -> target, then the old app is deleted; if the second move fails the
/// old app is put back.
pub fn replace_app(target: &Path, new: &Path, old: &Path) -> anyhow::Result<()> {
    let _ = fs::remove_dir_all(old);
    fs::rename(target, old)
        .with_context(|| format!("can't move {} aside - nothing changed", target.display()))?;
    if let Err(e) = fs::rename(new, target) {
        let what = match fs::rename(old, target) {
            Ok(()) => "the old version is back in place".to_string(),
            Err(_) => format!("the old version is {}", old.display()),
        };
        return Err(e).with_context(|| {
            format!("can't install the new app at {} ({what})", target.display())
        });
    }
    // A running program keeps its open files, so the old app can go now.
    let _ = fs::remove_dir_all(old);
    Ok(())
}

/// Removes what an interrupted update of `app` left beside it.
pub fn clean_old(app: &Path) {
    let _ = fs::remove_dir_all(AppSwap::for_app(app).old);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_in_an_app_knows_its_app() {
        let d = tempfile::tempdir().unwrap();
        let macos = d.path().join("Hydra.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("hydraui"), "").unwrap();
        let app = d.path().join("Hydra.app").canonicalize().unwrap();
        assert_eq!(app_of(&macos.join("hydraui")), Some(app));
        let bare = d.path().join("hydraui");
        std::fs::write(&bare, "").unwrap();
        assert_eq!(app_of(&bare), None);
        let not_app = d.path().join("Hydra/Contents/MacOS");
        std::fs::create_dir_all(&not_app).unwrap();
        std::fs::write(not_app.join("hydraui"), "").unwrap();
        assert_eq!(app_of(&not_app.join("hydraui")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_the_app_finds_the_app() {
        let d = tempfile::tempdir().unwrap();
        let macos = d.path().join("Hydra.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("hydra"), "").unwrap();
        std::os::unix::fs::symlink(macos.join("hydra"), d.path().join("hydra")).unwrap();
        let app = d.path().join("Hydra.app").canonicalize().unwrap();
        assert_eq!(app_of(&d.path().join("hydra")), Some(app));
    }

    fn app_with(dir: &Path, name: &str, marker: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), marker).unwrap();
        app
    }

    #[test]
    fn a_failed_move_puts_the_old_app_back() {
        let d = tempfile::tempdir().unwrap();
        let target = app_with(d.path(), "Hydra.app", "old");
        let swap = AppSwap::for_app(&target);
        // No new app: the second move fails after the old one was moved aside.
        let err = replace_app(&target, &d.path().join("missing.app"), &swap.old).unwrap_err();
        assert_eq!(
            std::fs::read_to_string(target.join("Contents/Info.plist")).unwrap(),
            "old"
        );
        assert!(!swap.old.exists());
        assert!(
            format!("{err:#}").contains("the old version is back in place"),
            "{err:#}"
        );
    }

    #[test]
    fn an_app_is_replaced_whole() {
        let d = tempfile::tempdir().unwrap();
        let target = app_with(d.path(), "Hydra.app", "old");
        std::fs::write(target.join("Contents/stale"), "").unwrap();
        let new = app_with(&d.path().join("staging"), "Hydra.app", "new");
        let swap = AppSwap::for_app(&target);
        replace_app(&target, &new, &swap.old).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("Contents/Info.plist")).unwrap(),
            "new"
        );
        assert!(!target.join("Contents/stale").exists());
        assert!(!swap.old.exists() && !new.exists());
    }

    #[test]
    fn a_leftover_old_app_is_cleaned_up() {
        let d = tempfile::tempdir().unwrap();
        let target = app_with(d.path(), "Hydra.app", "now");
        let old = app_with(d.path(), ".Hydra.app.old", "before");
        clean_old(&target);
        assert!(!old.exists());
        assert!(target.exists());
    }
}
