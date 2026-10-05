use std::path::{Path, PathBuf};

use crate::config::EnvConfig;
use crate::contribution::Contribution;
use crate::name::EnvName;
use crate::paths::HydraPaths;
use crate::secret::{SecretRef, SecretStore};

/// Everything a provider may look at while preparing one environment.
pub struct Ctx<'a> {
    pub name: &'a EnvName,
    pub env: &'a EnvConfig,
    pub paths: &'a HydraPaths,
    pub user_home: &'a Path,
    pub secrets: &'a dyn SecretStore,
}

impl Ctx<'_> {
    pub fn state_dir(&self) -> PathBuf {
        self.paths.state_dir(self.name)
    }
    /// The environment whose saved logins `tool` uses: its owner when borrowed, else this one.
    pub fn tool_owner(&self, tool: &str) -> &EnvName {
        self.env.borrowed.get(tool).unwrap_or(self.name)
    }
    /// The tool's state folder; a borrowed tool's lives in its owner's state.
    pub fn provider_dir(&self, sub: &str) -> PathBuf {
        let tool = if sub == "gh" { "github" } else { sub };
        self.paths.state_dir(self.tool_owner(tool)).join(sub)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Mismatch,
    Missing,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityReport {
    pub provider: String,
    pub status: Status,
    pub detail: String,
}

/// Runs a program with the environment applied; returns trimmed stdout, or the first stderr line.
pub trait CommandRunner {
    fn output(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub message: String,
    pub fix: Option<String>,
}

impl ProviderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fix: None,
        }
    }
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

impl From<std::io::Error> for ProviderError {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

/// One identity type (gh, git, aws, ...).
pub trait Provider {
    fn id(&self) -> &'static str;
    fn is_configured(&self, env: &EnvConfig) -> bool;
    /// Variables this provider owns. Removed from the child's environment when it isn't configured.
    fn managed_vars(&self) -> &'static [&'static str];
    /// Create or refresh files under `state/<env>/`.
    fn materialise(&self, _ctx: &Ctx) -> Result<(), ProviderError> {
        Ok(())
    }
    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError>;
    fn check(&self, ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport;
    /// The tool's own sign-in command, run inside the environment by `hydra auth`.
    fn auth_command(&self, _ctx: &Ctx) -> Option<Vec<String>> {
        None
    }
    /// Variables to drop while the sign-in command runs.
    fn auth_unset(&self) -> &'static [&'static str] {
        &[]
    }
    /// Runs after a successful sign-in command (e.g. to capture a token).
    fn after_auth(&self, _ctx: &Ctx, _run: &dyn CommandRunner) -> Result<(), ProviderError> {
        Ok(())
    }
    /// One line telling the user how to sign in, when this tool isn't signed in yet in this
    /// environment. Must only look at files (no network, no subprocesses): it runs every time a shell opens.
    fn sign_in_hint(&self, _ctx: &Ctx) -> Option<String> {
        None
    }
    /// Fix to suggest when one of this provider's secrets is missing.
    fn missing_secret_fix(&self, _ctx: &Ctx, _r: &SecretRef) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::MemoryStore;

    #[test]
    fn borrowed_tools_use_the_owners_state_folder() {
        let paths = HydraPaths::new("C:/h");
        let work = EnvName::parse("work").unwrap();
        let mut env = EnvConfig::default();
        env.borrowed
            .insert("github".into(), EnvName::parse("personal").unwrap());
        env.borrowed
            .insert("claude".into(), EnvName::parse("personal").unwrap());
        let store = MemoryStore::default();
        let ctx = Ctx {
            name: &work,
            env: &env,
            paths: &paths,
            user_home: Path::new("C:/u"),
            secrets: &store,
        };
        let state = |e: &str| paths.state_dir(&EnvName::parse(e).unwrap());
        assert_eq!(ctx.provider_dir("gh"), state("personal").join("gh"));
        assert_eq!(ctx.provider_dir("claude"), state("personal").join("claude"));
        assert_eq!(ctx.provider_dir("git"), state("work").join("git"));
        assert_eq!(ctx.provider_dir("codex"), state("work").join("codex"));
        assert_eq!(ctx.tool_owner("claude").as_str(), "personal");
        assert_eq!(ctx.tool_owner("aws").as_str(), "work");
        assert_eq!(ctx.state_dir(), state("work"));
    }
}
