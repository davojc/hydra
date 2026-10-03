use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};
use hydra_core::secret::SecretRef;

use crate::report::from_command;

pub struct Github;

pub const TOKEN_KEY: &str = "github-token";

const MANAGED: &[&str] = &[
    "GH_CONFIG_DIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
];

fn token_ref(ctx: &Ctx) -> SecretRef {
    SecretRef {
        env: ctx.name.clone(),
        key: TOKEN_KEY.to_string(),
    }
}

impl Provider for Github {
    fn id(&self) -> &'static str {
        "github"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.github.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        MANAGED
    }

    fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
        std::fs::create_dir_all(ctx.provider_dir("gh"))?;
        Ok(())
    }

    /// gh on Windows keeps tokens under shared Credential Manager names, so hydra passes
    /// each environment's token explicitly; GH_TOKEN always wins inside gh.
    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        Ok(Contribution::new()
            .path("GH_CONFIG_DIR", &ctx.provider_dir("gh"))
            .secret("GH_TOKEN", token_ref(ctx))
            .unset("GITHUB_TOKEN")
            .unset("GH_ENTERPRISE_TOKEN")
            .unset("GITHUB_ENTERPRISE_TOKEN")
            .unset("GH_HOST"))
    }

    fn check(&self, _ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport {
        from_command("github", run, &["gh", "api", "user", "--jq", ".login"])
    }

    fn auth_command(&self, _ctx: &Ctx) -> Option<Vec<String>> {
        Some(
            ["gh", "auth", "login", "--hostname", "github.com"]
                .map(String::from)
                .to_vec(),
        )
    }

    /// `gh auth login` refuses to run while GH_TOKEN is set.
    fn auth_unset(&self) -> &'static [&'static str] {
        &["GH_TOKEN"]
    }

    fn after_auth(&self, ctx: &Ctx, run: &dyn CommandRunner) -> Result<(), ProviderError> {
        let token = run
            .output("gh", &["auth", "token"])
            .map_err(ProviderError::new)?;
        ctx.secrets
            .set(&token_ref(ctx), &token)
            .map_err(|e| ProviderError::new(e.to_string()))
    }

    fn missing_secret_fix(&self, ctx: &Ctx, _r: &SecretRef) -> Option<String> {
        Some(format!("hydra auth github {}", ctx.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeRunner, Fixture};
    use hydra_core::contribution::VarValue;
    use hydra_core::secret::SecretStore;

    #[test]
    fn contributes_config_dir_and_token_secret() {
        let f = Fixture::new("[github]\nowners = [\"acme\"]\n");
        Github.materialise(&f.ctx()).unwrap();
        let c = Github.contribute(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&f.name).join("gh");
        assert!(dir.is_dir());
        assert_eq!(
            c.vars["GH_CONFIG_DIR"],
            VarValue::Literal(dir.to_string_lossy().into_owned())
        );
        assert_eq!(
            c.vars["GH_TOKEN"],
            VarValue::Secret(SecretRef::parse_path("work/github-token").unwrap())
        );
        assert!(c.unset.contains("GITHUB_TOKEN"));
    }

    #[test]
    fn missing_token_points_at_hydra_auth() {
        let f = Fixture::new("[github]\n");
        let fix = Github.missing_secret_fix(
            &f.ctx(),
            &SecretRef::parse_path("work/github-token").unwrap(),
        );
        assert_eq!(fix.as_deref(), Some("hydra auth github work"));
    }

    #[test]
    fn auth_drops_gh_token_and_captures_new_one() {
        let f = Fixture::new("[github]\n");
        assert_eq!(
            Github.auth_command(&f.ctx()).unwrap(),
            vec!["gh", "auth", "login", "--hostname", "github.com"]
        );
        assert_eq!(Github.auth_unset(), &["GH_TOKEN"]);
        let run = FakeRunner::default().with("gh auth token", Ok("gho_example"));
        Github.after_auth(&f.ctx(), &run).unwrap();
        assert_eq!(
            f.store
                .get(&SecretRef::parse_path("work/github-token").unwrap())
                .unwrap()
                .as_deref(),
            Some("gho_example")
        );
    }
}
