use anyhow::Context;
use hydra_core::config::load_env;
use hydra_core::provider::Ctx;
use hydra_core::resolve::PrepareOptions;
use hydra_platform::process::EnvRunner;

use crate::app::App;
use crate::commands::launch;
use crate::style;

pub fn run(app: &App, provider: String, env: String) -> anyhow::Result<i32> {
    let name = app.env_name(&env)?;
    let p = hydra_providers::by_id(&provider)
        .with_context(|| format!("unknown provider {provider:?}"))?;
    let config = load_env(&app.paths, &name)?;
    anyhow::ensure!(
        p.is_configured(&config),
        "{id} isn't configured for {name}; add a [{id}] section to {}",
        app.paths.env_file(&name).display(),
        id = p.id()
    );
    let opts = PrepareOptions {
        allow_missing_secrets: true,
        drop_vars: p.auth_unset().iter().map(|s| s.to_string()).collect(),
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
    // A borrowed tool signs in to (and keeps its secrets in) the owner's environment.
    let owner = launch.config.borrowed.get(p.id());
    let argv = p.auth_command(&ctx).with_context(|| {
        format!(
            "{} has no sign-in command; store its secret with hydra secret set {}/<key>",
            p.id(),
            owner.unwrap_or(&name)
        )
    })?;
    let runner = EnvRunner { launch: &launch };
    let mut cmd = runner.command(&argv[0]).map_err(anyhow::Error::msg)?;
    cmd.args(&argv[1..]);
    launch::ignore_ctrl_c();
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("{} exited with {status}; nothing was saved", argv.join(" "));
    }
    p.after_auth(&ctx, &runner)
        .map_err(|e| anyhow::anyhow!("{}", e.message))?;
    let tail = owner
        .map(|o| format!(" (borrowed from {o} - this updates {o}'s login)"))
        .unwrap_or_default();
    anstream::println!(
        "{}",
        style::ok(format!("signed in: {} for {name}{tail}", p.id()))
    );
    Ok(0)
}
