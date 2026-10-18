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
    let api = std::env::var("HYDRA_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into());
    format!(
        "{}/repos/davojc/hydra/releases/latest",
        api.trim_end_matches('/')
    )
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
        let swaps = [
            swap_in(dir.path(), "hydraui.exe"),
            swap_in(dir.path(), "hydra.exe"),
        ];
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
