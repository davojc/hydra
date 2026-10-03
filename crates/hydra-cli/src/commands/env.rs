use std::path::Path;
use std::process::Command;

use anyhow::Context;
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
        EnvCmd::Edit { name } => {
            let n = app.env_name(&name)?;
            let file = app.paths.env_file(&n);
            loop {
                open_editor(&file)?;
                match load_env(&app.paths, &n) {
                    Ok(_) => {
                        println!("saved {}", file.display());
                        return Ok(0);
                    }
                    Err(e) => {
                        eprintln!("hydra: {e}");
                        if !prompt::confirm("Reopen the editor to fix it?", true)? {
                            return Ok(1);
                        }
                    }
                }
            }
        }
        EnvCmd::Rename { old, new } => {
            let o = app.env_name(&old)?;
            let n = EnvName::parse(&new)?;
            let r = envs::rename(&app.paths, &o, &n, app.store.as_ref())?;
            println!("renamed {o} to {n}");
            if !r.bindings.is_empty() {
                println!("updated bindings: {}", r.bindings.join(", "));
            }
            if r.references > 0 {
                println!("updated {} secret reference(s)", r.references);
            }
            if !r.secrets.is_empty() {
                println!("moved secrets: {}", r.secrets.join(", "));
            }
            println!("if any repo has a .hydra file with env = \"{o}\", change it to \"{n}\"");
            Ok(0)
        }
    }
}

/// Runs $VISUAL, $EDITOR or notepad on the file and waits for it to close.
fn open_editor(file: &Path) -> anyhow::Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "notepad".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().context("EDITOR is empty")?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exe = hydra_platform::process::resolve_program(
        program,
        &path,
        std::env::var_os("PATHEXT").as_deref(),
    )
    .with_context(|| format!("editor {program:?} not found; set EDITOR"))?;
    let status = Command::new(exe).args(parts).arg(file).status()?;
    if !status.success() {
        anyhow::bail!("the editor exited with {status}");
    }
    Ok(())
}
