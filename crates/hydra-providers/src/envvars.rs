use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError, Status};

use crate::report::{report, secret_detail};

/// Arbitrary `[env]` variables.
pub struct EnvVars;

impl Provider for EnvVars {
    fn id(&self) -> &'static str {
        "env"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        !env.env.is_empty()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        &[]
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        ctx.env
            .env
            .iter()
            .try_fold(Contribution::new(), |c, (k, v)| c.value(k, v))
            .map_err(|e| ProviderError::new(e.to_string()))
    }

    fn check(&self, ctx: &Ctx, _run: &dyn CommandRunner) -> IdentityReport {
        let mut status = Status::Info;
        let parts: Vec<String> = ctx
            .env
            .env
            .iter()
            .map(|(k, v)| {
                let (s, d) = secret_detail(ctx, v);
                if s == Status::Missing {
                    status = Status::Missing;
                } else if s == Status::Ok && status == Status::Info {
                    status = Status::Ok;
                }
                format!("{k} {d}")
            })
            .collect();
        report("env", status, parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeRunner, Fixture};
    use hydra_core::contribution::VarValue;

    #[test]
    fn literals_and_secrets() {
        let f = Fixture::new("[env]\nREGION = \"eu\"\nLINEAR_API_KEY = \"secret:work/linear\"\n");
        assert!(EnvVars.is_configured(&f.env));
        let c = EnvVars.contribute(&f.ctx()).unwrap();
        assert_eq!(c.vars["REGION"], VarValue::Literal("eu".into()));
        assert!(matches!(c.vars["LINEAR_API_KEY"], VarValue::Secret(_)));
    }

    #[test]
    fn check_reports_missing_secrets() {
        let f = Fixture::new("[env]\nLINEAR_API_KEY = \"secret:work/linear\"\n");
        let r = EnvVars.check(&f.ctx(), &FakeRunner::default());
        assert_eq!(r.status, Status::Missing);
        assert_eq!(r.detail, "LINEAR_API_KEY secret work/linear isn't set");
    }
}
