use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::{report, secret_detail};

/// A Linear personal API key: hydraui's orchestrator reads Linear with it (orchestrator phase 2),
/// and the environment's own scripts and Claude sessions can too.
pub struct Linear;

impl Provider for Linear {
    fn id(&self) -> &'static str {
        "linear"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.linear.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        &["LINEAR_API_KEY"]
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        let Some(j) = &ctx.env.linear else {
            return Err(ProviderError::new("[linear] section missing"));
        };
        Contribution::new()
            .value("LINEAR_API_KEY", &j.api_key)
            .map_err(|e| ProviderError::new(e.to_string()))
    }

    fn check(&self, ctx: &Ctx, _run: &dyn CommandRunner) -> IdentityReport {
        let raw = ctx
            .env
            .linear
            .as_ref()
            .map(|j| j.api_key.as_str())
            .unwrap_or_default();
        let (status, detail) = secret_detail(ctx, raw);
        report("linear", status, format!("LINEAR_API_KEY {detail}"))
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
        let f = Fixture::new("[linear]\napi_key = \"secret:work/linear\"\n");
        let c = Linear.contribute(&f.ctx()).unwrap();
        assert_eq!(
            c.vars["LINEAR_API_KEY"],
            VarValue::Secret(SecretRef::parse_path("work/linear").unwrap())
        );
    }

    #[test]
    fn check_masks_the_key() {
        let f = Fixture::new("[linear]\napi_key = \"secret:work/linear\"\n");
        f.store
            .set(
                &SecretRef::parse_path("work/linear").unwrap(),
                "lin_api_example9876",
            )
            .unwrap();
        let r = Linear.check(&f.ctx(), &FakeRunner::default());
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.detail, "LINEAR_API_KEY \u{b7}\u{b7}\u{b7}\u{b7}9876");
    }
}
