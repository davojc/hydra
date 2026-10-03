use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::Context;
use hydra_core::config::load_global;
use hydra_core::lock::hold_shared;
use hydra_core::name::EnvName;
use hydra_core::provider::Ctx;
use hydra_core::resolve::{ENV_VARS_MARKER, LaunchEnv, PrepareOptions, prepare};
use hydra_platform::process::EnvRunner;
use hydra_platform::shell::{PromptStyle, ShellKind, find_shell, shell_command, write_init};

use crate::app::App;

pub fn prepare_launch(
    app: &App,
    name: &EnvName,
    opts: &PrepareOptions,
) -> anyhow::Result<LaunchEnv> {
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
        if p.is_configured(&launch.config)
            && let Some(hint) = p.sign_in_hint(&ctx)
        {
            eprintln!("hydra: {}: {hint}", p.id());
        }
    }
}

pub fn shell(
    app: &App,
    env: Option<String>,
    shell: Option<String>,
    cwd: Option<PathBuf>,
) -> anyhow::Result<i32> {
    let Some(env) = env else {
        anyhow::bail!("name an environment: hydra shell <env>");
    };
    let name = app.env_name(&env)?;
    if let Some(dir) = &cwd {
        anyhow::ensure!(dir.is_dir(), "{} isn't a folder", dir.display());
    }
    let global = load_global(&app.paths)?;
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
    let init = write_init(
        kind,
        &app.paths.state_dir(&name),
        &style,
        &launch.path_prepend,
    )?;
    let mut cmd = shell_command(kind, &exe, &init);
    launch.apply(&mut cmd);
    if let Some(dir) = cwd {
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
