//! `hydra update`: install the latest GitHub release over the running exe.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use semver::Version;
use sha2::{Digest, Sha256};

use crate::VERSION;
use crate::release::{self, Release, WindowsBuild};

/// The replaced exe. Windows can't delete a running exe, so the next hydra start removes it.
pub const OLD_EXE: &str = "hydra.old.exe";
const NEW_EXE: &str = "hydra.new.exe";

pub fn run(check: bool, force: bool) -> anyhow::Result<i32> {
    let api = std::env::var("HYDRA_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into());
    let url = format!(
        "{}/repos/davojc/hydra/releases/latest",
        api.trim_end_matches('/')
    );
    let release = Release::parse(
        &get(&url)?
            .into_string()
            .context("can't read GitHub's answer")?,
    )?;

    let current = Version::parse(VERSION)
        .with_context(|| format!("this hydra's version {VERSION} isn't valid"))?;
    let latest = release.version()?;
    let newer = release::is_newer(&latest, &current);
    if !newer && !force {
        println!("hydra {current} is up to date");
        return Ok(0);
    }
    if check {
        if newer {
            println!("hydra {current} is installed; {latest} is available");
        } else {
            println!("hydra {current} is up to date");
        }
        return Ok(0);
    }

    let others = other_hydra_pids()?;
    if !others.is_empty() {
        eprintln!("hydra is in use - not updating. Close these first:");
        for pid in others {
            eprintln!("  pid {pid}");
        }
        return Ok(1);
    }

    let build = release.windows_build()?;
    let target = match std::env::var_os("HYDRA_UPDATE_EXE") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().context("can't find the running hydra exe")?,
    };
    let dir = target.parent().context("the hydra exe has no folder")?;
    let new = dir.join(NEW_EXE);
    if let Err(e) = download_verified(&build, &new) {
        let _ = fs::remove_file(&new);
        return Err(e);
    }
    replace(&target, &new, &dir.join(OLD_EXE))?;
    println!("updated to {latest}");
    Ok(0)
}

fn get(url: &str) -> anyhow::Result<ureq::Response> {
    match ureq::get(url)
        .set("User-Agent", &format!("hydra/{VERSION}"))
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(code, _)) => anyhow::bail!("GitHub answered HTTP {code} for {url}"),
        Err(e) => anyhow::bail!("can't reach GitHub: {e}"),
    }
}

/// Downloads the exe to `new` and checks it against the release's .sha256 file.
fn download_verified(build: &WindowsBuild, new: &Path) -> anyhow::Result<()> {
    let mut body = get(&build.exe_url)?.into_reader();
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

    let sums = get(&build.sha256_url)?
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

/// target -> old, new -> target; puts the old exe back if the second step fails.
fn replace(target: &Path, new: &Path, old: &Path) -> anyhow::Result<()> {
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
            "the old hydra is back in place"
        } else {
            "the old hydra is hydra.old.exe"
        };
        return Err(e).with_context(|| {
            format!(
                "can't install the new hydra at {} ({what})",
                target.display()
            )
        });
    }
    // Fails while the old exe is still running (it is this process); the next start removes it.
    let _ = fs::remove_file(old);
    Ok(())
}

/// PIDs of other running hydra processes. `HYDRA_UPDATE_SKIP_PROCESS_CHECK=1` skips the check (tests).
fn other_hydra_pids() -> anyhow::Result<Vec<u32>> {
    if std::env::var("HYDRA_UPDATE_SKIP_PROCESS_CHECK").as_deref() == Ok("1") {
        return Ok(Vec::new());
    }
    let pids = if cfg!(windows) {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq hydra.exe", "/FO", "CSV", "/NH"])
            .output()
            .context("can't list running processes (tasklist)")?;
        release::parse_tasklist_pids(&String::from_utf8_lossy(&out.stdout))
    } else {
        let out = std::process::Command::new("pgrep")
            .args(["-x", "hydra"])
            .output()
            .context("can't list running processes (pgrep)")?;
        release::parse_pgrep_pids(&String::from_utf8_lossy(&out.stdout))
    };
    let me = std::process::id();
    Ok(pids.into_iter().filter(|&p| p != me).collect())
}
