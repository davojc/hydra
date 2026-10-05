use std::path::Path;
use std::process::Command;

use anyhow::Context;
use hydra_core::bindedit;
use hydra_core::config::load_env;
use hydra_core::envs;
use hydra_core::name::EnvName;

use crate::app::App;
use crate::cli::EnvCmd;
use crate::commands::bind;
use crate::prompt;
use crate::style;

pub fn run(app: &App, cmd: EnvCmd) -> anyhow::Result<i32> {
    match cmd {
        EnvCmd::New { name, home } => {
            let n = EnvName::parse(&name)?;
            // Resolve the folder first so a bad path fails before anything is created.
            let home = home
                .map(|h| {
                    std::path::absolute(&h)
                        .with_context(|| format!("can't resolve {}", h.display()))
                })
                .transpose()?;
            let file = envs::create(&app.paths, &n)?;
            anstream::println!("{}", style::ok(format!("created {}", file.display())));
            if let Some(dir) = home {
                let text = std::fs::read_to_string(&file)?;
                let text = bindedit::set_home(&text, &dir).map_err(anyhow::Error::msg)?;
                std::fs::write(&file, text)?;
                let shown = dir.to_string_lossy().replace('\\', "/");
                anstream::println!("{}", style::ok(format!("home = \"{shown}\"")));
                bind::bind(
                    app,
                    vec![dir.to_string_lossy().into_owned(), n.to_string()],
                    false,
                    false,
                )?;
            }
            anstream::println!(
                "{}",
                style::dim(format!("edit it with: hydra env edit {n}"))
            );
            Ok(0)
        }
        EnvCmd::List => {
            let l = envs::list(&app.paths)?;
            if l.valid.is_empty() {
                anstream::println!("no environments yet; create one with: hydra env new <name>");
            }
            for n in &l.valid {
                match load_env(&app.paths, n) {
                    Ok(c) => anstream::println!(
                        "{} {:<24} {}",
                        style::env_name(format!("{n:<16}"), c.rgb().map(|c| (c.0, c.1, c.2))),
                        c.label_or(n),
                        c.color.as_deref().unwrap_or("")
                    ),
                    Err(e) => anstream::println!(
                        "{} error: {e}",
                        style::env_name(format!("{n:<16}"), None)
                    ),
                }
            }
            for bad in &l.invalid {
                anstream::eprintln!(
                    "{}",
                    style::warn(format!(
                        "hydra: warning: ignoring envs/{bad}: names use lowercase letters, digits and '-'"
                    ))
                );
            }
            Ok(0)
        }
        EnvCmd::Rm { name, yes } => rm(app, &name, yes, &mut |q| prompt::confirm(q, false)),
        EnvCmd::Edit { name } => {
            let n = app.env_name(&name)?;
            let file = app.paths.env_file(&n);
            loop {
                open_editor(&file)?;
                match load_env(&app.paths, &n) {
                    Ok(_) => {
                        anstream::println!("{}", style::ok(format!("saved {}", file.display())));
                        return Ok(0);
                    }
                    Err(e) => {
                        anstream::eprintln!("{}", style::error(format!("hydra: {e}")));
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
            anstream::println!("{}", style::ok(format!("renamed {o} to {n}")));
            if !r.bindings.is_empty() {
                anstream::println!("updated bindings: {}", r.bindings.join(", "));
            }
            if !r.borrowers.is_empty() {
                anstream::println!("updated borrowers: {}", r.borrowers.join(", "));
            }
            if r.references > 0 {
                anstream::println!("updated {} secret reference(s)", r.references);
            }
            if !r.secrets.is_empty() {
                anstream::println!("moved secrets: {}", r.secrets.join(", "));
            }
            anstream::println!(
                "if any repo has a .hydra file with env = \"{o}\", change it to \"{n}\""
            );
            Ok(0)
        }
    }
}

/// `hydra env rm`. `ask` is the yes/no prompt (asked only when the env can be removed).
fn rm(
    app: &App,
    name: &str,
    yes: bool,
    ask: &mut dyn FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<i32> {
    let n = app.env_name(name)?;
    // Refuse (running, or lending a tool) before asking anything.
    envs::check_removable(&app.paths, &n)?;
    let state = app.paths.state_dir(&n);
    let delete_state = state.exists()
        && (yes || ask(&format!("Also delete saved logins in {}?", state.display()))?);
    let r = envs::remove(&app.paths, &n, app.store.as_ref(), delete_state)?;
    anstream::println!("{}", style::ok(format!("removed environment {n}")));
    if !r.secrets.is_empty() {
        anstream::println!("removed secrets: {}", r.secrets.join(", "));
    }
    if state.exists() {
        anstream::println!("kept saved logins in {}", state.display());
    }
    for b in &r.bindings {
        anstream::eprintln!(
            "{}",
            style::warn(format!(
                "hydra: warning: binding {b:?} in config.toml still points at {n}"
            ))
        );
    }
    Ok(0)
}

/// Runs $VISUAL, $EDITOR or notepad on the file and waits for it to close.
fn open_editor(file: &Path) -> anyhow::Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "notepad".to_string());
    // A value that is itself an existing file (e.g. a path with spaces) is the program, no arguments.
    let whole = Path::new(editor.trim());
    if whole.is_file() {
        let status = Command::new(whole).arg(file).status()?;
        if !status.success() {
            anyhow::bail!("the editor exited with {status}");
        }
        return Ok(());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use hydra_core::paths::HydraPaths;
    use hydra_core::secret::MemoryStore;

    fn app_with(envs: &[(&str, &str)]) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        for (name, toml) in envs {
            let n = EnvName::parse(name).unwrap();
            std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
            std::fs::write(paths.env_file(&n), toml).unwrap();
            std::fs::create_dir_all(paths.state_dir(&n)).unwrap();
        }
        let app = App {
            paths,
            user_home: dir.path().join("user"),
            store: Box::new(MemoryStore::default()),
        };
        (dir, app)
    }

    #[test]
    fn rm_refuses_before_asking() {
        let (_d, app) = app_with(&[
            ("personal", "[claude]\n"),
            ("work", "[claude]\nfrom = \"personal\"\n"),
        ]);
        let mut asked = Vec::new();
        let e = rm(&app, "personal", false, &mut |q| {
            asked.push(q.to_string());
            Ok(true)
        })
        .unwrap_err();
        assert!(
            e.to_string().contains("work borrows claude from personal"),
            "{e}"
        );
        assert!(asked.is_empty(), "asked {asked:?}");
        let personal = EnvName::parse("personal").unwrap();
        assert!(app.paths.env_file(&personal).is_file());

        // Running: also refused without a question.
        let work = EnvName::parse("work").unwrap();
        let _lock = hydra_core::lock::hold_shared(&app.paths, &work).unwrap();
        let e = rm(&app, "work", false, &mut |q| {
            asked.push(q.to_string());
            Ok(true)
        })
        .unwrap_err();
        assert!(asked.is_empty(), "asked {asked:?} ({e})");
        drop(_lock);

        // Removable: now it asks.
        rm(&app, "work", false, &mut |q| {
            asked.push(q.to_string());
            Ok(false)
        })
        .unwrap();
        assert_eq!(asked.len(), 1);
        assert!(
            asked[0].starts_with("Also delete saved logins in "),
            "{asked:?}"
        );
    }
}
