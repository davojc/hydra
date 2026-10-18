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

    let current = Version::parse(VERSION)
        .with_context(|| format!("this hydra's version {VERSION} isn't valid"))?;
    let latest = release.version()?;
    let newer = release::is_newer(&latest, &current);
    if !newer && !force {
        anstream::println!("{}", style::ok(format!("hydra {current} is up to date")));
        return Ok(0);
    }
    if check {
        if newer {
            anstream::println!("hydra {current} is installed; {latest} is available");
        } else {
            anstream::println!("{}", style::ok(format!("hydra {current} is up to date")));
        }
        return Ok(0);
    }

    let others = other_hydra_pids()?;
    if !others.is_empty() {
        anstream::eprintln!(
            "{}",
            style::warn("hydra is in use - not updating. Close these first:")
        );
        for pid in others {
            anstream::eprintln!("  pid {pid}");
        }
        return Ok(1);
    }

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
