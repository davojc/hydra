use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError, Status};
use hydra_core::secret::SecretRef;
use hydra_platform::fs::write_private;

use crate::report::{from_output, report};

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

fn remove_if_exists(file: &std::path::Path) -> Result<(), ProviderError> {
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
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
        let file = dir.join("credentials.json");
        // The plaintext file must not outlive the config or the secret it came from.
        let Some(r) = credentials_ref(ctx)? else {
            return remove_if_exists(&file);
        };
        match ctx
            .secrets
            .get(&r)
            .map_err(|e| ProviderError::new(e.to_string()))?
        {
            Some(v) => Ok(write_private(&file, &v)?),
            None => {
                remove_if_exists(&file)?;
                Err(ProviderError::new(format!("secret {} isn't set", r.path()))
                    .with_fix(format!("hydra secret set {}", r.path())))
            }
        }
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

    /// `gws auth status` prints JSON: when signed in, it checks the token and adds the
    /// account's email as `user`.
    fn check(&self, _ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport {
        let out = run.output("gws", &["auth", "status"]);
        let Some(status) = out
            .as_ref()
            .ok()
            .and_then(|o| serde_json::from_str::<serde_json::Value>(o).ok())
        else {
            // An error, or not JSON (an older gws): read the way other tools are.
            return from_output("gws", out);
        };
        let text = |key: &str| status.get(key).and_then(|v| v.as_str());
        if text("auth_method").is_none_or(|m| m == "none") {
            return report("gws", Status::Missing, "not signed in");
        }
        if status.get("token_valid").and_then(|v| v.as_bool()) == Some(false) {
            let why = text("token_error").unwrap_or("the token was refused");
            return report("gws", Status::Missing, format!("sign-in expired: {why}"));
        }
        report("gws", Status::Ok, text("user").unwrap_or("signed in"))
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
    use crate::testutil::{FakeRunner, Fixture};
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

    fn stale_credentials(f: &Fixture) -> std::path::PathBuf {
        let dir = f.paths.state_dir(&f.name).join("gws");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("credentials.json");
        std::fs::write(&file, "{\"old\":true}").unwrap();
        file
    }

    fn check_with(out: Result<&str, &str>) -> (Status, String) {
        let f = Fixture::new("[gws]\n");
        let run = FakeRunner::default().with("gws auth status", out);
        let r = Gws.check(&f.ctx(), &run);
        (r.status, r.detail)
    }

    #[test]
    fn check_shows_the_signed_in_account() {
        let signed_in = r#"{
  "auth_method": "oauth2",
  "credential_source": "encrypted_credentials",
  "token_valid": true,
  "user": "david@example.com",
  "scopes": ["openid"]
}"#;
        assert_eq!(
            check_with(Ok(signed_in)),
            (Status::Ok, "david@example.com".to_string())
        );
    }

    #[test]
    fn check_says_when_no_one_is_signed_in() {
        let none = r#"
{
  "auth_method": "none",
  "credential_source": "none",
  "storage": "none",
  "token_cache_exists": false
}"#;
        assert_eq!(
            check_with(Ok(none)),
            (Status::Missing, "not signed in".to_string())
        );
        let expired = r#"{"auth_method": "oauth2", "token_valid": false,
            "token_error": "invalid_grant: Token has been expired or revoked."}"#;
        assert_eq!(
            check_with(Ok(expired)),
            (
                Status::Missing,
                "sign-in expired: invalid_grant: Token has been expired or revoked.".to_string()
            )
        );
    }

    #[test]
    fn check_without_an_email_still_says_signed_in() {
        let no_user = r#"{"auth_method": "oauth2", "token_valid": true}"#;
        assert_eq!(
            check_with(Ok(no_user)),
            (Status::Ok, "signed in".to_string())
        );
        // Output that isn't JSON (an older gws) and errors pass through.
        assert_eq!(
            check_with(Ok("Logged in as a@b.c\nmore")),
            (Status::Ok, "Logged in as a@b.c".to_string())
        );
        assert_eq!(check_with(Err("gws: not found")).0, Status::Missing);
    }

    #[test]
    fn dir_mode_deletes_old_credentials_file() {
        let f = Fixture::new("[gws]\n");
        let file = stale_credentials(&f);
        Gws.materialise(&f.ctx()).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn missing_secret_deletes_old_credentials_file() {
        let f = Fixture::new("[gws]\ncredentials = \"secret:work/gws-creds\"\n");
        let file = stale_credentials(&f);
        assert!(Gws.materialise(&f.ctx()).is_err());
        assert!(!file.exists());
    }
}
