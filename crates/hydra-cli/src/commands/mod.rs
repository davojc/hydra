mod auth;
mod bind;
mod env;
mod guard;
mod init;
pub mod launch;
mod secret;
mod tools;
pub mod update;
mod whoami;

use crate::app::App;
use crate::cli::{Cmd, GuardCmd};

pub fn run(cmd: Cmd) -> anyhow::Result<i32> {
    // Updating needs no hydra home, so it works even when ~/.hydra is missing or broken.
    if let Cmd::Update { check, force } = cmd {
        return update::run(check, force);
    }
    // The guard loads what it needs itself and fails open, so it runs before the App.
    if let Cmd::Guard { command } = cmd {
        return match command {
            GuardCmd::Git { hook, args } => guard::git(&hook, &args),
        };
    }
    let app = App::from_env()?;
    match cmd {
        Cmd::Init => init::run(&app),
        Cmd::Env { command } => env::run(&app, command),
        Cmd::Secret { command } => secret::run(&app, command),
        Cmd::Shell { env, shell, cwd } => launch::shell(&app, env, shell, cwd),
        Cmd::Run { env, command } => launch::run(&app, env, command),
        Cmd::Auth { provider, env } => auth::run(&app, provider, env),
        Cmd::Add {
            tool,
            env,
            mcp_exclude,
            owners,
            strict,
            name,
            email,
            ssh_key,
            signing_key,
            profile,
            isolate,
            credentials_secret,
            kube_config,
            api_key_secret,
        } => tools::add(
            &app,
            tool,
            env,
            tools::Flags {
                mcp_exclude,
                owners,
                strict,
                name,
                email,
                ssh_key,
                signing_key,
                profile,
                isolate,
                credentials_secret,
                kube_config,
                api_key_secret,
            },
        ),
        Cmd::Remove { tool, env } => tools::remove(&app, tool, env),
        Cmd::Whoami { env } => whoami::run(&app, env),
        Cmd::Bind { args, file, list } => bind::bind(&app, args, file, list),
        Cmd::Unbind { path } => bind::unbind(&app, path),
        Cmd::Update { .. } | Cmd::Guard { .. } => unreachable!("handled above"),
    }
}
