use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::from_command;

pub struct Aws;

const MANAGED: &[&str] = &[
    "AWS_PROFILE",
    "AWS_DEFAULT_PROFILE",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

impl Provider for Aws {
    fn id(&self) -> &'static str {
        "aws"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.aws.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        MANAGED
    }

    fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
        if ctx.env.aws.as_ref().is_some_and(|a| a.isolate) {
            std::fs::create_dir_all(ctx.provider_dir("aws"))?;
        }
        Ok(())
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        let Some(cfg) = &ctx.env.aws else {
            return Err(ProviderError::new("[aws] section missing"));
        };
        let mut c = Contribution::new()
            .literal("AWS_PROFILE", &cfg.profile)
            .unset("AWS_DEFAULT_PROFILE")
            .unset("AWS_ACCESS_KEY_ID")
            .unset("AWS_SECRET_ACCESS_KEY")
            .unset("AWS_SESSION_TOKEN");
        if cfg.isolate {
            let dir = ctx.provider_dir("aws");
            c = c
                .path("AWS_CONFIG_FILE", &dir.join("config"))
                .path("AWS_SHARED_CREDENTIALS_FILE", &dir.join("credentials"));
        } else {
            c = c
                .unset("AWS_CONFIG_FILE")
                .unset("AWS_SHARED_CREDENTIALS_FILE");
        }
        Ok(c)
    }

    fn check(&self, _ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport {
        from_command(
            "aws",
            run,
            &[
                "aws",
                "sts",
                "get-caller-identity",
                "--query",
                "Arn",
                "--output",
                "text",
            ],
        )
    }

    fn auth_command(&self, ctx: &Ctx) -> Option<Vec<String>> {
        let profile = ctx.env.aws.as_ref()?.profile.clone();
        Some(vec![
            "aws".into(),
            "sso".into(),
            "login".into(),
            "--profile".into(),
            profile,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;
    use hydra_core::contribution::VarValue;

    #[test]
    fn sets_profile_and_clears_static_keys() {
        let f = Fixture::new("[aws]\nprofile = \"acme-dev\"\n");
        let c = Aws.contribute(&f.ctx()).unwrap();
        assert_eq!(c.vars["AWS_PROFILE"], VarValue::Literal("acme-dev".into()));
        for v in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_CONFIG_FILE",
        ] {
            assert!(c.unset.contains(v), "{v}");
        }
    }

    #[test]
    fn isolate_uses_private_files() {
        let f = Fixture::new("[aws]\nprofile = \"p\"\nisolate = true\n");
        Aws.materialise(&f.ctx()).unwrap();
        let c = Aws.contribute(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&f.name).join("aws");
        assert!(dir.is_dir());
        assert_eq!(
            c.vars["AWS_CONFIG_FILE"],
            VarValue::Literal(dir.join("config").to_string_lossy().into_owned())
        );
        assert!(c.vars.contains_key("AWS_SHARED_CREDENTIALS_FILE"));
    }

    #[test]
    fn login_uses_profile() {
        let f = Fixture::new("[aws]\nprofile = \"p\"\n");
        assert_eq!(
            Aws.auth_command(&f.ctx()).unwrap(),
            vec!["aws", "sso", "login", "--profile", "p"]
        );
    }
}
