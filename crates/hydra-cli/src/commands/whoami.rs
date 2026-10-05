use anyhow::Context;
use hydra_core::bindings;
use hydra_core::config::load_global;
use hydra_core::provider::{Ctx, IdentityReport, Status};
use hydra_core::resolve::PrepareOptions;
use hydra_platform::process::EnvRunner;

use crate::app::App;
use crate::commands::launch;
use crate::style;

pub fn run(app: &App, env: Option<String>) -> anyhow::Result<i32> {
    let cwd = std::env::current_dir().context("can't find the current folder")?;
    let rules = match load_global(&app.paths) {
        Ok(g) => g.bindings,
        Err(e) => {
            anstream::eprintln!(
                "{}",
                style::warn(format!(
                    "hydra: warning: couldn't read config.toml ({e}) - folder bindings ignored"
                ))
            );
            Default::default()
        }
    };
    let binding = bindings::resolve(&cwd, &rules);
    let env = env
        .or_else(|| std::env::var("HYDRA_ENV").ok())
        .or_else(|| binding.as_ref().map(|b| b.env.clone()))
        .context("not in a hydra terminal or a bound folder; use hydra whoami --env <name>")?;
    let name = app.env_name(&env)?;
    let opts = PrepareOptions {
        allow_missing_secrets: true,
        ..Default::default()
    };
    let launch = launch::prepare_launch(app, &name, &opts)?;
    let ctx = Ctx {
        name: &name,
        env: &launch.config,
        paths: &app.paths,
        user_home: &app.user_home,
        secrets: app.store.as_ref(),
    };
    let runner = EnvRunner { launch: &launch };

    anstream::println!(
        "{:<8} {} · {}",
        "env",
        style::env_name(&name, launch.config.rgb().map(|c| (c.0, c.1, c.2))),
        launch.config.label_or(&name)
    );
    let mut rows: Vec<IdentityReport> = hydra_providers::all()
        .iter()
        .filter(|p| p.is_configured(&launch.config))
        .map(|p| {
            let mut r = p.check(&ctx, &runner);
            if let Some(owner) = launch.config.borrowed.get(p.id()) {
                r.detail = format!("from {owner} · {}", r.detail);
            }
            r
        })
        .collect();
    group_google(&mut rows);

    let mut problems = 0;
    for r in &rows {
        let mark = match r.status {
            Status::Ok => style::ok("ok"),
            Status::Info => String::new(),
            Status::Mismatch => {
                problems += 1;
                style::error("MISMATCH")
            }
            Status::Missing => {
                problems += 1;
                style::warn("missing")
            }
        };
        anstream::println!("{:<8} {:<48} {}", r.provider, r.detail, mark);
    }
    if own_hooks_path(&cwd) {
        anstream::eprintln!(
            "{}",
            style::warn(
                "note: this repo sets its own core.hooksPath, so hydra's git guard is off here"
            )
        );
    }
    match &binding {
        Some(b) => {
            let mark = if b.env == name.as_str() {
                style::ok("ok")
            } else {
                problems += 1;
                style::error("MISMATCH")
            };
            anstream::println!(
                "{:<8} {} -> {} ({}) {}",
                "folder",
                cwd.display(),
                b.env,
                b.describe(),
                mark
            );
        }
        None => anstream::println!("{:<8} {} isn't bound", "folder", cwd.display()),
    }
    Ok(if problems == 0 { 0 } else { 1 })
}

/// True when the repo at `cwd` sets its own `core.hooksPath` (e.g. Husky).
fn own_hooks_path(cwd: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// gcloud and gws usually share one Google identity: show them as one line when they agree.
fn group_google(rows: &mut Vec<IdentityReport>) {
    let gcloud = rows.iter().position(|r| r.provider == "gcloud");
    let gws = rows.iter().position(|r| r.provider == "gws");
    if let (Some(a), Some(b)) = (gcloud, gws)
        && rows[a].status == Status::Ok
        && rows[b].status == Status::Ok
        && rows[a].detail == rows[b].detail
    {
        rows[a] = IdentityReport {
            provider: "google".into(),
            status: Status::Ok,
            detail: format!("{} (gcloud, gws)", rows[a].detail),
        };
        rows.remove(b);
    }
}
