//! The folder guard: `hydra guard git` (run by hydra's git hooks), the gh and ssh shims, and
//! `hydra allow`. Fails open: anything that goes wrong while loading the configuration allows
//! the command with a warning.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use hydra_core::bindings::{self, Binding};
use hydra_core::config::{load_env, load_global};
use hydra_core::guard::{Check, Facts, GitAction, Verdict, classify_gh, decide, github_owner};
use hydra_core::name::EnvName;
use hydra_core::paths::{HydraPaths, expand_tilde};
use hydra_platform::process::resolve_program;

use super::launch::ignore_ctrl_c;
use crate::style;

/// What the decision needs from the configuration.
struct Loaded {
    binding: Option<Binding>,
    owners: Vec<String>,
    strict: bool,
}

fn load(current: &str) -> anyhow::Result<Loaded> {
    let paths = HydraPaths::discover()?;
    let cwd = std::env::current_dir()?;
    let rules = load_global(&paths)?.bindings;
    let name = EnvName::parse(current)?;
    let github = load_env(&paths, &name)?.github;
    Ok(Loaded {
        binding: bindings::resolve(&cwd, &rules),
        owners: github
            .as_ref()
            .map(|g| g.owners.clone())
            .unwrap_or_default(),
        strict: github.is_some_and(|g| g.strict),
    })
}

/// Prints a verdict to stderr; returns the exit code for `block_code`.
fn report(verdict: Verdict, block_code: i32) -> i32 {
    match verdict {
        Verdict::Allow => 0,
        Verdict::Warn(m) => {
            anstream::eprintln!("{}", style::warn(m));
            0
        }
        Verdict::Block(m) => {
            for (i, line) in m.lines().enumerate() {
                if i == 0 {
                    anstream::eprintln!("{}", style::error(line));
                } else {
                    anstream::eprintln!("{}", style::dim(line));
                }
            }
            block_code
        }
    }
}

/// `hydra guard git <hook> [args...]`, run by the hook wrappers. Never reads stdin.
pub fn git(hook: &str, args: &[String]) -> anyhow::Result<i32> {
    let check = match hook {
        "pre-commit" => Check::Git {
            action: GitAction::Commit,
            dir: None,
            remote: None,
        },
        "pre-push" => Check::Git {
            action: GitAction::Push,
            dir: None,
            remote: args.get(1).cloned(),
        },
        _ => return Ok(0),
    };
    let remote = match &check {
        Check::Git { remote, .. } => remote.clone(),
        Check::Gh { .. } => None,
    };
    Ok(guarded(&check, || remote.as_deref().and_then(github_owner)))
}

/// The fail-open decision for `check` in the current folder; prints it and returns 0 or 1.
/// `target_owner` is only asked for when the environment lists github owners.
fn guarded(check: &Check, target_owner: impl FnOnce() -> Option<String>) -> i32 {
    let current = std::env::var("HYDRA_ENV").ok().filter(|s| !s.is_empty());
    let allow = std::env::var("HYDRA_ALLOW").is_ok_and(|v| v == "1");
    // Outside a hydra terminal, or allowed once: nothing to check.
    let Some(current) = current.filter(|_| !allow) else {
        return 0;
    };
    let loaded = match load(&current) {
        Ok(l) => l,
        Err(e) => {
            anstream::eprintln!(
                "{}",
                style::warn(format!("hydra: warning: guard skipped ({e:#})"))
            );
            return 0;
        }
    };
    let target_owner = if loaded.owners.is_empty() {
        None
    } else {
        target_owner()
    };
    let facts = Facts {
        current_env: Some(&current),
        allow,
        binding: loaded.binding.as_ref(),
        owners: &loaded.owners,
        strict: loaded.strict,
        target_owner: target_owner.as_deref(),
    };
    report(decide(check, &facts), 1)
}

/// gh.exe in a hydra terminal: guards gh's write commands, then runs the real gh.
pub fn gh_shim(args: &[String]) -> anyhow::Result<i32> {
    if let Some(check) = classify_gh(args) {
        let repo = match &check {
            Check::Gh { repo, .. } => repo.clone(),
            Check::Git { .. } => None,
        };
        let code = guarded(&check, || match repo {
            Some(r) => r.split('/').next().map(str::to_string),
            None => origin_owner(),
        });
        if code != 0 {
            return Ok(code);
        }
    }
    run_inherited(Command::new(real_program("gh")?), args)
}

/// The owner of the current folder's `origin` remote, if it is on GitHub.
fn origin_owner() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    github_owner(&String::from_utf8_lossy(&out.stdout))
}

/// ssh.exe in a hydra terminal: adds the environment's git ssh_key, then runs the real ssh.
pub fn ssh_shim(args: &[String]) -> anyhow::Result<i32> {
    let mut full: Vec<String> = Vec::new();
    match ssh_key() {
        Ok(Some(key)) => full.extend([
            "-i".to_string(),
            key.to_string_lossy().into_owned(),
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
        ]),
        Ok(None) => {}
        Err(e) => anstream::eprintln!(
            "{}",
            style::warn(format!("hydra: warning: ssh key not added ({e:#})"))
        ),
    }
    full.extend_from_slice(args);
    run_inherited(Command::new(real_program("ssh")?), &full)
}

/// The current environment's git `ssh_key`, with `~` expanded.
fn ssh_key() -> anyhow::Result<Option<PathBuf>> {
    let Some(current) = std::env::var("HYDRA_ENV").ok().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let paths = HydraPaths::discover()?;
    let name = EnvName::parse(&current)?;
    let Some(key) = load_env(&paths, &name)?.git.and_then(|g| g.ssh_key) else {
        return Ok(None);
    };
    Ok(Some(expand_tilde(&key, &crate::app::user_home()?)))
}

/// `hydra allow -- <command...>`: runs the command once with the guard switched off.
pub fn allow(command: &[String]) -> anyhow::Result<i32> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exe = resolve_program(&command[0], &path, std::env::var_os("PATHEXT").as_deref())
        .ok_or_else(|| anyhow::anyhow!("{} isn't installed or isn't on PATH", command[0]))?;
    let mut cmd = Command::new(exe);
    cmd.env("HYDRA_ALLOW", "1");
    run_inherited(cmd, &command[1..])
}

/// Runs `cmd` with `args` and inherited stdio; returns its exit code.
fn run_inherited(mut cmd: Command, args: &[String]) -> anyhow::Result<i32> {
    cmd.args(args);
    ignore_ctrl_c();
    let status = cmd
        .status()
        .with_context(|| format!("can't start {}", cmd.get_program().to_string_lossy()))?;
    Ok(status.code().unwrap_or(1))
}

/// The real `name`: found on PATH with hydra's shims folders left out.
fn real_program(name: &str) -> anyhow::Result<PathBuf> {
    let shim_dirs: Vec<PathBuf> = [
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf)),
        HydraPaths::discover().ok().map(|p| p.shims_dir()),
    ]
    .into_iter()
    .flatten()
    .filter_map(|d| d.canonicalize().ok())
    .collect();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path)
        .filter(|d| !d.canonicalize().is_ok_and(|c| shim_dirs.contains(&c)));
    let path = std::env::join_paths(dirs).context("PATH holds a folder that can't be searched")?;
    resolve_program(name, &path, std::env::var_os("PATHEXT").as_deref())
        .ok_or_else(|| anyhow::anyhow!("{name} isn't installed or isn't on PATH"))
}
