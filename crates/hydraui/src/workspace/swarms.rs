//! The swarms (spec: docs/superpowers/specs/2026-10-27-swarms-design.md): one window on this
//! machine runs every swarm, holding `<running_dir>/swarms.lock`, as the phone link is run. The
//! others try again every 10 s, so one of them takes over when that window closes.
//!
//! Commands go to the service and its events come back to the window, which shows the swarms and
//! raises their notices.
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{Context, Task, Window};
use gpui_component::WindowExt;
use gpui_component::notification::Notification;
use hydra_core::paths::HydraPaths;
use hydra_swarms::{Command, Identity, ServiceConfig, ServiceEvent, SwarmsService};

use super::Workspace;
use crate::action_log;
use crate::config::{self, UiConfig};
use crate::link::owner;
use crate::swarms::{self, store::SwarmKeys};

/// While another window runs the swarms, this one tries to take them this often.
const RETRY: Duration = Duration::from_secs(10);
/// The lock one window holds while it runs the swarms: `<running_dir>/swarms.lock`.
const LOCK: &str = "swarms";

/// The swarms service, while this window runs it. Dropping it stops the service and lets the
/// swarms go for another window only once the service has stopped: the forwarder owns the
/// lock and the service's events, and drops the lock when those end, which is once every
/// swarm has closed.
pub(super) struct SwarmsRun {
    commands: UnboundedSender<Command>,
    forwarder: Option<Task<()>>,
    _events: Task<()>,
}

impl Drop for SwarmsRun {
    fn drop(&mut self) {
        // The forwarder finishes on its own (not cancelled), so the lock outlives the service.
        let _ = self.commands.unbounded_send(Command::Stop);
        if let Some(forwarder) = self.forwarder.take() {
            forwarder.detach();
        }
    }
}

/// What came of trying to run the swarms.
enum Opened {
    Running(
        owner::OwnerLock,
        UnboundedSender<Command>,
        UnboundedReceiver<ServiceEvent>,
    ),
    /// No name yet, or no swarm to run: nothing was started.
    Off,
    /// Another window runs them.
    Elsewhere,
    Failed(String),
}

/// What the disk says about the swarms. hydraui windows don't watch config.toml, so another
/// window may have changed the names since this one read them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Saved {
    name: Option<String>,
    machine: Option<String>,
    /// This machine is in at least one swarm.
    swarms: bool,
}

impl Saved {
    /// Reads files: never on the UI thread.
    fn read(paths: &HydraPaths) -> Self {
        let cfg = config::load(paths).0;
        Saved {
            name: cfg.peer_name,
            machine: cfg.machine_name,
            swarms: swarms::has_swarms(paths),
        }
    }

    /// Takes the names into `ui`, over what this window remembered.
    fn adopt(self, ui: &mut UiConfig) {
        ui.peer_name = self.name;
        ui.machine_name = self.machine;
    }
}

/// What running the swarms does, decided by the disk alone.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// No name yet, or nothing to run: start nothing.
    Off,
    /// Start the service as `name` on `machine`.
    Start { name: String, machine: String },
}

/// `first` when a swarm is being made or joined, which starts the swarms with none yet.
fn plan(saved: &Saved, first: bool) -> Plan {
    let set = |n: &Option<String>| {
        n.as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(String::from)
    };
    let Some(name) = set(&saved.name) else {
        return Plan::Off;
    };
    if !saved.swarms && !first {
        return Plan::Off;
    }
    let machine = set(&saved.machine).unwrap_or_else(swarms::default_machine_name);
    Plan::Start { name, machine }
}

/// Reads the names and the registry and, when there is something to run, takes the swarms for
/// this window and starts the service, unless another window has them. Reads files: never on
/// the UI thread.
fn open(paths: &HydraPaths, first: bool) -> (Saved, Opened) {
    let saved = Saved::read(paths);
    let opened = match plan(&saved, first) {
        Plan::Off => Opened::Off,
        Plan::Start { name, machine } => start(paths, name, machine),
    };
    (saved, opened)
}

/// Takes the swarms for this window and starts the service, unless another window has them.
/// Reads the keyring: never on the UI thread.
fn start(paths: &HydraPaths, name: String, machine: String) -> Opened {
    let Some(lock) = owner::try_own_named(paths, LOCK) else {
        return Opened::Elsewhere;
    };
    let keys = SwarmKeys::new();
    let person = match keys.person() {
        Ok(key) => key,
        Err(e) => return Opened::Failed(format!("Couldn't read your swarm key: {e}")),
    };
    let identity = Identity {
        person,
        name,
        machine_name: machine,
    };
    let swarm_key = Box::new(move |id: &str| keys.swarm(id));
    let cfg = ServiceConfig::new(swarms::root(paths), identity, swarm_key, true);
    match SwarmsService::start(cfg) {
        Ok((commands, events)) => Opened::Running(lock, commands, events),
        Err(e) => Opened::Failed(format!("Couldn't start the swarms: {e}")),
    }
}

/// Whether the search goes on after `opened`: while another window runs the swarms, and while
/// there is nothing to run, since another window may make the first swarm or set the name and
/// then close.
fn try_again(opened: &Opened) -> bool {
    matches!(opened, Opened::Elsewhere | Opened::Off)
}

/// When the search next tries.
enum Next {
    Stop,
    /// A swarm was queued to be made or joined after this try read the disk.
    Now,
    /// After `RETRY`, or sooner when a swarm is queued to be made or joined.
    Later(futures::channel::oneshot::Receiver<()>),
}

/// Passes the service's events to the window until they end, which is once the service has
/// closed every swarm, then lets the swarms go, so the next window to run them never opens a
/// swarm this one still has open. Keeps going after the window has gone.
async fn forward(
    lock: owner::OwnerLock,
    mut events: UnboundedReceiver<ServiceEvent>,
    window: UnboundedSender<ServiceEvent>,
) {
    while let Some(event) = events.next().await {
        let _ = window.unbounded_send(event);
    }
    drop(lock);
}

/// Stops a service no longer wanted, then lets the swarms go once it has stopped.
async fn release(
    commands: UnboundedSender<Command>,
    mut events: UnboundedReceiver<ServiceEvent>,
    lock: owner::OwnerLock,
) {
    let _ = commands.unbounded_send(Command::Stop);
    while events.next().await.is_some() {}
    drop(lock);
}

impl Workspace {
    /// Runs every swarm in this window once your name is set and this machine is in a swarm,
    /// unless another window runs them. Until then it tries again every 10 s, so this one takes
    /// over when another closes, or once another has made the first swarm or set the name. Does
    /// nothing while they run here or are being sought.
    pub(super) fn swarms_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.swarms_run.is_some() || self.swarms_seeking {
            return;
        }
        self.swarms_seeking = true;
        let seek = self.swarms_seek;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                // Still wanted. The names and the registry are read from disk by `open`:
                // another window may have changed them since this one started.
                let Ok(Some((paths, first))) = this.update(cx, |ws, _| {
                    ws.swarms_wanted(seek)
                        .then(|| (ws.hydra.paths.clone(), !ws.swarms_pending.is_empty()))
                }) else {
                    return;
                };
                let (saved, opened) = cx
                    .background_spawn(async move { open(&paths, first) })
                    .await;
                let next = this.update_in(cx, |ws, window, cx| {
                    if !ws.swarms_opened(seek, first, saved, opened, window, cx) {
                        Next::Stop
                    } else if !first && !ws.swarms_pending.is_empty() {
                        Next::Now
                    } else {
                        let (wake, woken) = futures::channel::oneshot::channel();
                        ws.swarms_wake = Some(wake);
                        Next::Later(woken)
                    }
                });
                match next {
                    Ok(Next::Now) => {}
                    Ok(Next::Later(woken)) => {
                        let timer = cx.background_executor().timer(RETRY);
                        futures::future::select(timer, woken).await;
                    }
                    Ok(Next::Stop) | Err(_) => return,
                }
            }
        })
        .detach();
    }

    /// Stops the swarms here (or the wait for them), letting another window take them.
    #[expect(
        dead_code,
        reason = "nothing turns the swarms off yet: closing the window drops SwarmsRun, which stops them"
    )]
    pub(super) fn swarms_stop(&mut self) {
        self.swarms_seek += 1;
        self.swarms_seeking = false;
        self.swarms_here = false;
        self.swarms_elsewhere = false;
        self.swarms.clear();
        self.swarms_pending.clear();
        // Wakes a waiting search, which then ends.
        self.swarms_wake = None;
        if self.swarms_run.take().is_some() {
            action_log::write("swarms: this window let the swarms go");
        }
    }

    /// Sends `cmd` to the swarms; nothing when they don't run in this window.
    pub(super) fn swarms_command(&self, cmd: Command) {
        if let Some(run) = &self.swarms_run {
            let _ = run.commands.unbounded_send(cmd);
        }
    }

    /// Makes or joins a swarm (`Command::Create` or `Command::Join`). The first one starts the
    /// swarms, which don't run while this machine is in none: the search tries again at once.
    pub(super) fn swarms_create_or_join(
        &mut self,
        cmd: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(run) = &self.swarms_run {
            let _ = run.commands.unbounded_send(cmd);
            return;
        }
        self.swarms_pending.push(cmd);
        // A waiting search tries now; one mid-try tries again as soon as it has finished.
        if let Some(wake) = self.swarms_wake.take() {
            let _ = wake.send(());
        }
        self.swarms_start(window, cx);
    }

    /// Whether the search for the swarms numbered `seek` should go on.
    fn swarms_wanted(&self, seek: u64) -> bool {
        self.swarms_seek == seek && self.swarms_run.is_none()
    }

    /// Takes in what `open` found, `first` when it was looking for swarms to make or join; true
    /// while this window should try again.
    fn swarms_opened(
        &mut self,
        seek: u64,
        first: bool,
        saved: Saved,
        opened: Opened,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let current = self.swarms_seek == seek;
        if current {
            // The names on disk win over what this window remembered.
            saved.adopt(&mut self.ui);
            self.swarms_elsewhere = matches!(opened, Opened::Elsewhere);
            cx.notify();
        }
        let again = try_again(&opened) && self.swarms_wanted(seek);
        if current && !again {
            self.swarms_seeking = false;
        }
        // Swarms being made or joined, which can't be now. Ones queued after this try read the
        // disk wait for the next.
        let refused = current && first && !self.swarms_pending.is_empty();
        match opened {
            Opened::Running(lock, commands, events) => {
                // Stopped meanwhile: let the swarms go, once the service has stopped.
                if !self.swarms_wanted(seek) {
                    cx.background_spawn(release(commands, events, lock))
                        .detach();
                    return false;
                }
                action_log::write("swarms: this window runs the swarms");
                let (tx, mut rx) = unbounded();
                let forwarder = cx.background_spawn(forward(lock, events, tx));
                let pump = cx.spawn_in(window, async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        if this
                            .update_in(cx, |ws, window, cx| ws.swarms_event(event, window, cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                    // The service stopped by itself: it couldn't start, say.
                    let _ = this.update(cx, |ws, cx| ws.swarms_ended(seek, cx));
                });
                for cmd in self.swarms_pending.drain(..) {
                    let _ = commands.unbounded_send(cmd);
                }
                self.swarms_run = Some(SwarmsRun {
                    commands,
                    forwarder: Some(forwarder),
                    _events: pump,
                });
                self.swarms_here = true;
                cx.notify();
            }
            Opened::Failed(why) => {
                if current && first {
                    self.swarms_pending.clear();
                }
                action_log::write(format!("swarms: {why}"));
                window.push_notification(Notification::error(why).title("Swarms"), cx);
            }
            Opened::Elsewhere if refused => {
                self.swarms_pending.clear();
                let text = "Swarms run in another hydraui window: make or join the swarm there.";
                window.push_notification(Notification::warning(text).title("Swarms"), cx);
            }
            // With a swarm being made or joined, only a missing name stops them.
            Opened::Off if refused => {
                self.swarms_pending.clear();
                let text = "Set your name before making or joining a swarm.";
                window.push_notification(Notification::warning(text).title("Swarms"), cx);
            }
            Opened::Elsewhere | Opened::Off => {}
        }
        again
    }

    fn swarms_event(&mut self, event: ServiceEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ServiceEvent::Swarms(views) => {
                self.swarms = views;
                cx.notify();
            }
            ServiceEvent::Toast { text, .. } => {
                action_log::write(format!("swarms: {text}"));
                window.push_notification(Notification::warning(text).title("Swarms"), cx);
            }
            ServiceEvent::Failed(text) => {
                action_log::write(format!("swarms: {text}"));
                window.push_notification(Notification::error(text).title("Swarms"), cx);
            }
        }
    }

    /// The service numbered `seek` stopped by itself, and its lock has gone with it.
    fn swarms_ended(&mut self, seek: u64, cx: &mut Context<Self>) {
        if self.swarms_seek != seek || self.swarms_run.is_none() {
            return;
        }
        action_log::write("swarms: the swarms stopped");
        self.swarms_run = None;
        self.swarms_here = false;
        self.swarms.clear();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swarm_in_registry(paths: &HydraPaths) {
        let root = swarms::root(paths);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("registry.json"),
            r#"[{"ns":"a","anchor":"b","folder":"c"}]"#,
        )
        .unwrap();
    }

    #[test]
    fn swarms_do_not_start_before_a_name_is_set() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        swarm_in_registry(&paths);
        let (got, opened) = open(&paths, false);
        assert_eq!(got.name, None);
        assert!(matches!(opened, Opened::Off));
        assert!(
            !owner::held_elsewhere_named(&paths, LOCK),
            "the swarms weren't taken without a name"
        );
        let saved = |name: Option<&str>, swarms: bool| Saved {
            name: name.map(Into::into),
            machine: None,
            swarms,
        };
        assert_eq!(plan(&saved(None, true), true), Plan::Off);
        assert_eq!(plan(&saved(Some("  "), true), false), Plan::Off);
        assert_eq!(plan(&saved(Some("Davo"), false), false), Plan::Off);
        let davo = Plan::Start {
            name: "Davo".into(),
            machine: swarms::default_machine_name(),
        };
        assert_eq!(plan(&saved(Some("Davo"), true), false), davo);
        // Making or joining the first swarm starts them with none in the registry.
        assert_eq!(plan(&saved(Some("Davo"), false), true), davo);
        let named = Saved {
            machine: Some("Laptop".into()),
            ..saved(Some("Davo"), true)
        };
        assert_eq!(
            plan(&named, false),
            Plan::Start {
                name: "Davo".into(),
                machine: "Laptop".into()
            }
        );
    }

    #[test]
    fn the_search_goes_on_while_another_window_runs_them_or_there_is_nothing_to_run() {
        assert!(try_again(&Opened::Elsewhere));
        // Another window may make the first swarm, or set the name, and then close.
        assert!(try_again(&Opened::Off));
        assert!(!try_again(&Opened::Failed("no keyring".into())));
    }

    #[test]
    fn the_lock_is_held_until_the_service_has_stopped() {
        use std::future::Future;
        use std::task::{Context, Poll};
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let lock = owner::try_own_named(&paths, LOCK).unwrap();
        let (service, events) = unbounded();
        let (window, mut shown) = unbounded();
        let mut fwd = Box::pin(forward(lock, events, window));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(fwd.as_mut().poll(&mut cx).is_pending());
        let event = ServiceEvent::Failed("x".into());
        service.unbounded_send(event.clone()).unwrap();
        assert!(fwd.as_mut().poll(&mut cx).is_pending());
        assert_eq!(shown.try_recv().ok(), Some(event));
        assert!(owner::held_elsewhere_named(&paths, LOCK));
        // The service's events end once it has closed every swarm.
        drop(service);
        assert_eq!(fwd.as_mut().poll(&mut cx), Poll::Ready(()));
        assert!(!owner::held_elsewhere_named(&paths, LOCK));
    }

    #[test]
    fn the_registry_says_whether_there_are_swarms() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert!(!swarms::has_swarms(&paths), "no registry yet");
        std::fs::create_dir_all(swarms::root(&paths)).unwrap();
        std::fs::write(swarms::root(&paths).join("registry.json"), "[]").unwrap();
        assert!(!swarms::has_swarms(&paths));
        swarm_in_registry(&paths);
        assert!(swarms::has_swarms(&paths));
        // A damaged one is the service's to set aside and report.
        std::fs::write(swarms::root(&paths).join("registry.json"), "{").unwrap();
        assert!(swarms::has_swarms(&paths));
    }

    #[test]
    fn a_hydra_home_gets_its_own_keyring_service() {
        assert_eq!(swarms::store::service_for(None), "hydraui");
        assert_eq!(
            swarms::store::service_for(Some(std::ffi::OsStr::new(r"E:\scratch\a"))),
            r"hydraui:E:\scratch\a"
        );
        assert_ne!(
            swarms::store::service_for(Some(std::ffi::OsStr::new("a"))),
            swarms::store::service_for(Some(std::ffi::OsStr::new("b")))
        );
        assert_eq!(
            swarms::store::swarm_user("join-0123456789abcdef"),
            "swarm-key-join-0123456789abcdef"
        );
        assert_eq!(swarms::store::PERSON_USER, "swarm-person-key");
    }

    #[test]
    fn the_default_machine_name_is_the_host_name() {
        let named = |c: Option<&str>, h: Option<&str>| {
            swarms::machine_name_from(c.map(Into::into), h.map(Into::into))
        };
        assert_eq!(named(Some("DESKTOP-1"), Some("box")), "DESKTOP-1");
        assert_eq!(named(None, Some("box")), "box");
        assert_eq!(named(Some(" "), Some("box")), "box");
        assert_eq!(named(None, None), "This machine");
        let host = std::env::var("COMPUTERNAME")
            .ok()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| "This machine".into());
        assert_eq!(swarms::default_machine_name(), host.trim());
    }
}
