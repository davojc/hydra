use std::path::{Path, PathBuf};

use anyhow::Context;
use hydra_core::bindedit;
use hydra_core::bindings;
use hydra_core::config::{self, GlobalConfig};

use crate::app::App;
use crate::style;

fn absolute(path: &str) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("can't make {path} absolute"))
}

fn current_dir() -> anyhow::Result<PathBuf> {
    std::env::current_dir().context("can't find the current folder")
}

fn read_config(app: &App) -> anyhow::Result<String> {
    let path = app.paths.config_file();
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(t.strip_prefix('\u{feff}').unwrap_or(&t).to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("can't read {}", path.display())),
    }
}

/// Checks the edited text as a whole before anything is written.
fn write_config(app: &App, text: &str) -> anyhow::Result<()> {
    let path = app.paths.config_file();
    let cfg: GlobalConfig =
        toml::from_str(text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    for (pattern, env) in &cfg.bindings {
        hydra_core::name::EnvName::parse(env)
            .map_err(|e| anyhow::anyhow!("{}: binding {pattern:?}: {e}", path.display()))?;
    }
    std::fs::write(&path, text).with_context(|| format!("can't write {}", path.display()))
}

pub fn bind(app: &App, args: Vec<String>, file: bool, list: bool) -> anyhow::Result<i32> {
    if list {
        return list_rules(app);
    }
    let (path, env) = match args.as_slice() {
        [] => return show(app),
        [env] => (current_dir()?, env.as_str()),
        [path, env] => (absolute(path)?, env.as_str()),
        _ => anyhow::bail!("too many arguments: hydra bind [<path>] <env>"),
    };
    let env = app.env_name(env)?;
    let env = env.as_str();
    // Likely a typo, but binding a folder before it is created is legitimate.
    if !path.exists() {
        anstream::eprintln!(
            "{}",
            style::warn(format!(
                "hydra: warning: {} doesn't exist; binding it anyway",
                path.display()
            ))
        );
    }
    let rules = config::load_global(&app.paths)?.bindings;
    if let Some(existing) = bindings::resolve(&path, &rules)
        && existing.env != env
    {
        anstream::println!(
            "{}",
            style::warn(format!(
                "note: {} was {} ({}); this rule overrides it for this folder",
                path.display(),
                existing.env,
                existing.describe()
            ))
        );
    }
    if file {
        return bind_file(&path, env);
    }
    let pattern = bindedit::rule_for_dir(&path);
    let text = bindedit::add_rule(&read_config(app)?, &pattern, env).map_err(anyhow::Error::msg)?;
    write_config(app, &text)?;
    anstream::println!(
        "{}",
        style::ok(format!(
            "bound {} -> {env} (rule {pattern})",
            path.display()
        ))
    );
    Ok(0)
}

fn bind_file(dir: &Path, env: &str) -> anyhow::Result<i32> {
    if !dir.is_dir() {
        anyhow::bail!("{} isn't a folder", dir.display());
    }
    let written = bindedit::write_hydra_file(dir, env)
        .with_context(|| format!("can't write .hydra in {}", dir.display()))?;
    let mut line = format!("wrote {} (env = \"{env}\")", written.display());
    let excluded = bindedit::exclude_in_git(dir);
    if matches!(excluded, Ok(true)) {
        line.push_str(", added to .git/info/exclude");
    }
    anstream::println!("{}", style::ok(line));
    if let Err(e) = excluded {
        anstream::println!(
            "{}",
            style::warn(format!(
                "hydra: warning: couldn't add .hydra to .git/info/exclude ({e})"
            ))
        );
    }
    Ok(0)
}

fn show(app: &App) -> anyhow::Result<i32> {
    let dir = current_dir()?;
    let rules = config::load_global(&app.paths)?.bindings;
    match bindings::resolve(&dir, &rules) {
        Some(b) => anstream::println!("{} -> {} ({})", dir.display(), b.env, b.describe()),
        None => anstream::println!("{} isn't bound", dir.display()),
    }
    Ok(0)
}

fn list_rules(app: &App) -> anyhow::Result<i32> {
    let rules = config::load_global(&app.paths)?.bindings;
    if rules.is_empty() {
        anstream::println!(
            "{}",
            style::dim("no rules (bind a folder with `hydra bind <env>`)")
        );
    }
    for (pattern, env) in rules {
        anstream::println!("{pattern}  ->  {env}");
    }
    Ok(0)
}

pub fn unbind(app: &App, path: Option<String>) -> anyhow::Result<i32> {
    let dir = match path {
        Some(p) => absolute(&p)?,
        None => current_dir()?,
    };
    let pattern = bindedit::rule_for_dir(&dir);
    let (text, removed_rule) =
        bindedit::remove_rule(&read_config(app)?, &pattern).map_err(anyhow::Error::msg)?;
    let hydra_file = dir.join(".hydra");
    let has_file = hydra_file.is_file();
    if !removed_rule && !has_file {
        anstream::println!("{} has no binding of its own", dir.display());
        let rules = config::load_global(&app.paths)?.bindings;
        if let Some(b) = bindings::resolve(&dir, &rules) {
            anstream::println!("it's covered by {}", b.describe());
        }
        return Ok(0);
    }
    if removed_rule {
        write_config(app, &text)?;
        anstream::println!("{}", style::ok(format!("removed rule {pattern}")));
    }
    if has_file {
        std::fs::remove_file(&hydra_file)
            .with_context(|| format!("can't delete {}", hydra_file.display()))?;
        anstream::println!("{}", style::ok(format!("removed {}", hydra_file.display())));
    }
    Ok(0)
}
