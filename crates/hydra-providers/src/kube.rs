use std::path::PathBuf;

use hydra_core::config::EnvConfig;
use hydra_core::contribution::Contribution;
use hydra_core::paths::expand_tilde;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError};

use crate::report::from_command;

pub struct Kube;

fn kubeconfig(ctx: &Ctx) -> PathBuf {
    match ctx.env.kube.as_ref().and_then(|k| k.config.as_deref()) {
        Some(p) => expand_tilde(p, ctx.user_home),
        None => ctx.provider_dir("kube").join("config"),
    }
}

impl Provider for Kube {
    fn id(&self) -> &'static str {
        "kube"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.kube.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        &["KUBECONFIG"]
    }

    fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
        if let Some(dir) = kubeconfig(ctx).parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        Ok(Contribution::new().path("KUBECONFIG", &kubeconfig(ctx)))
    }

    fn check(&self, _ctx: &Ctx, run: &dyn CommandRunner) -> IdentityReport {
        from_command("kube", run, &["kubectl", "config", "current-context"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;
    use hydra_core::contribution::VarValue;

    #[test]
    fn default_kubeconfig_lives_in_state() {
        let f = Fixture::new("[kube]\n");
        Kube.materialise(&f.ctx()).unwrap();
        let expected = f.paths.state_dir(&f.name).join("kube").join("config");
        assert!(expected.parent().unwrap().is_dir());
        assert_eq!(Kube.contribute(&f.ctx()).unwrap().vars["KUBECONFIG"], VarValue::Literal(expected.to_string_lossy().into_owned()));
    }

    #[test]
    fn custom_kubeconfig_expands_tilde() {
        let f = Fixture::new("[kube]\nconfig = \"~/.kube/work\"\n");
        let expected = f.home.join(".kube/work");
        assert_eq!(Kube.contribute(&f.ctx()).unwrap().vars["KUBECONFIG"], VarValue::Literal(expected.to_string_lossy().into_owned()));
    }
}
