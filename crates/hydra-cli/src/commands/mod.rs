mod env;
mod init;
mod secret;

use crate::app::App;
use crate::cli::Cmd;

pub fn run(cmd: Cmd) -> anyhow::Result<i32> {
    let app = App::from_env()?;
    match cmd {
        Cmd::Init => init::run(&app),
        Cmd::Env { command } => env::run(&app, command),
        Cmd::Secret { command } => secret::run(&app, command),
    }
}
