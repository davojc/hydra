//! hydraui's link to hydra (spec §3.1, §4, §7.1): environments, `prepare` with guards off,
//! starting panes, and the identity checks for the status bar. No UI.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use hydra_core::bindings;
use hydra_core::config::{GlobalConfig, load_env, load_global};
use hydra_core::envs;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::provider::{Ctx, IdentityReport, Provider, Status, Warnings};
use hydra_core::resolve::{ENV_VARS_MARKER, LaunchEnv, PrepareError, PrepareOptions, prepare};
use hydra_platform::keyring_store::KeyringStore;
use hydra_platform::process::EnvRunner;
use hydra_platform::shell::ShellKind;
use hydraui_term::{GridSize, Pane, SpawnSpec, TermEvent};

use crate::palette::env_tint;

/// What a pane runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneRequest {
    pub env: String,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// Shell text run after the shell starts.
    pub command: Option<String>,
    /// Shown in the pane's warning strip (e.g. the folder is bound to another environment).
    pub note: Option<String>,
}

impl PaneRequest {
    /// A plain shell in `env` at `cwd`.
    pub fn new(env: impl Into<String>, cwd: PathBuf, shell: ShellKind) -> Self {
        Self {
            env: env.into(),
            cwd,
            shell,
            command: None,
            note: None,
        }
    }
}

/// A started pane, with the warnings `prepare` gave (shown as the pane's strip).
pub struct Started {
    pub pane: Pane,
    pub events: Receiver<TermEvent>,
    pub warnings: Vec<String>,
}

pub struct Hydra {
    pub paths: HydraPaths,
    pub user_home: PathBuf,
    /// hydra's config.toml: bindings, `default_shell`, `git_bash`.
    pub global: GlobalConfig,
    store: KeyringStore,
    /// `prepare` rewrites state files through fixed temp names, so two at once (a pane
    /// starting while the status bar runs whoami) take turns.
    prepare_turn: Mutex<()>,
}

impl Hydra {
    /// Finds `HYDRA_HOME` (else `~/.hydra`) and the user's home (`HYDRA_USER_HOME`, else the
    /// real one) as the CLI does. A config.toml that can't be read gives defaults plus the
    /// problem.
    pub fn from_env() -> Result<(Hydra, Option<String>), String> {
        let paths = HydraPaths::discover().map_err(|e| e.to_string())?;
        let user_home = match std::env::var_os("HYDRA_USER_HOME") {
            Some(h) => PathBuf::from(h),
            None => dirs::home_dir().ok_or("can't find your home folder")?,
        };
        let store = KeyringStore::from_env(&paths);
        Ok(Hydra::new(paths, user_home, store))
    }

    pub fn new(
        paths: HydraPaths,
        user_home: PathBuf,
        store: KeyringStore,
    ) -> (Hydra, Option<String>) {
        let (global, problem) = match load_global(&paths) {
            Ok(g) => (g, None),
            Err(e) => (
                GlobalConfig::default(),
                Some(format!(
                    "{}: {e} - folder bindings ignored",
                    paths.config_file().display()
                )),
            ),
        };
        (
            Hydra {
                paths,
                user_home,
                global,
                store,
                prepare_turn: Mutex::new(()),
            },
            problem,
        )
    }

    pub fn env_names(&self) -> Vec<String> {
        envs::list(&self.paths)
            .map(|l| l.valid.iter().map(|n| n.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn env_exists(&self, env: &str) -> bool {
        EnvName::parse(env).is_ok_and(|n| self.paths.env_file(&n).is_file())
    }

    /// The environment for a new Loose tab in `cwd`: the folder's binding, else
    /// `default_env`; `None` means ask (spec §7.6).
    pub fn adhoc_env(&self, cwd: &Path, default_env: Option<&str>) -> Option<String> {
        bindings::resolve(cwd, &self.global.bindings)
            .map(|b| b.env)
            .filter(|e| self.env_exists(e))
            .or_else(|| {
                default_env
                    .filter(|e| self.env_exists(e))
                    .map(str::to_string)
            })
    }

    /// hydraui's `default_shell`, else hydra's, else pwsh.
    pub fn shell(&self, ui_default: Option<&str>) -> ShellKind {
        ui_default
            .and_then(ShellKind::parse)
            .or_else(|| {
                self.global
                    .default_shell
                    .as_deref()
                    .and_then(ShellKind::parse)
            })
            .unwrap_or(ShellKind::Pwsh)
    }

    /// The environment's tab tint (spec §7.1).
    pub fn tint(&self, env: &str) -> u32 {
        let rgb = EnvName::parse(env)
            .ok()
            .and_then(|n| load_env(&self.paths, &n).ok())
            .and_then(|c| c.rgb())
            .map(|c| (c.0, c.1, c.2));
        env_tint(env, rgb)
    }

    /// `prepare` for a hydraui pane: guards off (spec §4.1).
    pub fn prepare(&self, env: &str) -> Result<LaunchEnv, String> {
        let name = EnvName::parse(env).map_err(|e| e.to_string())?;
        let opts = PrepareOptions {
            no_guards: true,
            inherited_env_vars: std::env::var(ENV_VARS_MARKER).ok(),
            ..Default::default()
        };
        self.prepare_in_turn(&name, &hydra_providers::all(), &opts)
            .map_err(|e| e.to_string())
    }

    fn prepare_in_turn(
        &self,
        name: &EnvName,
        providers: &[Box<dyn Provider>],
        opts: &PrepareOptions,
    ) -> Result<LaunchEnv, PrepareError> {
        let _turn = self.prepare_turn.lock().unwrap_or_else(|e| e.into_inner());
        prepare(
            &self.paths,
            name,
            &self.user_home,
            providers,
            &self.store,
            opts,
        )
    }

    /// Prepares the environment and starts the pane's shell. The error is the pane's banner
    /// text (spec §7.5).
    pub fn start(&self, req: &PaneRequest, size: GridSize) -> Result<Started, String> {
        let launch = self.prepare(&req.env)?;
        let warnings = launch.warnings.clone();
        let spec = SpawnSpec {
            paths: self.paths.clone(),
            launch,
            cwd: req.cwd.clone(),
            shell: req.shell,
            git_bash: self.global.git_bash.clone(),
            command: req.command.clone(),
            size,
            inherited: std::env::vars_os().collect(),
        };
        let (pane, events) = Pane::spawn(spec).map_err(|e| e.to_string())?;
        Ok(Started {
            pane,
            events,
            warnings,
        })
    }

    /// What `hydra whoami` reports for `env`'s configured tools, with missing secrets
    /// allowed. Slow (it runs the tools): call it off the UI thread.
    pub fn whoami(&self, env: &str) -> Vec<IdentityReport> {
        let Ok(name) = EnvName::parse(env) else {
            return Vec::new();
        };
        let opts = PrepareOptions {
            allow_missing_secrets: true,
            no_guards: true,
            ..Default::default()
        };
        let providers = hydra_providers::all();
        let Ok(launch) = self.prepare_in_turn(&name, &providers, &opts) else {
            return Vec::new();
        };
        let warnings = Warnings::default();
        let ctx = Ctx {
            name: &name,
            env: &launch.config,
            paths: &self.paths,
            user_home: &self.user_home,
            secrets: &self.store,
            guards: false,
            warnings: &warnings,
        };
        let runner = EnvRunner { launch: &launch };
        providers
            .iter()
            .filter(|p| p.is_configured(&launch.config))
            .map(|p| p.check(&ctx, &runner))
            .collect()
    }
}

/// The status bar's accounts: `github: davojc · claude: me@x.com`, skipping info-only rows
/// and marking problems with `(!)`.
pub fn accounts_summary(reports: &[IdentityReport]) -> String {
    reports
        .iter()
        .filter(|r| r.status != Status::Info)
        .map(|r| {
            let mark = if r.status == Status::Ok { "" } else { " (!)" };
            format!("{}: {}{mark}", r.provider, r.detail)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// How long an environment's accounts stay on the status bar before they are checked again
/// (spec §7.1).
pub const STATUS_TTL: Duration = Duration::from_secs(60);

/// The last accounts summary per environment.
#[derive(Debug, Default)]
pub struct StatusCache {
    entries: HashMap<String, (Instant, String)>,
}

impl StatusCache {
    /// The last summary, however old.
    pub fn get(&self, env: &str) -> Option<&str> {
        self.entries.get(env).map(|(_, s)| s.as_str())
    }

    pub fn is_fresh(&self, env: &str, now: Instant) -> bool {
        self.entries
            .get(env)
            .is_some_and(|(at, _)| now.duration_since(*at) < STATUS_TTL)
    }

    pub fn store(&mut self, env: String, summary: String, now: Instant) {
        self.entries.insert(env, (now, summary));
    }
}

#[cfg(test)]
mod tests {
    use hydra_platform::shell::find_shell;

    use super::*;

    struct Fx {
        _d: tempfile::TempDir,
        hydra: Hydra,
        bound: PathBuf,
        free: PathBuf,
    }

    /// `work` (no tools) and `personal` (coloured); `config` with BOUND standing for a
    /// folder bound to personal.
    fn fx(config: &str) -> Fx {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path().join(".hydra"));
        for (env, toml) in [("work", ""), ("personal", "color = \"#112233\"\n")] {
            let n = EnvName::parse(env).unwrap();
            std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
            std::fs::write(paths.env_file(&n), toml).unwrap();
        }
        let bound = d.path().join("bound");
        let free = d.path().join("free");
        std::fs::create_dir_all(&bound).unwrap();
        std::fs::create_dir_all(&free).unwrap();
        let fwd = bound.to_string_lossy().replace('\\', "/");
        std::fs::write(paths.config_file(), config.replace("BOUND", &fwd)).unwrap();
        let store = KeyringStore::new(
            format!("hydraui-test-{}", std::process::id()),
            paths.secret_index(),
        );
        let (hydra, problem) = Hydra::new(paths, d.path().join("home"), store);
        assert_eq!(problem, None);
        Fx {
            _d: d,
            hydra,
            bound,
            free,
        }
    }

    const BOUND: &str = "[bindings]\n\"BOUND/**\" = \"personal\"\n";

    #[test]
    fn lists_environments() {
        let f = fx("");
        let mut names = f.hydra.env_names();
        names.sort();
        assert_eq!(names, vec!["personal", "work"]);
        assert!(f.hydra.env_exists("work"));
        assert!(!f.hydra.env_exists("nobody"));
        assert!(!f.hydra.env_exists("Not A Name"));
    }

    #[test]
    fn a_new_tab_uses_the_binding_then_default_env_then_asks() {
        let f = fx(BOUND);
        assert_eq!(
            f.hydra.adhoc_env(&f.bound, Some("work")).as_deref(),
            Some("personal")
        );
        assert_eq!(
            f.hydra.adhoc_env(&f.free, Some("work")).as_deref(),
            Some("work")
        );
        assert_eq!(f.hydra.adhoc_env(&f.free, Some("nobody")), None);
        assert_eq!(f.hydra.adhoc_env(&f.free, None), None);
    }

    #[test]
    fn shell_choice_is_hydraui_then_hydra_then_pwsh() {
        assert_eq!(fx("").hydra.shell(Some("bash")), ShellKind::Bash);
        assert_eq!(fx("").hydra.shell(None), ShellKind::Pwsh);
        let f = fx("default_shell = \"bash\"\n");
        assert_eq!(f.hydra.shell(None), ShellKind::Bash);
        assert_eq!(f.hydra.shell(Some("pwsh")), ShellKind::Pwsh);
        assert_eq!(f.hydra.shell(Some("fish")), ShellKind::Bash);
    }

    #[test]
    fn tints_come_from_env_toml_or_the_name() {
        let f = fx("");
        assert_eq!(f.hydra.tint("personal"), 0x112233);
        assert_eq!(f.hydra.tint("work"), env_tint("work", None));
    }

    #[test]
    fn a_bad_hydra_config_is_reported_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::write(paths.config_file(), "bindings = 3\n").unwrap();
        let store = KeyringStore::new("hydraui-test-unused", paths.secret_index());
        let (hydra, problem) = Hydra::new(paths, d.path().to_path_buf(), store);
        assert!(problem.unwrap().contains("config.toml"));
        assert!(hydra.global.bindings.is_empty());
    }

    #[test]
    fn prepare_works_and_a_missing_environment_says_so() {
        let f = fx("");
        assert_eq!(f.hydra.prepare("work").unwrap().name.as_str(), "work");
        assert!(f.hydra.prepare("nobody").unwrap_err().contains("nobody"));
    }

    #[test]
    fn prepare_from_several_threads_at_once_all_succeed() {
        // A pane starting while the status bar runs whoami on the same environment.
        for _ in 0..5 {
            let f = fx("");
            let results: Vec<Result<LaunchEnv, String>> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..8)
                    .map(|_| s.spawn(|| f.hydra.prepare("work")))
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for r in results {
                assert!(r.is_ok(), "{:?}", r.err());
            }
        }
    }

    #[test]
    fn whoami_checks_only_configured_tools() {
        // git is always configured: every environment has a git identity.
        let rows = fx("").hydra.whoami("work");
        let providers: Vec<&str> = rows.iter().map(|r| r.provider.as_str()).collect();
        assert_eq!(providers, vec!["git"]);
        assert!(fx("").hydra.whoami("nobody").is_empty());
    }

    #[test]
    fn start_runs_a_real_pane() {
        if find_shell(ShellKind::Pwsh, None).is_none() {
            eprintln!("skipped: pwsh isn't installed");
            return;
        }
        let f = fx("");
        let req = PaneRequest::new("work", f.free.clone(), ShellKind::Pwsh);
        let started = f
            .hydra
            .start(&req, GridSize { cols: 80, rows: 24 })
            .unwrap();
        assert!(started.warnings.is_empty());
        let missing = PaneRequest {
            cwd: f.free.join("gone"),
            ..req
        };
        assert!(
            f.hydra
                .start(&missing, GridSize { cols: 80, rows: 24 })
                .err()
                .unwrap()
                .contains("gone")
        );
    }

    #[test]
    fn accounts_summary_skips_info_rows() {
        let r = |provider: &str, status, detail: &str| IdentityReport {
            provider: provider.into(),
            status,
            detail: detail.into(),
        };
        let rows = [
            r("github", Status::Ok, "davojc"),
            r("env", Status::Info, "3 variables"),
            r("claude", Status::Missing, "not signed in"),
        ];
        assert_eq!(
            accounts_summary(&rows),
            "github: davojc · claude: not signed in (!)"
        );
    }

    #[test]
    fn status_cache_is_fresh_for_a_minute() {
        let mut c = StatusCache::default();
        let t0 = Instant::now();
        assert!(!c.is_fresh("work", t0));
        assert_eq!(c.get("work"), None);
        c.store("work".into(), "github: x".into(), t0);
        assert!(c.is_fresh("work", t0 + Duration::from_secs(59)));
        assert!(!c.is_fresh("work", t0 + STATUS_TTL));
        assert_eq!(c.get("work"), Some("github: x"));
    }
}
