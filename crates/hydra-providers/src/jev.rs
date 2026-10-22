use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::{report, secret_detail};

/// TypeSafe's Jev API key, for the environment's own scripts and Claude sessions.
pub struct Jev;

impl Provider for Jev {
    fn id(&self) -> &'static str {
        "jev"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.jev.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        &["JEV_API_KEY"]
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        let Some(j) = &ctx.env.jev else {
            return Err(ProviderError::new("[jev] section missing"));
        };
        Contribution::new()
            .value("JEV_API_KEY", &j.api_key)
            .map_err(|e| ProviderError::new(e.to_string()))
    }

    fn check(&self, ctx: &Ctx, _run: &dyn CommandRunner) -> IdentityReport {
        let raw = ctx
            .env
            .jev
            .as_ref()
            .map(|j| j.api_key.as_str())
            .unwrap_or_default();
        let (status, detail) = secret_detail(ctx, raw);
        report("jev", status, format!("JEV_API_KEY {detail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeRunner, Fixture};
    use hydra_core::contribution::VarValue;
    use hydra_core::provider::Status;
    use hydra_core::secret::{SecretRef, SecretStore};

    #[test]
    fn key_comes_from_secret() {
        let f = Fixture::new("[jev]\napi_key = \"secret:work/jev\"\n");
        let c = Jev.contribute(&f.ctx()).unwrap();
        assert_eq!(
            c.vars["JEV_API_KEY"],
            VarValue::Secret(SecretRef::parse_path("work/jev").unwrap())
        );
    }

    #[test]
    fn check_masks_the_key() {
        let f = Fixture::new("[jev]\napi_key = \"secret:work/jev\"\n");
        f.store
            .set(
                &SecretRef::parse_path("work/jev").unwrap(),
                "ts-example-9876",
            )
            .unwrap();
        let r = Jev.check(&f.ctx(), &FakeRunner::default());
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.detail, "JEV_API_KEY \u{b7}\u{b7}\u{b7}\u{b7}9876");
    }
}
