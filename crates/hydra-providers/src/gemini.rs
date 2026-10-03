use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::{report, secret_detail};

pub struct Gemini;

impl Provider for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.gemini.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        &["GEMINI_API_KEY"]
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        let Some(g) = &ctx.env.gemini else {
            return Err(ProviderError::new("[gemini] section missing"));
        };
        Contribution::new()
            .value("GEMINI_API_KEY", &g.api_key)
            .map_err(|e| ProviderError::new(e.to_string()))
    }

    fn check(&self, ctx: &Ctx, _run: &dyn CommandRunner) -> IdentityReport {
        let raw = ctx
            .env
            .gemini
            .as_ref()
            .map(|g| g.api_key.as_str())
            .unwrap_or_default();
        let (status, detail) = secret_detail(ctx, raw);
        report("gemini", status, format!("GEMINI_API_KEY {detail}"))
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
        let f = Fixture::new("[gemini]\napi_key = \"secret:work/gemini\"\n");
        let c = Gemini.contribute(&f.ctx()).unwrap();
        assert_eq!(
            c.vars["GEMINI_API_KEY"],
            VarValue::Secret(SecretRef::parse_path("work/gemini").unwrap())
        );
    }

    #[test]
    fn check_masks_the_key() {
        let f = Fixture::new("[gemini]\napi_key = \"secret:work/gemini\"\n");
        f.store
            .set(
                &SecretRef::parse_path("work/gemini").unwrap(),
                "AIzaSyExample1234",
            )
            .unwrap();
        let r = Gemini.check(&f.ctx(), &FakeRunner::default());
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.detail, "GEMINI_API_KEY \u{b7}\u{b7}\u{b7}\u{b7}1234");
    }
}
