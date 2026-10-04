use anyhow::Context;
use hydra_core::config::EnvConfig;
use hydra_core::envedit::{self, AddOutcome, TOOLS, ToolValues};
use hydra_core::lock;
use hydra_core::name::EnvName;
use hydra_core::provider::Ctx;
use hydra_core::secret::SecretRef;

use crate::app::App;
use crate::style;

/// The raw `hydra add` flags, before secret names are turned into references.
pub struct Flags {
    pub mcp_exclude: Vec<String>,
    pub owners: Vec<String>,
    pub strict: bool,
    pub name: Option<String>,
    pub email: Option<String>,
    pub ssh_key: Option<String>,
    pub signing_key: Option<String>,
    pub profile: Option<String>,
    pub isolate: bool,
    pub credentials_secret: Option<String>,
    pub kube_config: Option<String>,
    pub api_key_secret: Option<String>,
}

fn secret_value(flag: &str, path: Option<String>) -> anyhow::Result<Option<String>> {
    path.map(|p| {
        SecretRef::parse_path(&p)
            .map(|r| r.to_value())
            .map_err(|e| anyhow::anyhow!("{flag}: {e}"))
    })
    .transpose()
}

impl Flags {
    fn into_values(self) -> anyhow::Result<ToolValues> {
        Ok(ToolValues {
            mcp_exclude: self.mcp_exclude,
            owners: self.owners,
            strict: self.strict.then_some(true),
            name: self.name,
            email: self.email,
            ssh_key: self.ssh_key,
            signing_key: self.signing_key,
            profile: self.profile,
            isolate: self.isolate.then_some(true),
            credentials: secret_value("--credentials-secret", self.credentials_secret)?,
            kube_config: self.kube_config,
            api_key: secret_value("--api-key-secret", self.api_key_secret)?,
        })
    }
}

/// The given environment, else `HYDRA_ENV`.
fn env_arg(env: Option<String>) -> Option<String> {
    env.or_else(|| std::env::var("HYDRA_ENV").ok().filter(|s| !s.is_empty()))
}

fn require_env(app: &App, verb: &str, tool: &str, env: Option<String>) -> anyhow::Result<EnvName> {
    let given =
        env_arg(env).with_context(|| format!("name an environment: hydra {verb} {tool} <env>"))?;
    app.env_name(&given)
}

fn read_env_file(app: &App, env: &EnvName) -> anyhow::Result<String> {
    let path = app.paths.env_file(env);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("can't read {}", path.display()))?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

fn write_env_file(app: &App, env: &EnvName, text: &str) -> anyhow::Result<()> {
    let file = app.paths.env_file(env);
    std::fs::write(&file, text).with_context(|| format!("can't write {}", file.display()))
}

/// Parses and validates the edited text; nothing is written if either fails.
fn check(app: &App, env: &EnvName, text: &str) -> anyhow::Result<EnvConfig> {
    let path = app.paths.env_file(env);
    let cfg: EnvConfig =
        toml::from_str(text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    cfg.validate()
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(cfg)
}

fn note_if_running(app: &App, env: &EnvName) -> anyhow::Result<()> {
    if lock::is_running(&app.paths, env)? {
        anstream::println!(
            "{}",
            style::warn(format!(
                "note: {env} terminals already open won't see this until you reopen them"
            ))
        );
    }
    Ok(())
}

pub fn add(
    app: &App,
    tool: Option<String>,
    env: Option<String>,
    flags: Flags,
) -> anyhow::Result<i32> {
    let Some(tool) = tool else {
        return list(app, env);
    };
    let tool = envedit::tool_name(&tool).map_err(anyhow::Error::msg)?;
    let env = require_env(app, "add", tool, env)?;
    let values = flags.into_values()?;
    let text = read_env_file(app, &env)?;
    let (new_text, outcome) =
        envedit::add_tool(&text, tool, &values).map_err(anyhow::Error::msg)?;
    let cfg = check(app, &env, &new_text)?;
    let (verb, prep) = match outcome {
        AddOutcome::AlreadyPresent => {
            anstream::println!("{tool} is already on in {env}");
            return Ok(0);
        }
        AddOutcome::Added => ("added", "to"),
        AddOutcome::Updated => ("updated", "in"),
    };
    write_env_file(app, &env, &new_text)?;
    anstream::println!("{}", style::ok(format!("{verb} {tool} {prep} {env}")));
    let mut next = format!("next: open a new {env} terminal (hydra shell {env})");
    if has_sign_in(app, &env, &cfg, tool) {
        next.push_str(" and sign in there");
    }
    anstream::println!("{}", style::dim(next));
    note_if_running(app, &env)?;
    Ok(0)
}

/// Whether the tool has a command to sign in with (only asks; nothing is run).
fn has_sign_in(app: &App, env: &EnvName, cfg: &EnvConfig, tool: &str) -> bool {
    let Some(p) = hydra_providers::by_id(tool) else {
        return false;
    };
    let ctx = Ctx {
        name: env,
        env: cfg,
        paths: &app.paths,
        user_home: &app.user_home,
        secrets: app.store.as_ref(),
    };
    p.auth_command(&ctx).is_some()
}

fn list(app: &App, env: Option<String>) -> anyhow::Result<i32> {
    let on = match env_arg(env) {
        Some(given) => {
            let env = app.env_name(&given)?;
            Some(envedit::tools_in(&read_env_file(app, &env)?).map_err(anyhow::Error::msg)?)
        }
        None => None,
    };
    for t in TOOLS {
        if on.as_ref().is_some_and(|on| on.contains(t)) {
            anstream::println!("{t:<8} on");
        } else {
            anstream::println!("{t}");
        }
    }
    Ok(0)
}

pub fn remove(app: &App, tool: String, env: Option<String>) -> anyhow::Result<i32> {
    let tool = envedit::tool_name(&tool).map_err(anyhow::Error::msg)?;
    let env = require_env(app, "remove", tool, env)?;
    let text = read_env_file(app, &env)?;
    let (new_text, removed) = envedit::remove_tool(&text, tool).map_err(anyhow::Error::msg)?;
    if !removed {
        anstream::println!("{tool} isn't on in {env}");
        return Ok(0);
    }
    check(app, &env, &new_text)?;
    write_env_file(app, &env, &new_text)?;
    let folder = if tool == "github" { "gh" } else { tool };
    let state = app.paths.state_dir(&env).join(folder);
    anstream::println!(
        "{}",
        style::ok(format!(
            "removed {tool} from {env}; saved logins stay in {}",
            state.display()
        ))
    );
    note_if_running(app, &env)?;
    Ok(0)
}
