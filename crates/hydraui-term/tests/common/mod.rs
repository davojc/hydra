//! Shared helpers for the real-shell tests: a temp HYDRA_HOME, a spawned pane, and waits.
#![allow(dead_code)] // each test file uses a different subset

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use hydra_core::config::EnvConfig;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::resolve::LaunchEnv;
use hydra_platform::shell::{ShellKind, find_shell};
use hydraui_term::{GridSize, Pane, PromptState, SpawnSpec, TermEvent};

pub const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Fx {
    pub dir: tempfile::TempDir,
    pub paths: HydraPaths,
    /// The pane's start folder. It holds `it's here`, a name both shells must quote.
    pub start: PathBuf,
}

impl Fx {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path().join(".hydra"));
        let start = dir.path().join("start here");
        std::fs::create_dir_all(start.join("it's here")).unwrap();
        Self { dir, paths, start }
    }

    pub fn env(&self) -> EnvName {
        EnvName::parse("work").unwrap()
    }

    /// A pane in `work` with FOO=bar set and GH_TOKEN removed, starting from the test's own
    /// environment plus an inherited GH_TOKEN. `None` (after a message) when the shell
    /// isn't installed.
    pub fn spawn(
        &self,
        shell: ShellKind,
        command: Option<&str>,
    ) -> Option<(Pane, Receiver<TermEvent>)> {
        if find_shell(shell, None).is_none() {
            eprintln!("skipped: {} isn't installed", shell.as_str());
            return None;
        }
        let mut inherited: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("GH_TOKEN") && !k.eq_ignore_ascii_case("HOME"))
            .collect();
        inherited.push(("GH_TOKEN".into(), "leaked".into()));
        if shell == ShellKind::Bash {
            // Keeps the developer's ~/.bashrc out of the test.
            inherited.push(("HOME".into(), self.dir.path().into()));
        }
        let spec = SpawnSpec {
            paths: self.paths.clone(),
            launch: LaunchEnv {
                name: self.env(),
                config: EnvConfig::default(),
                set: BTreeMap::from([("FOO".to_string(), "bar".to_string())]),
                unset: BTreeSet::from(["GH_TOKEN".to_string()]),
                path_drop: Vec::new(),
                warnings: Vec::new(),
            },
            cwd: self.start.clone(),
            shell,
            git_bash: None,
            command: command.map(str::to_string),
            size: GridSize {
                cols: 120,
                rows: 30,
            },
            inherited,
        };
        Some(Pane::spawn(spec).unwrap())
    }

    /// How many pane init scripts exist for `work`.
    pub fn scripts(&self) -> usize {
        std::fs::read_dir(
            self.paths
                .state_dir(&self.env())
                .join("shell")
                .join("hydraui"),
        )
        .map(|d| d.count())
        .unwrap_or(0)
    }
}

pub fn at_prompt(e: &TermEvent) -> bool {
    *e == TermEvent::Prompt(PromptState::AtPrompt)
}

/// Receives events until `pred` matches one; returns everything received, that one last.
pub fn wait_for(
    rx: &Receiver<TermEvent>,
    what: &str,
    mut pred: impl FnMut(&TermEvent) -> bool,
) -> Vec<TermEvent> {
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(e) => {
                let done = pred(&e);
                seen.push(e);
                if done {
                    return seen;
                }
            }
            Err(_) => panic!("timed out waiting for {what}; got {seen:?}"),
        }
    }
}

/// Just the prompt states in `events`, in order.
pub fn prompts(events: &[TermEvent]) -> Vec<PromptState> {
    events
        .iter()
        .filter_map(|e| match e {
            TermEvent::Prompt(p) => Some(*p),
            _ => None,
        })
        .collect()
}

/// Polls the screen until it contains `needle`.
pub fn wait_text(pane: &Pane, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !pane.screen_text().contains(needle) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; screen:\n{}",
            pane.screen_text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The same folder, however each side spells it (short names, case, `\\?\`).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `events` include a `CwdChanged` to `dir`.
pub fn reached(events: &[TermEvent], dir: &Path) -> bool {
    events
        .iter()
        .any(|e| matches!(e, TermEvent::CwdChanged(p) if same_dir(p, dir)))
}
