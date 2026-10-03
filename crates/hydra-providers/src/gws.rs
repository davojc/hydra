use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};
use hydra_core::secret::SecretRef;
use hydra_platform::fs::write_private;

use crate::report::from_command;

pub struct Gws;

const CONFIG_DIR: &str = "GOOGLE_WORKSPACE_CLI_CONFIG_DIR";
const CREDENTIALS_FILE: &str = "GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE";
const TOKEN: &str = "GOOGLE_WORKSPACE_CLI_TOKEN";

fn credentials_ref(ctx: &Ctx) -> Result<Option<SecretRef>, ProviderError> {
    match ctx.env.gws.as_ref().and_then(|g| g.credentials.as_deref()) {
        None => Ok(None),
        Some(raw) => SecretRef::parse_value(raw).map_err(|e| ProviderError::new(e.to_string())),
    }
}

impl Provider for Gws {
    fn id(&self) -> &'static str {
        "gws"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.gws.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        &[CONFIG_DIR, CREDENTIALS_FILE, TOKEN]
    }

    fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
        let dir = ctx.provider_dir("gws");
        std::fs::create_dir_all(&dir)?;
        if let Some(r) = credentials_ref(ctx)? {
            let value = ctx
                .secrets
                .get(&r)
                .map_err(|e| ProviderError::new(e.to_string()))?
                .ok_or_else(|| {
                    ProviderError::new(format!("secret {} isn't set", r.path()))
                        .with_fix(format!("hydra secret set {}", r.path()))
                })?;
            write_private(&dir.join("credentials.json"), &value)?;
        }
        Ok(())
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        let dir = ctx.provider_dir("gws");
        let c = Contribution::new().path(CONFIG_DIR, &dir).unset(TOKEN);
        Ok(if credentials_ref(ctx)?.is_some() {
            c.path(CREDENTIALS_FILE, &dir.join("credentials.json"))
        } else {
            c.unset(CREDENTIALS_FILE)
        })
    }

    fn check(&self, _ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport {
        from_command("gws", run, &["gws", "auth", "status"])
    }

    fn auth_command(&self, ctx: &Ctx) -> Option<Vec<String>> {
        match credentials_ref(ctx) {
            Ok(None) => Some(vec!["gws".into(), "auth".into(), "login".into()]),
            _ => None, // credentials come from the stored secret
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;
    use hydra_core::contribution::VarValue;
    use hydra_core::secret::SecretStore;

    #[test]
    fn dir_mode() {
        let f = Fixture::new("[gws]\n");
        Gws.materialise(&f.ctx()).unwrap();
        let c = Gws.contribute(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&f.name).join("gws");
        assert_eq!(
            c.vars["GOOGLE_WORKSPACE_CLI_CONFIG_DIR"],
            VarValue::Literal(dir.to_string_lossy().into_owned())
        );
        assert!(c.unset.contains("GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE"));
        assert_eq!(
            Gws.auth_command(&f.ctx()).unwrap(),
            vec!["gws", "auth", "login"]
        );
    }

    #[test]
    fn credentials_mode_writes_file_from_secret() {
        let f = Fixture::new("[gws]\ncredentials = \"secret:work/gws-creds\"\n");
        f.store
            .set(
                &SecretRef::parse_path("work/gws-creds").unwrap(),
                "{\"type\":\"authorized_user\"}",
            )
            .unwrap();
        Gws.materialise(&f.ctx()).unwrap();
        let file = f
            .paths
            .state_dir(&f.name)
            .join("gws")
            .join("credentials.json");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"type\":\"authorized_user\"}"
        );
        let c = Gws.contribute(&f.ctx()).unwrap();
        assert_eq!(
            c.vars["GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE"],
            VarValue::Literal(file.to_string_lossy().into_owned())
        );
        assert_eq!(Gws.auth_command(&f.ctx()), None);
    }

    #[test]
    fn credentials_mode_fails_without_secret() {
        let f = Fixture::new("[gws]\ncredentials = \"secret:work/gws-creds\"\n");
        let e = Gws.materialise(&f.ctx()).unwrap_err();
        assert_eq!(e.message, "secret work/gws-creds isn't set");
        assert_eq!(e.fix.as_deref(), Some("hydra secret set work/gws-creds"));
    }
}
