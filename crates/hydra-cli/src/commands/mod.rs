mod auth;
mod env;
mod init;
pub mod launch;
mod secret;
mod whoami;

use crate::app::App;
use crate::cli::Cmd;

pub fn run(cmd: Cmd) -> anyhow::Result<i32> {
    let app = App::from_env()?;
    match cmd {
        Cmd::Init => init::run(&app),
        Cmd::Env { command } => env::run(&app, command),
        Cmd::Secret { command } => secret::run(&app, command),
        Cmd::Shell { env, shell, cwd } => launch::shell(&app, env, shell, cwd),
        Cmd::Run { env, command } => launch::run(&app, env, command),
        Cmd::Auth { provider, env } => auth::run(&app, provider, env),
        Cmd::Whoami { env } => whoami::run(&app, env),
    }
}
