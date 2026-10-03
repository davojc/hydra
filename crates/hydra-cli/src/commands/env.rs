use hydra_core::config::load_env;
use hydra_core::envs;
use hydra_core::name::EnvName;

use crate::app::App;
use crate::cli::EnvCmd;
use crate::prompt;

pub fn run(app: &App, cmd: EnvCmd) -> anyhow::Result<i32> {
    match cmd {
        EnvCmd::New { name } => {
            let n = EnvName::parse(&name)?;
            let file = envs::create(&app.paths, &n)?;
            println!("created {}", file.display());
            println!("edit it with: hydra env edit {n}");
            Ok(0)
        }
        EnvCmd::List => {
            let l = envs::list(&app.paths)?;
            if l.valid.is_empty() {
                println!("no environments yet; create one with: hydra env new <name>");
            }
            for n in &l.valid {
                match load_env(&app.paths, n) {
                    Ok(c) => println!(
                        "{:<16} {:<24} {}",
                        n,
                        c.label_or(n),
                        c.color.as_deref().unwrap_or("")
                    ),
                    Err(e) => println!("{n:<16} error: {e}"),
                }
            }
            for bad in &l.invalid {
                eprintln!(
                    "hydra: warning: ignoring envs/{bad}: names use lowercase letters, digits and '-'"
                );
            }
            Ok(0)
        }
        EnvCmd::Rm { name, yes } => {
            let n = app.env_name(&name)?;
            let state = app.paths.state_dir(&n);
            let delete_state = state.exists()
                && (yes
                    || prompt::confirm(
                        &format!("Also delete saved logins in {}?", state.display()),
                        false,
                    )?);
            let r = envs::remove(&app.paths, &n, app.store.as_ref(), delete_state)?;
            println!("removed environment {n}");
            if !r.secrets.is_empty() {
                println!("removed secrets: {}", r.secrets.join(", "));
            }
            if state.exists() {
                println!("kept saved logins in {}", state.display());
            }
            for b in &r.bindings {
                eprintln!("hydra: warning: binding {b:?} in config.toml still points at {n}");
            }
            Ok(0)
        }
    }
}
