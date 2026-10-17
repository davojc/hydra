//! A shell in a ConPTY, in a hydra environment, feeding an `alacritty_terminal` grid
//! (spec §6).
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::io::BorrowedHandle;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi::Processor;
use hydra_core::lock::{ShellLock, hold_shared};
use hydra_core::paths::HydraPaths;
use hydra_core::resolve::LaunchEnv;
use hydra_platform::shell::{ShellKind, find_shell};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::command::pane_command;
use crate::event::{Listener, TermEvent, Writer};
use crate::init::{next_pane_id, write_pane_init};
use crate::job::Job;
use crate::osc::{Osc, OscScanner};

/// A pane's size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl GridSize {
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Everything needed to start a pane. The app runs `prepare` (with `no_guards: true`) first.
pub struct SpawnSpec {
    pub paths: HydraPaths,
    pub launch: LaunchEnv,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// hydra's `git_bash` setting from config.toml, if any.
    pub git_bash: Option<PathBuf>,
    /// Shell text run after the shell starts.
    pub command: Option<String>,
    pub size: GridSize,
    /// The environment the shell starts from before hydra's changes; normally
    /// `std::env::vars_os().collect()`.
    pub inherited: Vec<(OsString, OsString)>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("folder {} doesn't exist", .0.display())]
    NoFolder(PathBuf),
    #[error("pwsh.exe isn't on PATH; install PowerShell 7")]
    NoPwsh,
    #[error("Git Bash wasn't found; set git_bash in config.toml")]
    NoBash,
    #[error("can't {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("can't start the terminal: {0}")]
    Pty(String),
}

fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> SpawnError {
    move |source| SpawnError::Io { what, source }
}

fn pty_err(e: impl std::fmt::Display) -> SpawnError {
    SpawnError::Pty(e.to_string())
}

/// A running shell. Dropping it ends the shell's whole process tree, closes the PTY and
/// releases the environment's shell lock, in that order (spec §6.4).
pub struct Pane {
    id: String,
    term: Arc<FairMutex<Term<Listener>>>,
    writer: Writer,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    wake: Arc<AtomicBool>,
    init: PathBuf,
    job: Option<Job>,
    /// Last field: released after everything else is gone.
    _lock: ShellLock,
}

impl Pane {
    /// Starts the shell (spec §6.1) and returns the pane with its events.
    pub fn spawn(spec: SpawnSpec) -> Result<(Pane, Receiver<TermEvent>), SpawnError> {
        // portable-pty would quietly start in the user's home instead.
        if !spec.cwd.is_dir() {
            return Err(SpawnError::NoFolder(spec.cwd));
        }
        let exe = find_shell(spec.shell, spec.git_bash.as_deref()).ok_or(match spec.shell {
            ShellKind::Pwsh => SpawnError::NoPwsh,
            ShellKind::Bash => SpawnError::NoBash,
        })?;
        let env = &spec.launch.name;
        let lock = hold_shared(&spec.paths, env).map_err(io("lock the environment"))?;
        let id = next_pane_id();
        let rgb = spec.launch.config.rgb().map(|c| (c.0, c.1, c.2));
        let init = write_pane_init(
            &spec.paths,
            env,
            rgb,
            spec.shell,
            spec.command.as_deref(),
            &id,
        )
        .map_err(io("write the init script"))?;
        let cmd = pane_command(
            spec.shell,
            &exe,
            &init,
            &spec.cwd,
            &spec.launch,
            &spec.inherited,
        );
        let (started, events) = match start(cmd, spec.size) {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(&init);
                return Err(e);
            }
        };
        let pane = Pane {
            id,
            term: started.term,
            writer: started.writer,
            master: Mutex::new(started.master),
            killer: started.killer,
            wake: started.wake,
            init,
            job: Some(started.job),
            _lock: lock,
        };
        Ok((pane, events))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Sends input (keys, pasted text) to the shell.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("the pane's writer is poisoned"))?;
        w.write_all(bytes)?;
        w.flush()
    }

    /// Resizes the PTY and the grid together.
    pub fn resize(&self, size: GridSize) -> std::io::Result<()> {
        self.master
            .lock()
            .map_err(|_| std::io::Error::other("the pane's PTY is poisoned"))?
            .resize(size.pty())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.term.lock().resize(size);
        Ok(())
    }

    /// Runs `f` on the grid, for rendering. Clears the pending `Wakeup` first, so output
    /// that arrives during or after `f` sends a new one.
    pub fn with_term<R>(&self, f: impl FnOnce(&mut Term<Listener>) -> R) -> R {
        self.wake.store(false, Ordering::Release);
        f(&mut self.term.lock())
    }

    /// The visible screen as text, one line per row with trailing spaces trimmed. For tests
    /// and diagnostics; it doesn't count as rendering.
    pub fn screen_text(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        (0..grid.screen_lines())
            .map(|l| {
                let row = &grid[Line(l as i32)];
                let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
                text.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // The job first: it ends the whole tree (npm → node), not just the shell.
        drop(self.job.take());
        let _ = self.killer.kill();
        let _ = std::fs::remove_file(&self.init);
        // Then the fields: the PTY closes (the reader thread sees EOF), the lock goes last.
    }
}

/// What `start` hands back to `spawn`.
struct Started {
    term: Arc<FairMutex<Term<Listener>>>,
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    wake: Arc<AtomicBool>,
    job: Job,
}

/// Opens the ConPTY, starts `cmd` in it inside a new job, and starts the reader and waiter
/// threads.
fn start(
    cmd: CommandBuilder,
    size: GridSize,
) -> Result<(Started, Receiver<TermEvent>), SpawnError> {
    let pair = native_pty_system().openpty(size.pty()).map_err(pty_err)?;
    let mut child = pair.slave.spawn_command(cmd).map_err(pty_err)?;
    drop(pair.slave);
    let killer = child.clone_killer();
    let job = Job::new().and_then(|job| {
        let process = child
            .as_raw_handle()
            .ok_or_else(|| std::io::Error::other("the shell has no process handle"))?;
        // SAFETY: `child` owns the handle and outlives this call.
        job.assign(unsafe { BorrowedHandle::borrow_raw(process) })?;
        Ok(job)
    });
    let job = match job {
        Ok(job) => job,
        Err(source) => {
            let _ = child.kill();
            return Err(SpawnError::Io {
                what: "put the shell in a job object",
                source,
            });
        }
    };
    // From here on an early return drops `job`, which ends the shell.
    let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(pty_err)?));
    let reader = pair.master.try_clone_reader().map_err(pty_err)?;
    let (tx, rx) = channel();
    let listener = Listener {
        writer: writer.clone(),
        events: tx.clone(),
    };
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &size,
        listener,
    )));
    let wake = Arc::new(AtomicBool::new(false));
    {
        let (term, wake, tx) = (term.clone(), wake.clone(), tx.clone());
        std::thread::Builder::new()
            .name("hydraui-pty-read".into())
            .spawn(move || read_loop(reader, &term, &wake, &tx))
            .map_err(io("start the reader thread"))?;
    }
    std::thread::Builder::new()
        .name("hydraui-pty-wait".into())
        .spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            let _ = tx.send(TermEvent::Exited(code));
        })
        .map_err(io("start the waiter thread"))?;
    Ok((
        Started {
            term,
            writer,
            master: pair.master,
            killer,
            wake,
            job,
        },
        rx,
    ))
}

/// Feeds PTY output to the grid and the OSC scanner until the PTY closes.
fn read_loop(
    mut reader: Box<dyn Read + Send>,
    term: &FairMutex<Term<Listener>>,
    wake: &AtomicBool,
    tx: &Sender<TermEvent>,
) {
    let mut parser: Processor = Processor::new();
    let mut osc = OscScanner::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let found = osc.feed(&buf[..n]);
        parser.advance(&mut *term.lock(), &buf[..n]);
        for o in found {
            let _ = tx.send(match o {
                Osc::Cwd(p) => TermEvent::CwdChanged(p),
                Osc::Prompt(s) => TermEvent::Prompt(s),
            });
        }
        // One Wakeup until the app renders (`with_term` clears the flag), so heavy output
        // can't flood the channel.
        if !wake.swap(true, Ordering::AcqRel) {
            let _ = tx.send(TermEvent::Wakeup);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use hydra_core::config::EnvConfig;
    use hydra_core::name::EnvName;

    use super::*;

    #[test]
    fn a_missing_folder_is_an_error_not_a_start_in_home() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("gone");
        let spec = SpawnSpec {
            paths: HydraPaths::new(d.path().join(".hydra")),
            launch: LaunchEnv {
                name: EnvName::parse("work").unwrap(),
                config: EnvConfig::default(),
                set: BTreeMap::new(),
                unset: BTreeSet::new(),
                path_drop: Vec::new(),
                warnings: Vec::new(),
            },
            cwd: missing.clone(),
            shell: ShellKind::Pwsh,
            git_bash: None,
            command: None,
            size: GridSize { cols: 80, rows: 24 },
            inherited: Vec::new(),
        };
        match Pane::spawn(spec) {
            Err(SpawnError::NoFolder(p)) => assert_eq!(p, missing),
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("spawned in a missing folder"),
        }
        assert!(
            !d.path().join(".hydra").exists(),
            "nothing is written before the folder check"
        );
    }
}
