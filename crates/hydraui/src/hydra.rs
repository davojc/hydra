//! hydraui's link to hydra (spec §3.1, §4, §7.1): environments, `prepare` with guards off,
//! starting panes, and the identity checks for the status bar. No UI.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use hydra_core::bindings;
use hydra_core::borrow::declared_owner;
use hydra_core::config::{EnvConfig, GlobalConfig, load_env, load_global, parse_env};
use hydra_core::envedit::{self, ToolValues};
use hydra_core::envs;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::provider::{Ctx, IdentityReport, Provider, Status, Warnings};
use hydra_core::resolve::{ENV_VARS_MARKER, LaunchEnv, PrepareError, PrepareOptions, prepare};
use hydra_core::secret::{SecretRef, SecretStore};
use hydra_platform::keyring_store::KeyringStore;
use hydra_platform::process::EnvRunner;
use hydra_platform::shell::{ShellKind, pwsh_quote, sh_quote};
use hydraui_term::{GridSize, Pane, SpawnSpec, TermEvent};

use hydra_providers::claude::is_session_marker;

use crate::accounts::{AddChoice, add_choices};
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
    /// Variables the pane's shell must not inherit (a sign-in's `auth_unset`).
    pub drop_vars: Vec<String>,
    /// Start even when a referenced secret isn't stored (signing in is how it gets stored).
    pub allow_missing_secrets: bool,
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
            drop_vars: Vec::new(),
            allow_missing_secrets: false,
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
    store: Box<dyn SecretStore + Send + Sync>,
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
        let store = Box::new(KeyringStore::from_env(&paths));
        Ok(Hydra::new(paths, user_home, store))
    }

    pub fn new(
        paths: HydraPaths,
        user_home: PathBuf,
        store: Box<dyn SecretStore + Send + Sync>,
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

    /// `prepare` for a pane: guards off (spec §4.1), plus a sign-in's dropped variables and
    /// leniency about missing secrets.
    pub fn prepare(&self, req: &PaneRequest) -> Result<LaunchEnv, String> {
        let name = EnvName::parse(&req.env).map_err(|e| e.to_string())?;
        let opts = PrepareOptions {
            no_guards: true,
            allow_missing_secrets: req.allow_missing_secrets,
            drop_vars: req.drop_vars.clone(),
            inherited_env_vars: std::env::var(ENV_VARS_MARKER).ok(),
            ..Default::default()
        };
        self.prepare_in_turn(&name, &hydra_providers::all(), &opts)
            .map_err(|e| e.to_string())
    }

    fn ctx<'a>(
        &'a self,
        name: &'a EnvName,
        config: &'a EnvConfig,
        warnings: &'a Warnings,
    ) -> Ctx<'a> {
        Ctx {
            name,
            env: config,
            paths: &self.paths,
            user_home: &self.user_home,
            secrets: self.store.as_ref(),
            guards: false,
            warnings,
        }
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
            self.store.as_ref(),
            opts,
        )
    }

    /// Prepares the environment and starts the pane's shell. The error is the pane's banner
    /// text (spec §7.5).
    pub fn start(&self, req: &PaneRequest, size: GridSize) -> Result<Started, String> {
        let launch = self.prepare(req)?;
        let warnings = launch.warnings.clone();
        let spec = SpawnSpec {
            paths: self.paths.clone(),
            launch,
            cwd: req.cwd.clone(),
            shell: req.shell,
            git_bash: self.global.git_bash.clone(),
            command: req.command.clone(),
            size,
            inherited: inherited_env(std::env::vars_os()),
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
        let ctx = self.ctx(&name, &launch.config, &warnings);
        let runner = EnvRunner { launch: &launch };
        providers
            .iter()
            .filter(|p| p.is_configured(&launch.config))
            .map(|p| p.check(&ctx, &runner))
            .collect()
    }

    /// `env`'s env.toml, without a byte-order mark.
    fn env_text(&self, env: &str) -> Result<String, String> {
        let name = EnvName::parse(env).map_err(|e| e.to_string())?;
        let file = self.paths.env_file(&name);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("can't read {}: {e}", file.display()))?;
        Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
    }

    /// Turns `tool` on for `env`, as `hydra add` does: with its own login, or `from` another
    /// environment's. The edited env.toml is checked first; nothing is written if it fails.
    pub fn add_tool(&self, env: &str, tool: &str, from: Option<&str>) -> Result<(), String> {
        let text = self.env_text(env)?;
        let values = ToolValues {
            from: from.map(str::to_string),
            ..Default::default()
        };
        let (new_text, _) = envedit::add_tool(&text, tool, &values)?;
        let name = EnvName::parse(env).map_err(|e| e.to_string())?;
        parse_env(&self.paths, &name, &new_text).map_err(|e| e.to_string())?;
        let file = self.paths.env_file(&name);
        std::fs::write(&file, new_text).map_err(|e| format!("can't write {}: {e}", file.display()))
    }

    /// The accounts that can be added to `env`: sign-ins for the tools it doesn't have, and the
    /// tools other environments have with their own login.
    pub fn account_choices(&self, env: &str) -> Vec<AddChoice> {
        let tools = |text: &str| -> Vec<String> {
            envedit::tools_in(text)
                .unwrap_or_default()
                .into_iter()
                .map(str::to_string)
                .collect()
        };
        let Ok(text) = self.env_text(env) else {
            return Vec::new();
        };
        let mut names = self.env_names();
        names.sort();
        let others: Vec<(String, Vec<String>)> = names
            .into_iter()
            .filter(|n| n != env)
            .filter_map(|n| {
                let t = self.env_text(&n).ok()?;
                let owned = tools(&t)
                    .into_iter()
                    .filter(|tool| declared_owner(&t, tool).is_none())
                    .collect();
                Some((n, owned))
            })
            .collect();
        add_choices(&tools(&text), &others)
    }

    /// The tools in `env` that have a sign-in command (what `hydra auth <tool> <env>`
    /// accepts), by id.
    pub fn sign_ins(&self, env: &str) -> Vec<String> {
        let Ok(name) = EnvName::parse(env) else {
            return Vec::new();
        };
        let Ok(config) = load_env(&self.paths, &name) else {
            return Vec::new();
        };
        let warnings = Warnings::default();
        let ctx = self.ctx(&name, &config, &warnings);
        hydra_providers::all()
            .iter()
            .filter(|p| p.is_configured(&config) && p.auth_command(&ctx).is_some())
            .map(|p| p.id().to_string())
            .collect()
    }

    /// A pane that signs `tool` in for `env` as `hydra auth <tool> <env>` does (spec §7.4):
    /// the tool's sign-in command as the startup command, its `auth_unset` variables dropped,
    /// missing secrets allowed.
    pub fn sign_in_request(
        &self,
        env: &str,
        tool: &str,
        cwd: PathBuf,
        shell: ShellKind,
    ) -> Result<PaneRequest, String> {
        let p = hydra_providers::by_id(tool).ok_or_else(|| format!("unknown tool {tool:?}"))?;
        let mut req = PaneRequest::new(env, cwd, shell);
        req.allow_missing_secrets = true;
        req.drop_vars = p.auth_unset().iter().map(|s| s.to_string()).collect();
        let launch = self.prepare(&req)?;
        if !p.is_configured(&launch.config) {
            return Err(format!(
                "{id} isn't configured for {env}; add a [{id}] section to its env.toml",
                id = p.id()
            ));
        }
        let warnings = Warnings::default();
        let argv = p
            .auth_command(&self.ctx(&launch.name, &launch.config, &warnings))
            .ok_or_else(|| {
                format!(
                    "{} has no sign-in command; store its secret with Set secret",
                    p.id()
                )
            })?;
        req.command = Some(command_line(shell, &argv));
        Ok(req)
    }

    /// The `secret:` references in `env`'s env.toml that aren't stored yet, as `env/key`.
    pub fn missing_secrets(&self, env: &str) -> Vec<String> {
        let Ok(name) = EnvName::parse(env) else {
            return Vec::new();
        };
        match std::fs::read_to_string(self.paths.env_file(&name)) {
            Ok(text) => missing_secrets(&text, self.store.as_ref()),
            Err(_) => Vec::new(),
        }
    }

    /// Stores a secret through hydra's store, which also keeps secrets.toml's index. The
    /// value is kept exactly as typed; an empty or blank one is refused.
    pub fn set_secret(&self, path: &str, value: &str) -> Result<(), String> {
        let r = SecretRef::parse_path(path.trim()).map_err(|e| e.to_string())?;
        if value.trim().is_empty() {
            return Err(format!("{}: the value is empty", r.path()));
        }
        self.store.set(&r, value).map_err(|e| e.to_string())
    }
}

/// The environment a pane starts from: hydraui's own, without a Claude Code session's markers
/// (hydraui may have been started from inside one; a pane is a session of its own).
pub fn inherited_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(k, _)| !k.to_str().is_some_and(is_session_marker))
        .collect()
}

/// `argv` as one command line for `shell`, every word quoted, so paths with spaces or
/// apostrophes stay one word.
pub fn command_line(shell: ShellKind, argv: &[String]) -> String {
    match shell {
        ShellKind::Pwsh => {
            let words: Vec<String> = argv.iter().map(|a| pwsh_quote(a)).collect();
            format!("& {}", words.join(" "))
        }
        ShellKind::Bash => argv
            .iter()
            .map(|a| sh_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The `secret:` references anywhere in env.toml text that `store` doesn't hold, as sorted
/// `env/key` paths. Text that isn't TOML has none.
pub fn missing_secrets(env_toml: &str, store: &dyn SecretStore) -> Vec<String> {
    let Ok(table) =
        toml::from_str::<toml::Table>(env_toml.strip_prefix('\u{feff}').unwrap_or(env_toml))
    else {
        return Vec::new();
    };
    let mut refs = BTreeSet::new();
    collect_refs(&toml::Value::Table(table), &mut refs);
    refs.into_iter()
        .filter(|r| matches!(store.get(r), Ok(None)))
        .map(|r| r.path())
        .collect()
}

fn collect_refs(value: &toml::Value, out: &mut BTreeSet<SecretRef>) {
    match value {
        toml::Value::String(s) => {
            if let Ok(Some(r)) = SecretRef::parse_value(s) {
                out.insert(r);
            }
        }
        toml::Value::Array(items) => items.iter().for_each(|v| collect_refs(v, out)),
        toml::Value::Table(t) => t.values().for_each(|v| collect_refs(v, out)),
        _ => {}
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

/// The last account checks per environment.
#[derive(Debug, Default)]
pub struct StatusCache {
    entries: HashMap<String, (Instant, Vec<IdentityReport>)>,
    expired: HashSet<String>,
}

impl StatusCache {
    /// The last checks, however old.
    pub fn get(&self, env: &str) -> Option<&[IdentityReport]> {
        self.entries.get(env).map(|(_, r)| r.as_slice())
    }

    pub fn is_fresh(&self, env: &str, now: Instant) -> bool {
        !self.expired.contains(env)
            && self
                .entries
                .get(env)
                .is_some_and(|(at, _)| now.duration_since(*at) < STATUS_TTL)
    }

    /// Checks again on the next refresh, still showing the last result until then.
    pub fn expire(&mut self, env: &str) {
        self.expired.insert(env.to_string());
    }

    pub fn store(&mut self, env: String, reports: Vec<IdentityReport>, now: Instant) {
        self.expired.remove(&env);
        self.entries.insert(env, (now, reports));
    }
}

#[cfg(test)]
mod tests {
    use hydra_core::secret::{MemoryStore, SecretRef, SecretStore};
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
        for (env, toml) in [
            ("work", ""),
            ("personal", "color = \"#112233\"\n"),
            ("gh", "[github]\n"),
            (
                "keyed",
                "[env]\nLINEAR_API_KEY = \"secret:keyed/linear\"\nPLAIN = \"x\"\n",
            ),
        ] {
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
        let (hydra, problem) = Hydra::new(
            paths,
            d.path().join("home"),
            Box::new(MemoryStore::default()),
        );
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
    fn adding_a_tool_turns_it_on_ready_to_sign_in() {
        let f = fx("");
        f.hydra.add_tool("work", "github", None).unwrap();
        assert_eq!(f.hydra.sign_ins("work"), vec!["github"]);
    }

    #[test]
    fn every_sign_in_tool_can_be_added_and_signed_in() {
        let f = fx("");
        for tool in crate::accounts::SIGN_IN_TOOLS {
            f.hydra.add_tool("work", tool, None).unwrap();
            assert!(
                f.hydra.sign_ins("work").contains(&tool.to_string()),
                "{tool}"
            );
        }
    }

    #[test]
    fn a_tool_can_be_borrowed_from_an_env_that_has_it() {
        let f = fx("");
        f.hydra.add_tool("work", "github", Some("gh")).unwrap();
        let text =
            std::fs::read_to_string(f.hydra.paths.env_file(&EnvName::parse("work").unwrap()))
                .unwrap();
        assert!(text.contains("from = \"gh\""), "{text}");
    }

    #[test]
    fn a_tool_that_doesnt_check_out_leaves_env_toml_alone() {
        let f = fx("");
        let file = f.hydra.paths.env_file(&EnvName::parse("work").unwrap());
        let before = std::fs::read_to_string(&file).unwrap();
        // personal has no github to lend.
        assert!(
            f.hydra
                .add_tool("work", "github", Some("personal"))
                .is_err()
        );
        assert!(f.hydra.add_tool("work", "nope", None).is_err());
        assert!(f.hydra.add_tool("nobody", "github", None).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    #[test]
    fn account_choices_skip_whats_on_and_borrow_what_others_own() {
        let f = fx("");
        let work = f.hydra.account_choices("work");
        assert!(work.contains(&AddChoice::SignIn("github".into())));
        assert!(work.contains(&AddChoice::Borrow {
            tool: "github".into(),
            from: "gh".into()
        }));
        let gh = f.hydra.account_choices("gh");
        assert!(!gh.iter().any(|c| matches!(c,
            AddChoice::SignIn(t) | AddChoice::Borrow { tool: t, .. } if t == "github")));
    }

    #[test]
    fn lists_environments() {
        let f = fx("");
        let mut names = f.hydra.env_names();
        names.sort();
        assert_eq!(names, vec!["gh", "keyed", "personal", "work"]);
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
        let (hydra, problem) = Hydra::new(
            paths,
            d.path().to_path_buf(),
            Box::new(MemoryStore::default()),
        );
        assert!(problem.unwrap().contains("config.toml"));
        assert!(hydra.global.bindings.is_empty());
    }

    #[test]
    fn prepare_works_and_a_missing_environment_says_so() {
        let f = fx("");
        assert_eq!(
            f.hydra
                .prepare(&PaneRequest::new("work", f.free.clone(), ShellKind::Pwsh))
                .unwrap()
                .name
                .as_str(),
            "work"
        );
        assert!(
            f.hydra
                .prepare(&PaneRequest::new("nobody", f.free.clone(), ShellKind::Pwsh))
                .unwrap_err()
                .contains("nobody")
        );
    }

    #[test]
    fn prepare_from_several_threads_at_once_all_succeed() {
        // A pane starting while the status bar runs whoami on the same environment.
        for _ in 0..5 {
            let f = fx("");
            let results: Vec<Result<LaunchEnv, String>> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        s.spawn(|| {
                            f.hydra.prepare(&PaneRequest::new(
                                "work",
                                f.free.clone(),
                                ShellKind::Pwsh,
                            ))
                        })
                    })
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
        let found = vec![IdentityReport {
            provider: "github".into(),
            status: Status::Ok,
            detail: "x".into(),
        }];
        c.store("work".into(), found.clone(), t0);
        assert!(c.is_fresh("work", t0 + Duration::from_secs(59)));
        assert!(!c.is_fresh("work", t0 + STATUS_TTL));
        assert_eq!(c.get("work"), Some(found.as_slice()));
    }

    #[test]
    fn an_expired_env_is_checked_again_but_keeps_its_last_result() {
        let mut c = StatusCache::default();
        let t0 = Instant::now();
        c.store("work".into(), Vec::new(), t0);
        c.expire("work");
        assert!(!c.is_fresh("work", t0));
        assert_eq!(c.get("work"), Some(&[][..]));
        c.store("work".into(), Vec::new(), t0);
        assert!(c.is_fresh("work", t0));
    }

    #[test]
    fn prepare_needs_secrets_unless_signing_in() {
        let f = fx("");
        let mut req = PaneRequest::new("keyed", f.free.clone(), ShellKind::Pwsh);
        assert!(f.hydra.prepare(&req).is_err());
        req.allow_missing_secrets = true;
        assert!(f.hydra.prepare(&req).is_ok());
    }

    #[test]
    fn missing_secrets_are_the_unstored_references() {
        let store = MemoryStore::default();
        store
            .set(&SecretRef::parse_path("work/b").unwrap(), "1")
            .unwrap();
        let text = "[env]\nA = \"secret:work/a\"\nB = \"secret:work/b\"\nC = \"plain\"\n\
                    [gws]\nlist = [\"secret:acme/x\"]\n";
        assert_eq!(missing_secrets(text, &store), vec!["acme/x", "work/a"]);
        assert!(missing_secrets("not toml [", &store).is_empty());
        let f = fx("");
        assert_eq!(f.hydra.missing_secrets("keyed"), vec!["keyed/linear"]);
        assert!(f.hydra.missing_secrets("work").is_empty());
        assert!(f.hydra.missing_secrets("nobody").is_empty());
    }

    #[test]
    fn set_secret_stores_exactly_and_refuses_blank_values() {
        let f = fx("");
        f.hydra.set_secret("keyed/linear", "  a b  ").unwrap();
        assert!(f.hydra.missing_secrets("keyed").is_empty());
        let stored = f
            .hydra
            .store
            .get(&SecretRef::parse_path("keyed/linear").unwrap())
            .unwrap();
        assert_eq!(stored.as_deref(), Some("  a b  "));
        assert!(
            f.hydra
                .set_secret("keyed/other", "   ")
                .unwrap_err()
                .contains("empty")
        );
        assert!(f.hydra.set_secret("no-slash", "x").is_err());
    }

    #[test]
    fn sign_ins_are_the_configured_tools_with_a_sign_in_command() {
        let f = fx("");
        assert_eq!(f.hydra.sign_ins("gh"), vec!["github"]);
        assert!(f.hydra.sign_ins("work").is_empty());
        assert!(f.hydra.sign_ins("nobody").is_empty());
    }

    #[test]
    fn a_sign_in_pane_runs_the_tools_own_command() {
        let f = fx("");
        let req = f
            .hydra
            .sign_in_request("gh", "github", f.free.clone(), ShellKind::Pwsh)
            .unwrap();
        assert_eq!(
            req.command.as_deref(),
            Some("& 'gh' 'auth' 'login' '--hostname' 'github.com'")
        );
        let unset: Vec<String> = hydra_providers::by_id("github")
            .unwrap()
            .auth_unset()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(req.drop_vars, unset);
        assert!(req.allow_missing_secrets);
        assert!(
            f.hydra
                .sign_in_request("work", "github", f.free.clone(), ShellKind::Pwsh)
                .unwrap_err()
                .contains("isn't configured")
        );
        assert!(
            f.hydra
                .sign_in_request("gh", "nope", f.free.clone(), ShellKind::Pwsh)
                .unwrap_err()
                .contains("nope")
        );
    }

    #[test]
    fn command_lines_quote_every_word() {
        assert_eq!(
            command_line(ShellKind::Pwsh, &["a‘b‛c".to_string()]),
            "& 'a‘‘b‛‛c'"
        );
        let argv = vec![
            "C:\\Program Files\\x\\gh.exe".to_string(),
            "it's".to_string(),
        ];
        assert_eq!(
            command_line(ShellKind::Pwsh, &argv),
            "& 'C:\\Program Files\\x\\gh.exe' 'it''s'"
        );
        assert_eq!(
            command_line(ShellKind::Bash, &argv),
            "'C:\\Program Files\\x\\gh.exe' 'it'\\''s'"
        );
    }

    #[test]
    fn panes_dont_inherit_a_claude_code_session() {
        let vars: Vec<(OsString, OsString)> = vec![
            ("CLAUDECODE".into(), "1".into()),
            ("CLAUDE_CODE_CHILD_SESSION".into(), "1".into()),
            ("Path".into(), "C:/x".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN".into(), "t".into()),
        ];
        let kept: Vec<String> = inherited_env(vars)
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept, vec!["Path", "CLAUDE_CODE_OAUTH_TOKEN"]);
    }
}
