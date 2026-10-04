//! `hydra guard ...`: the decision behind hydra's git hooks. Fails open: anything that goes
//! wrong while loading the configuration allows the command with a warning.
use hydra_core::bindings::{self, Binding};
use hydra_core::config::{load_env, load_global};
use hydra_core::guard::{Check, Facts, GitAction, Verdict, decide, github_owner};
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;

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
    let current = std::env::var("HYDRA_ENV").ok().filter(|s| !s.is_empty());
    let allow = std::env::var("HYDRA_ALLOW").is_ok_and(|v| v == "1");
    // Outside a hydra terminal, or allowed once: nothing to check.
    let Some(current) = current.filter(|_| !allow) else {
        return Ok(0);
    };
    let loaded = match load(&current) {
        Ok(l) => l,
        Err(e) => {
            anstream::eprintln!(
                "{}",
                style::warn(format!("hydra: warning: guard skipped ({e:#})"))
            );
            return Ok(0);
        }
    };
    let target_owner = match &check {
        Check::Git {
            remote: Some(url), ..
        } => github_owner(url),
        _ => None,
    };
    let facts = Facts {
        current_env: Some(&current),
        allow,
        binding: loaded.binding.as_ref(),
        owners: &loaded.owners,
        strict: loaded.strict,
        target_owner: target_owner.as_deref(),
    };
    Ok(report(decide(&check, &facts), 1))
}
