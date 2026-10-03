use std::collections::HashMap;
use std::path::PathBuf;

use hydra_core::config::EnvConfig;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::provider::{CommandRunner, Ctx};
use hydra_core::secret::MemoryStore;

/// A temporary hydra home with one environment named `work`.
pub struct Fixture {
    /// Keeps the temporary folder alive for the fixture's lifetime.
    _dir: tempfile::TempDir,
    pub paths: HydraPaths,
    pub name: EnvName,
    pub env: EnvConfig,
    pub home: PathBuf,
    pub store: MemoryStore,
}

impl Fixture {
    pub fn new(env_toml: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path().join(".hydra"));
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env: EnvConfig = toml::from_str(env_toml).unwrap();
        Self { _dir: dir, paths, name: EnvName::parse("work").unwrap(), env, home, store: MemoryStore::default() }
    }

    pub fn ctx(&self) -> Ctx<'_> {
        Ctx { name: &self.name, env: &self.env, paths: &self.paths, user_home: &self.home, secrets: &self.store }
    }
}

/// Answers commands from a table keyed by the full command line.
#[derive(Default)]
pub struct FakeRunner(pub HashMap<String, Result<String, String>>);

impl FakeRunner {
    pub fn with(mut self, cmd: &str, out: Result<&str, &str>) -> Self {
        self.0.insert(cmd.to_string(), out.map(str::to_string).map_err(str::to_string));
        self
    }
}

impl CommandRunner for FakeRunner {
    fn output(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let key = std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
        self.0.get(&key).cloned().unwrap_or_else(|| Err(format!("unexpected command: {key}")))
    }
}
