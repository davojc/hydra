//! One module per identity type.
use hydra_core::provider::Provider;

pub mod aws;
pub mod dir;
pub mod envvars;
pub mod gws;
pub mod gemini;
pub mod kube;
pub mod report;
#[cfg(test)]
pub(crate) mod testutil;

/// Every provider, in the order their variables are merged.
pub fn all() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(aws::Aws),
        Box::new(dir::AZURE),
        Box::new(dir::GCLOUD),
        Box::new(gws::Gws),
        Box::new(kube::Kube),
        Box::new(dir::CODEX),
        Box::new(gemini::Gemini),
        Box::new(envvars::EnvVars),
    ]
}

/// Looks a provider up by id; `gh` is accepted for `github`.
pub fn by_id(id: &str) -> Option<Box<dyn Provider>> {
    let id = if id == "gh" { "github" } else { id };
    all().into_iter().find(|p| p.id() == id)
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_ids_are_unique() {
        let mut ids: Vec<_> = super::all().iter().map(|p| p.id()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }
}
