use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::Context;
use hydra_core::bindings;
use hydra_core::config::{load_env, load_global};
use hydra_core::envs;
use hydra_core::lock::hold_shared;
use hydra_core::name::EnvName;
use hydra_core::paths::expand_tilde;
use hydra_core::provider::Ctx;
use hydra_core::resolve::{ENV_VARS_MARKER, LaunchEnv, PrepareOptions, prepare};
use hydra_platform::process::EnvRunner;
use hydra_platform::shell::{PromptStyle, ShellKind, find_shell, shell_command, write_init};

use crate::app::App;
use crate::style;

pub fn prepare_launch(
    app: &App,
    name: &EnvName,
    opts: &PrepareOptions,
) -> anyhow::Result<LaunchEnv> {
    remove_legacy_shims(&app.paths.legacy_shims_dir());
    let providers = hydra_providers::all();
    let opts = PrepareOptions {
        inherited_env_vars: std::env::var(ENV_VARS_MARKER).ok(),
        ..opts.clone()
    };
    Ok(prepare(
        &app.paths,
        name,
        &app.user_home,
        &providers,
        app.store.as_ref(),
        &opts,
    )?)
}

/// Deletes the gh/ssh shims folder an older hydra made. Best effort: a file that is in use
/// (a shim still running in an old terminal) is noted and left for the next launch.
fn remove_legacy_shims(dir: &Path) {
    if !dir.exists() {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(dir) {
        anstream::eprintln!(
            "{}",
            style::dim(format!(
                "hydra: note: can't remove old shims folder {} yet ({e})",
                dir.display()
            ))
        );
    }
}

/// Ctrl-C belongs to the child; hydra stays alive to return its exit code.
pub fn ignore_ctrl_c() {
    let _ = ctrlc::set_handler(|| {});
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Tells the user which of the environment's tools aren't signed in yet.
fn print_sign_in_hints(app: &App, name: &EnvName, launch: &LaunchEnv) {
    let ctx = Ctx {
        name,
        env: &launch.config,
        paths: &app.paths,
        user_home: &app.user_home,
        secrets: app.store.as_ref(),
    };
    for p in hydra_providers::all() {
        if !p.is_configured(&launch.config) {
            continue;
        }
        let Some(hint) = p.sign_in_hint(&ctx) else {
            continue;
        };
        match launch.config.borrowed.get(p.id()) {
            Some(owner) => {
                let fix = match hint.strip_prefix("not signed in - run ") {
                    Some(cmd) => {
                        anstream::eprintln!(
                            "{}",
                            style::warn(format!(
                                "hydra: {}: not signed in - {name} borrows it from {owner}",
                                p.id()
                            ))
                        );
                        format!("  -> sign in there: hydra shell {owner}, then {cmd}")
                    }
                    None => {
                        anstream::eprintln!(
                            "{}",
                            style::warn(format!(
                                "hydra: {}: {hint} - {name} borrows it from {owner}",
                                p.id()
                            ))
                        );
                        format!("  -> fix it there: hydra shell {owner}")
                    }
                };
                anstream::eprintln!("{}", style::dim(fix));
            }
            None => {
                anstream::eprintln!("{}", style::warn(format!("hydra: {}: {hint}", p.id())));
            }
        }
    }
}

/// The environment's `home`, when a named shell is opened from a folder that isn't bound to it.
fn home_folder(
    app: &App,
    name: &EnvName,
    here: &std::path::Path,
    rules: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<Option<PathBuf>> {
    if bindings::resolve(here, rules).is_some_and(|b| b.env == name.as_str()) {
        return Ok(None);
    }
    let Some(home) = load_env(&app.paths, name)?.home else {
        return Ok(None);
    };
    let dir = expand_tilde(&home, &app.user_home);
    if !dir.is_dir() {
        anstream::eprintln!(
            "{}",
            style::warn(format!(
                "hydra: warning: home {} isn't a folder; staying in the current folder",
                dir.display()
            ))
        );
        return Ok(None);
    }
    Ok(Some(dir))
}

pub fn shell(
    app: &App,
    env: Option<String>,
    shell: Option<String>,
    cwd: Option<PathBuf>,
) -> anyhow::Result<i32> {
    let global = load_global(&app.paths)?;
    let here = std::env::current_dir().context("can't find the current folder")?;
    if let Some(dir) = &cwd {
        anyhow::ensure!(dir.is_dir(), "{} isn't a folder", dir.display());
    }
    let probe = match &cwd {
        Some(dir) => std::path::absolute(dir).unwrap_or_else(|_| dir.clone()),
        None => here.clone(),
    };
    let name = match env.as_deref() {
        Some(e) => app.env_name(e)?,
        None => {
            let Some(b) = bindings::resolve(&probe, &global.bindings) else {
                let l = envs::list(&app.paths)?;
                let names: Vec<&str> = l.valid.iter().map(|n| n.as_str()).collect();
                anyhow::bail!(
                    "{} isn't bound to an environment - run hydra shell <env>
{}",
                    probe.display(),
                    style::dim(format!("  environments: {}", names.join(", ")))
                );
            };
            let name = app.env_name(&b.env).with_context(|| {
                format!(
                    "{} names environment {}, which doesn't exist",
                    b.describe(),
                    b.env
                )
            })?;
            anstream::eprintln!(
                "{}",
                style::dim(format!("opening {name} ({})", b.describe()))
            );
            name
        }
    };
    let folder = match cwd {
        Some(dir) => Some(dir),
        None if env.is_some() => home_folder(app, &name, &here, &global.bindings)?,
        None => None,
    };
    let kind_name = shell
        .or(global.default_shell.clone())
        .unwrap_or_else(|| "pwsh".to_string());
    let kind = ShellKind::parse(&kind_name)
        .with_context(|| format!("unknown shell {kind_name:?}; use pwsh or bash"))?;
    let exe = find_shell(kind, global.git_bash.as_deref()).with_context(|| match kind {
        ShellKind::Pwsh => "pwsh.exe isn't on PATH; install PowerShell 7".to_string(),
        ShellKind::Bash => "Git Bash wasn't found; set git_bash in config.toml".to_string(),
    })?;
    let launch = prepare_launch(app, &name, &PrepareOptions::default())?;
    let _lock = hold_shared(&app.paths, &name)?;
    print_sign_in_hints(app, &name, &launch);
    let style = PromptStyle {
        name: name.as_str(),
        rgb: launch.config.rgb().map(|c| (c.0, c.1, c.2)),
    };
    let init = write_init(kind, &app.paths.state_dir(&name), &style)?;
    let mut cmd = shell_command(kind, &exe, &init);
    launch.apply(&mut cmd);
    if let Some(dir) = folder {
        cmd.current_dir(dir);
    }
    ignore_ctrl_c();
    let status = cmd
        .status()
        .with_context(|| format!("can't start {}", exe.display()))?;
    Ok(exit_code(status))
}

pub fn run(app: &App, env: String, command: Vec<String>) -> anyhow::Result<i32> {
    let name = app.env_name(&env)?;
    let launch = prepare_launch(app, &name, &PrepareOptions::default())?;
    // Held until the child exits, so env rm/rename know the environment is in use.
    let _lock = hold_shared(&app.paths, &name)?;
    let runner = EnvRunner { launch: &launch };
    let mut cmd = runner.command(&command[0]).map_err(anyhow::Error::msg)?;
    cmd.args(&command[1..]);
    ignore_ctrl_c();
    let status = cmd
        .status()
        .with_context(|| format!("can't start {}", command[0]))?;
    Ok(exit_code(status))
}
