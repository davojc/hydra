use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::Context;
use hydra_core::config::load_global;
use hydra_core::lock::hold_shared;
use hydra_core::name::EnvName;
use hydra_core::resolve::{LaunchEnv, PrepareOptions, prepare};
use hydra_platform::process::EnvRunner;
use hydra_platform::shell::{PromptStyle, ShellKind, find_shell, shell_command, write_init};

use crate::app::App;

pub fn prepare_launch(
    app: &App,
    name: &EnvName,
    opts: &PrepareOptions,
) -> anyhow::Result<LaunchEnv> {
    let providers = hydra_providers::all();
    Ok(prepare(
        &app.paths,
        name,
        &app.user_home,
        &providers,
        app.store.as_ref(),
        opts,
    )?)
}

/// Ctrl-C belongs to the child; hydra stays alive to return its exit code.
pub fn ignore_ctrl_c() {
    let _ = ctrlc::set_handler(|| {});
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
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
        anyhow::ensure!(dir.is_dir(), "{} isn't a folder", dir.display());
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
    let runner = EnvRunner { launch: &launch };
    let mut cmd = runner.command(&command[0]).map_err(anyhow::Error::msg)?;
    cmd.args(&command[1..]);
    ignore_ctrl_c();
    let status = cmd
        .status()
        .with_context(|| format!("can't start {}", command[0]))?;
    Ok(exit_code(status))
}
