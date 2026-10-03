use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::from_command;

/// A tool that keeps all its state in one folder named by a single variable.
pub struct DirProvider {
    pub id: &'static str,
    pub var: &'static str,
    pub subdir: &'static str,
    pub configured: fn(&EnvConfig) -> bool,
    /// `var` plus any variables that would override it.
    pub managed: &'static [&'static str],
    pub login: &'static [&'static str],
    pub whoami: &'static [&'static str],
}

impl Provider for DirProvider {
    fn id(&self) -> &'static str {
        self.id
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        (self.configured)(env)
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        self.managed
    }

    fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
        std::fs::create_dir_all(ctx.provider_dir(self.subdir))?;
        Ok(())
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        let mut c = Contribution::new().path(self.var, &ctx.provider_dir(self.subdir));
        for v in self.managed.iter().filter(|v| **v != self.var) {
            c = c.unset(v);
        }
        Ok(c)
    }

    fn check(&self, _ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport {
        from_command(self.id, run, self.whoami)
    }

    fn auth_command(&self, _ctx: &Ctx) -> Option<Vec<String>> {
        (!self.login.is_empty()).then(|| self.login.iter().map(|s| s.to_string()).collect())
    }
}

fn azure_on(e: &EnvConfig) -> bool {
    e.azure.is_some()
}
fn gcloud_on(e: &EnvConfig) -> bool {
    e.gcloud.is_some()
}
fn codex_on(e: &EnvConfig) -> bool {
    e.codex.is_some()
}

pub const AZURE: DirProvider = DirProvider {
    id: "azure",
    var: "AZURE_CONFIG_DIR",
    subdir: "azure",
    configured: azure_on,
    managed: &["AZURE_CONFIG_DIR"],
    login: &["az", "login"],
    whoami: &["az", "account", "show", "--query", "user.name", "-o", "tsv"],
};

pub const GCLOUD: DirProvider = DirProvider {
    id: "gcloud",
    var: "CLOUDSDK_CONFIG",
    subdir: "gcloud",
    configured: gcloud_on,
    managed: &["CLOUDSDK_CONFIG", "CLOUDSDK_CORE_ACCOUNT", "CLOUDSDK_CORE_PROJECT"],
    login: &["gcloud", "auth", "login"],
    whoami: &["gcloud", "config", "get", "account"],
};

pub const CODEX: DirProvider = DirProvider {
    id: "codex",
    var: "CODEX_HOME",
    subdir: "codex",
    configured: codex_on,
    managed: &["CODEX_HOME"],
    login: &["codex", "login"],
    whoami: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeRunner, Fixture};
    use hydra_core::contribution::VarValue;
    use hydra_core::provider::Status;

    #[test]
    fn azure_gets_its_own_config_dir() {
        let f = Fixture::new("[azure]\n");
        assert!(AZURE.is_configured(&f.env));
        AZURE.materialise(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&f.name).join("azure");
        assert!(dir.is_dir());
        let c = AZURE.contribute(&f.ctx()).unwrap();
        assert_eq!(c.vars["AZURE_CONFIG_DIR"], VarValue::Literal(dir.to_string_lossy().into_owned()));
    }

    #[test]
    fn gcloud_clears_overriding_vars() {
        let f = Fixture::new("[gcloud]\n");
        let c = GCLOUD.contribute(&f.ctx()).unwrap();
        assert!(c.vars.contains_key("CLOUDSDK_CONFIG"));
        assert!(c.unset.contains("CLOUDSDK_CORE_ACCOUNT"));
        assert!(!c.unset.contains("CLOUDSDK_CONFIG"));
    }

    #[test]
    fn unconfigured_when_section_missing() {
        assert!(!CODEX.is_configured(&Fixture::new("").env));
    }

    #[test]
    fn check_uses_identity_command() {
        let f = Fixture::new("[gcloud]\n");
        let run = FakeRunner::default().with("gcloud config get account", Ok("work@example.com\n"));
        let r = GCLOUD.check(&f.ctx(), &run);
        assert_eq!((r.status, r.detail.as_str()), (Status::Ok, "work@example.com"));
    }

    #[test]
    fn login_commands() {
        let f = Fixture::new("[azure]\n");
        assert_eq!(AZURE.auth_command(&f.ctx()).unwrap(), vec!["az", "login"]);
    }
}
