use anyhow::Context;
use hydra_core::provider::{Ctx, IdentityReport, Status};
use hydra_core::resolve::PrepareOptions;
use hydra_platform::process::EnvRunner;

use crate::app::App;
use crate::commands::launch;

pub fn run(app: &App, env: Option<String>) -> anyhow::Result<i32> {
    let env = env
        .or_else(|| std::env::var("HYDRA_ENV").ok())
        .context("not in a hydra terminal; use hydra whoami --env <name>")?;
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

    println!("{:<8} {} · {}", "env", name, launch.config.label_or(&name));
    let mut rows: Vec<IdentityReport> = hydra_providers::all()
        .iter()
        .filter(|p| p.is_configured(&launch.config))
        .map(|p| p.check(&ctx, &runner))
        .collect();
    group_google(&mut rows);

    let mut problems = 0;
    for r in &rows {
        let mark = match r.status {
            Status::Ok => "ok",
            Status::Info => "",
            Status::Mismatch => {
                problems += 1;
                "MISMATCH"
            }
            Status::Missing => {
                problems += 1;
                "missing"
            }
        };
        println!("{:<8} {:<48} {}", r.provider, r.detail, mark);
    }
    Ok(if problems == 0 { 0 } else { 1 })
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
