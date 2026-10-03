use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::from_command;

pub struct Github;

const MANAGED: &[&str] = &[
    "GH_CONFIG_DIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
];

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

    /// The environment's gh login lives in its own GH_CONFIG_DIR (gh keeps the token in the
    /// Credential Manager, keyed by account). Inherited tokens are cleared because they would
    /// override that login.
    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        Ok(Contribution::new()
            .path("GH_CONFIG_DIR", &ctx.provider_dir("gh"))
            .unset("GH_TOKEN")
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

    fn sign_in_hint(&self, ctx: &Ctx) -> Option<String> {
        let hosts = std::fs::read_to_string(ctx.provider_dir("gh").join("hosts.yml")).ok();
        match hosts {
            Some(text) if text.contains("github.com") => None,
            _ => Some("not signed in - run gh auth login".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;
    use hydra_core::contribution::VarValue;

    #[test]
    fn contributes_config_dir_and_clears_inherited_tokens() {
        let f = Fixture::new(
            "[github]
owners = [\"acme\"]
",
        );
        Github.materialise(&f.ctx()).unwrap();
        let c = Github.contribute(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&f.name).join("gh");
        assert!(dir.is_dir());
        assert_eq!(
            c.vars["GH_CONFIG_DIR"],
            VarValue::Literal(dir.to_string_lossy().into_owned())
        );
        assert!(!c.vars.contains_key("GH_TOKEN"));
        assert!(c.unset.contains("GH_TOKEN"));
        assert!(c.unset.contains("GITHUB_TOKEN"));
    }

    #[test]
    fn hints_at_sign_in_when_hosts_file_is_missing() {
        let f = Fixture::new(
            "[github]
",
        );
        assert_eq!(
            Github.sign_in_hint(&f.ctx()).as_deref(),
            Some("not signed in - run gh auth login")
        );
    }

    #[test]
    fn no_hint_once_hosts_file_names_github() {
        let f = Fixture::new(
            "[github]
",
        );
        Github.materialise(&f.ctx()).unwrap();
        std::fs::write(
            f.paths.state_dir(&f.name).join("gh").join("hosts.yml"),
            "github.com:
  user: x
",
        )
        .unwrap();
        assert_eq!(Github.sign_in_hint(&f.ctx()), None);
    }

    #[test]
    fn auth_command_is_gh_auth_login() {
        let f = Fixture::new(
            "[github]
",
        );
        assert_eq!(
            Github.auth_command(&f.ctx()).unwrap(),
            vec!["gh", "auth", "login", "--hostname", "github.com"]
        );
    }
}
