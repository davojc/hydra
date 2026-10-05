use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{ConfigError, EnvConfig, load_env};
use crate::contribution::{VarValue, merge};
use crate::name::EnvName;
use crate::paths::HydraPaths;
use crate::provider::{Ctx, Provider};
use crate::secret::SecretStore;

/// Id of the provider for arbitrary `[env]` variables.
const ENV_PROVIDER: &str = "env";
/// Lists the `[env]` variable names a hydra launch set, `;`-separated.
pub const ENV_VARS_MARKER: &str = "HYDRA_ENV_VARS";
/// `hydra allow`'s one-command guard override.
pub const ALLOW_VAR: &str = "HYDRA_ALLOW";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub provider: String,
    pub message: String,
    pub fix: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("can't open {env}\n{}", render_failures(.failures))]
    Failed {
        env: EnvName,
        failures: Vec<Failure>,
    },
}

pub fn render_failures(failures: &[Failure]) -> String {
    let mut out = String::new();
    for f in failures {
        out.push_str(&format!("  {:<8} {}\n", f.provider, f.message));
        if let Some(fix) = &f.fix {
            out.push_str(&format!("  {:<8} -> {}\n", "", fix));
        }
    }
    out.trim_end().to_string()
}

#[derive(Debug, Clone, Default)]
pub struct PrepareOptions {
    /// `hydra auth`/`whoami`: skip secrets that aren't stored yet instead of failing.
    pub allow_missing_secrets: bool,
    /// Variables to remove even if a provider sets them (e.g. GH_TOKEN during `gh auth login`).
    pub drop_vars: Vec<String>,
    /// `HYDRA_ENV_VARS` from the parent process: `[env]` names set by an enclosing hydra
    /// environment, which must not leak into this one.
    pub inherited_env_vars: Option<String>,
    /// `hydra whoami`: this configuration (e.g. with broken borrows left out) instead of
    /// loading env.toml.
    pub config: Option<EnvConfig>,
}

/// The environment to apply to a child process.
#[derive(Clone)]
pub struct LaunchEnv {
    pub name: EnvName,
    pub config: EnvConfig,
    pub set: BTreeMap<String, String>,
    pub unset: BTreeSet<String>,
    pub path_prepend: Vec<PathBuf>,
    /// Folders taken out of the inherited PATH (compared case-insensitively, either separator).
    pub path_drop: Vec<PathBuf>,
}

/// Shows the names of the variables it sets, never their values (some are secrets).
impl std::fmt::Debug for LaunchEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchEnv")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("set", &self.set.keys().collect::<Vec<_>>())
            .field("unset", &self.unset)
            .field("path_prepend", &self.path_prepend)
            .field("path_drop", &self.path_drop)
            .finish()
    }
}

impl LaunchEnv {
    pub fn apply(&self, cmd: &mut Command) {
        for k in &self.unset {
            cmd.env_remove(k);
        }
        for (k, v) in &self.set {
            cmd.env(k, v);
        }
        cmd.env("PATH", self.path_value(std::env::var_os("PATH")));
    }

    pub fn path_value(&self, current: Option<OsString>) -> OsString {
        let mut parts = self.path_prepend.clone();
        if let Some(cur) = current {
            let drop: Vec<String> = self.path_drop.iter().map(|d| path_key(d)).collect();
            parts.extend(std::env::split_paths(&cur).filter(|p| !drop.contains(&path_key(p))));
        }
        std::env::join_paths(parts).unwrap_or_default()
    }
}

/// A PATH entry for comparison: lowercase, `\` separators, no trailing separator.
fn path_key(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Materialises every configured provider and builds the environment. Fails closed:
/// either everything succeeds or every failure is returned.
pub fn prepare(
    paths: &HydraPaths,
    name: &EnvName,
    user_home: &Path,
    providers: &[Box<dyn Provider>],
    secrets: &dyn SecretStore,
    opts: &PrepareOptions,
) -> Result<LaunchEnv, PrepareError> {
    let config = match &opts.config {
        Some(c) => c.clone(),
        None => load_env(paths, name)?,
    };
    let ctx = Ctx {
        name,
        env: &config,
        paths,
        user_home,
        secrets,
    };
    let mut failures = Vec::new();
    let mut parts = Vec::new();
    let mut unset = BTreeSet::new();

    for p in providers {
        if !p.is_configured(&config) {
            unset.extend(p.managed_vars().iter().map(|v| v.to_string()));
            continue;
        }
        match p.materialise(&ctx).and_then(|()| p.contribute(&ctx)) {
            Ok(c) => parts.push((p.id().to_string(), c)),
            Err(e) => failures.push(Failure {
                provider: p.id().into(),
                message: e.message,
                fix: e.fix,
            }),
        }
    }

    let merged = match merge(parts) {
        Ok(m) => m,
        Err(conflicts) => {
            for c in conflicts {
                failures.push(Failure {
                    provider: c.second.clone(),
                    message: c.to_string(),
                    fix: Some(format!(
                        "remove {} from one of them in {}",
                        c.var,
                        paths.env_file(name).display()
                    )),
                });
            }
            return Err(PrepareError::Failed {
                env: name.clone(),
                failures,
            });
        }
    };

    let mut set = BTreeMap::new();
    for (var, (provider, value)) in &merged.vars {
        match value {
            VarValue::Literal(v) => {
                set.insert(var.clone(), v.clone());
            }
            VarValue::Secret(r) => match secrets.get(r) {
                Ok(Some(v)) => {
                    set.insert(var.clone(), v);
                }
                Ok(None) if opts.allow_missing_secrets => {
                    // Don't let a value inherited from the parent shell stand in for it.
                    unset.insert(var.clone());
                }
                Ok(None) => {
                    let fix = providers
                        .iter()
                        .find(|p| p.id() == provider)
                        .and_then(|p| p.missing_secret_fix(&ctx, r))
                        .unwrap_or_else(|| format!("hydra secret set {}", r.path()));
                    failures.push(Failure {
                        provider: provider.clone(),
                        message: format!("secret {} isn't set", r.path()),
                        fix: Some(fix),
                    });
                }
                Err(e) => failures.push(Failure {
                    provider: provider.clone(),
                    message: e.to_string(),
                    fix: None,
                }),
            },
        }
    }
    if !failures.is_empty() {
        return Err(PrepareError::Failed {
            env: name.clone(),
            failures,
        });
    }

    set.insert("HYDRA_ENV".into(), name.to_string());
    set.insert(
        "HYDRA_HOME".into(),
        paths.root().to_string_lossy().into_owned(),
    );
    match &config.color {
        Some(c) => {
            set.insert("HYDRA_ENV_COLOR".into(), c.clone());
        }
        None => {
            unset.insert("HYDRA_ENV_COLOR".into());
        }
    }
    // Names (never values) of the [env] variables, so a nested hydra launch can remove them.
    let env_vars: Vec<&str> = merged
        .vars
        .iter()
        .filter(|(_, (provider, _))| provider == ENV_PROVIDER)
        .map(|(k, _)| k.as_str())
        .collect();
    if env_vars.is_empty() {
        unset.insert(ENV_VARS_MARKER.into());
    } else {
        set.insert(ENV_VARS_MARKER.into(), env_vars.join(";"));
    }
    if let Some(inherited) = &opts.inherited_env_vars {
        unset.extend(
            inherited
                .split(';')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string),
        );
    }
    for v in &opts.drop_vars {
        set.retain(|k, _| !k.eq_ignore_ascii_case(v));
        unset.insert(v.clone());
    }
    unset.extend(merged.unset.iter().cloned());
    unset.retain(|u| !set.keys().any(|k| k.eq_ignore_ascii_case(u)));
    // `hydra allow -- pwsh` must not switch the guard off for a whole session: the one-off
    // override never reaches what a launch starts. (`hydra allow` sets it on its own child.)
    unset.insert(ALLOW_VAR.into());

    Ok(LaunchEnv {
        name: name.clone(),
        config,
        set,
        unset,
        path_prepend: merged.path_prepend,
        // A terminal opened by an older hydra may still have its shims folder first.
        path_drop: vec![paths.legacy_shims_dir()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contribution::Contribution;
    use crate::provider::{CommandRunner, IdentityReport, ProviderError, Status};
    use crate::secret::{MemoryStore, SecretRef};

    struct Fake {
        id: &'static str,
        on: bool,
        managed: &'static [&'static str],
        contribution: Contribution,
        fail: Option<&'static str>,
    }

    impl Provider for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn is_configured(&self, _: &EnvConfig) -> bool {
            self.on
        }
        fn managed_vars(&self) -> &'static [&'static str] {
            self.managed
        }
        fn contribute(&self, _: &Ctx) -> Result<Contribution, ProviderError> {
            match self.fail {
                Some(m) => Err(ProviderError::new(m).with_fix("fix it")),
                None => Ok(self.contribution.clone()),
            }
        }
        fn check(&self, _: &Ctx, _: &dyn CommandRunner) -> IdentityReport {
            IdentityReport {
                provider: self.id.into(),
                status: Status::Info,
                detail: String::new(),
            }
        }
    }

    fn fake(id: &'static str, contribution: Contribution) -> Box<dyn Provider> {
        Box::new(Fake {
            id,
            on: true,
            managed: &[],
            contribution,
            fail: None,
        })
    }

    fn setup(env_toml: &str) -> (tempfile::TempDir, HydraPaths, EnvName) {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path().join(".hydra"));
        let name = EnvName::parse("work").unwrap();
        std::fs::create_dir_all(paths.env_dir(&name)).unwrap();
        std::fs::write(paths.env_file(&name), env_toml).unwrap();
        (dir, paths, name)
    }

    fn run(
        paths: &HydraPaths,
        name: &EnvName,
        providers: &[Box<dyn Provider>],
        store: &MemoryStore,
        opts: &PrepareOptions,
    ) -> Result<LaunchEnv, PrepareError> {
        prepare(paths, name, Path::new("/home/me"), providers, store, opts)
    }

    #[test]
    fn launch_unsets_the_one_off_guard_override() {
        let (_d, paths, name) = setup("");
        let env = run(
            &paths,
            &name,
            &[],
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        assert!(env.unset.contains(ALLOW_VAR), "{:?}", env.unset);
    }

    #[test]
    fn sets_configured_vars_and_hydra_markers() {
        let (_d, paths, name) = setup("color = \"#1f9a8a\"\n");
        let providers = vec![fake(
            "aws",
            Contribution::new().literal("AWS_PROFILE", "dev"),
        )];
        let env = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        assert_eq!(env.set["AWS_PROFILE"], "dev");
        assert_eq!(env.set["HYDRA_ENV"], "work");
        assert_eq!(env.set["HYDRA_ENV_COLOR"], "#1f9a8a");
        assert_eq!(env.set["HYDRA_HOME"], paths.root().to_string_lossy());
    }

    #[test]
    fn clears_vars_of_unconfigured_providers() {
        let (_d, paths, name) = setup("");
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(Fake {
            id: "aws",
            on: false,
            managed: &["AWS_PROFILE"],
            contribution: Contribution::new(),
            fail: None,
        })];
        let env = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        assert!(env.unset.contains("AWS_PROFILE"));
    }

    #[test]
    fn resolves_secrets() {
        let (_d, paths, name) = setup("");
        let store = MemoryStore::default();
        let r = SecretRef::parse_path("work/linear").unwrap();
        store.set(&r, "lin_123").unwrap();
        let providers = vec![fake("env", Contribution::new().secret("LINEAR_API_KEY", r))];
        let env = run(
            &paths,
            &name,
            &providers,
            &store,
            &PrepareOptions::default(),
        )
        .unwrap();
        assert_eq!(env.set["LINEAR_API_KEY"], "lin_123");
    }

    #[test]
    fn missing_secret_fails_closed_with_fix() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake(
            "env",
            Contribution::new().secret("T", SecretRef::parse_path("work/token").unwrap()),
        )];
        let err = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("can't open work"), "{msg}");
        assert!(msg.contains("secret work/token isn't set"), "{msg}");
        assert!(msg.contains("hydra secret set work/token"), "{msg}");
    }

    #[test]
    fn reports_every_failing_provider() {
        let (_d, paths, name) = setup("");
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(Fake {
                id: "github",
                on: true,
                managed: &[],
                contribution: Contribution::new(),
                fail: Some("no token"),
            }),
            Box::new(Fake {
                id: "git",
                on: true,
                managed: &[],
                contribution: Contribution::new(),
                fail: Some("no key"),
            }),
        ];
        let PrepareError::Failed { failures, .. } = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap_err() else {
            panic!("expected Failed")
        };
        let ids: Vec<_> = failures.iter().map(|f| f.provider.as_str()).collect();
        assert_eq!(ids, vec!["github", "git"]);
        assert_eq!(failures[0].fix.as_deref(), Some("fix it"));
    }

    #[test]
    fn auth_mode_skips_missing_secrets_and_drops_vars() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake(
            "github",
            Contribution::new().literal("GH_CONFIG_DIR", "d").secret(
                "GH_TOKEN",
                SecretRef::parse_path("work/github-token").unwrap(),
            ),
        )];
        let opts = PrepareOptions {
            allow_missing_secrets: true,
            drop_vars: vec!["GH_CONFIG_DIR".into()],
            ..Default::default()
        };
        let env = run(&paths, &name, &providers, &MemoryStore::default(), &opts).unwrap();
        assert!(!env.set.contains_key("GH_TOKEN"));
        assert!(!env.set.contains_key("GH_CONFIG_DIR"));
        assert!(env.unset.contains("GH_CONFIG_DIR"));
    }

    #[test]
    fn auth_mode_unsets_skipped_secret_vars() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake(
            "github",
            Contribution::new().secret(
                "GH_TOKEN",
                SecretRef::parse_path("work/github-token").unwrap(),
            ),
        )];
        let opts = PrepareOptions {
            allow_missing_secrets: true,
            ..Default::default()
        };
        let env = run(&paths, &name, &providers, &MemoryStore::default(), &opts).unwrap();
        assert!(env.unset.contains("GH_TOKEN"));
        assert!(!env.set.contains_key("GH_TOKEN"));
    }

    #[test]
    fn conflicts_fail() {
        let (_d, paths, name) = setup("");
        let providers = vec![
            fake("a", Contribution::new().literal("X", "1")),
            fake("b", Contribution::new().literal("X", "2")),
        ];
        let msg = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("X is set by both a and b"), "{msg}");
    }

    #[test]
    fn unknown_environment() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        let err = run(
            &paths,
            &EnvName::parse("nope").unwrap(),
            &[],
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PrepareError::Config(ConfigError::UnknownEnv(_))
        ));
    }

    #[test]
    fn path_value_prepends() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake(
            "x",
            Contribution::new().prepend(PathBuf::from("C:/tools")),
        )];
        let env = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        let joined = env.path_value(Some(OsString::from("C:/bin")));
        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            parts,
            vec![PathBuf::from("C:/tools"), PathBuf::from("C:/bin")]
        );
    }

    #[test]
    fn path_value_drops_the_old_shims_folder() {
        let (_d, paths, name) = setup("");
        let env = run(
            &paths,
            &name,
            &[],
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        let shims = paths.legacy_shims_dir().to_string_lossy().into_owned();
        let shouty = format!("{}\\", shims.replace('\\', "/").to_uppercase());
        let current = std::env::join_paths([
            PathBuf::from(&shims),
            PathBuf::from("C:/bin"),
            PathBuf::from(&shouty),
            PathBuf::from("C:/shims"),
        ])
        .unwrap();
        let parts: Vec<PathBuf> = std::env::split_paths(&env.path_value(Some(current))).collect();
        assert_eq!(
            parts,
            vec![PathBuf::from("C:/bin"), PathBuf::from("C:/shims")]
        );
    }

    #[test]
    fn exports_env_var_names_for_nested_launches() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake(
            "env",
            Contribution::new()
                .literal("REGION", "eu")
                .literal("LINEAR_API_KEY", "lin_123"),
        )];
        let env = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        assert_eq!(env.set["HYDRA_ENV_VARS"], "LINEAR_API_KEY;REGION");
    }

    #[test]
    fn nested_launch_unsets_the_outer_environments_vars() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake("env", Contribution::new().literal("REGION", "us"))];
        let opts = PrepareOptions {
            inherited_env_vars: Some("LINEAR_API_KEY;region;".into()),
            ..Default::default()
        };
        let env = run(&paths, &name, &providers, &MemoryStore::default(), &opts).unwrap();
        assert!(env.unset.contains("LINEAR_API_KEY"), "{:?}", env.unset);
        assert!(!env.unset.iter().any(|u| u.eq_ignore_ascii_case("REGION")));
        assert!(!env.unset.contains(""));
        assert_eq!(env.set["REGION"], "us");
        assert_eq!(env.set["HYDRA_ENV_VARS"], "REGION");
    }

    #[test]
    fn without_colour_or_env_vars_the_markers_are_unset() {
        let (_d, paths, name) = setup("");
        let opts = PrepareOptions {
            inherited_env_vars: Some("LINEAR_API_KEY".into()),
            ..Default::default()
        };
        let env = run(&paths, &name, &[], &MemoryStore::default(), &opts).unwrap();
        assert!(env.unset.contains("HYDRA_ENV_COLOR"));
        assert!(env.unset.contains("HYDRA_ENV_VARS"));
        assert!(env.unset.contains("LINEAR_API_KEY"));
        assert!(!env.set.contains_key("HYDRA_ENV_VARS"));
    }

    #[test]
    fn debug_output_redacts_values() {
        let (_d, paths, name) = setup("");
        let providers = vec![fake(
            "env",
            Contribution::new().literal("TOKEN", "s3cret-v4lue"),
        )];
        let env = run(
            &paths,
            &name,
            &providers,
            &MemoryStore::default(),
            &PrepareOptions::default(),
        )
        .unwrap();
        let shown = format!("{env:?}");
        assert!(shown.contains("TOKEN"), "{shown}");
        assert!(!shown.contains("s3cret-v4lue"), "{shown}");
    }
}
