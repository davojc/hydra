use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Status};
use hydra_core::secret::{SecretRef, mask};

pub fn report(provider: &str, status: Status, detail: impl Into<String>) -> IdentityReport {
    IdentityReport {
        provider: provider.to_string(),
        status,
        detail: detail.into(),
    }
}

/// Runs an identity command; its first output line is the identity.
pub fn from_command(provider: &str, run: &dyn CommandRunner, cmd: &[&str]) -> IdentityReport {
    let Some((program, args)) = cmd.split_first() else {
        return report(provider, Status::Info, "configured");
    };
    match run.output(program, args) {
        Ok(out) if !out.is_empty() => {
            report(provider, Status::Ok, out.lines().next().unwrap_or_default())
        }
        Ok(_) => report(provider, Status::Missing, "not signed in"),
        Err(e) => report(provider, Status::Missing, e),
    }
}

/// Status and display text for a config value that may be a secret reference.
pub fn secret_detail(ctx: &Ctx, raw: &str) -> (Status, String) {
    match SecretRef::parse_value(raw) {
        Ok(Some(r)) => match ctx.secrets.get(&r) {
            Ok(Some(v)) => (Status::Ok, mask(&v)),
            Ok(None) => (Status::Missing, format!("secret {} isn't set", r.path())),
            Err(e) => (Status::Missing, e.to_string()),
        },
        Ok(None) => (Status::Info, "plain value".to_string()),
        Err(e) => (Status::Missing, e.to_string()),
    }
}
