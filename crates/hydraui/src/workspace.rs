//! The window's content (spec §7.1, §7.5, §7.6; env-tabs design §2): one tab per environment,
//! the selected environment's shown terminal beside its file pane, the status bar, and the
//! commands bound to keys.
mod accounts;
mod all_tasks;
mod asking;
mod busy;
mod finder;
mod launcher;
mod layout_file;
mod linear;
mod orchestrator;
mod pickup;
mod pipeline_history;
mod pipeline_settings;
mod pipeline_tab;
mod settings_tab;
mod shortcuts;
mod slack;
mod start_page;
mod tasks_tab;
mod terminal_list;
mod title_bar;
mod tree_ops;
mod usage;
mod worktree;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity,
    FocusHandle, Focusable, Hsla, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    SharedString, Subscription, Task, Window, canvas, div, px, relative, rgb, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex};
use hydraui_sets::layout::Layout;

use hydraui_term::PromptState;

use crate::config::UiConfig;
use crate::envs::{EnvTab, EnvTabs, MAX_RATIO, MIN_RATIO, PaneId, new_terminal_cwd};
use crate::file_pane::{FilePane, FilePaneEvent};
use crate::file_tree::{cd_command, editor_command, quote_path};
use crate::file_tree_view::{FileTreeView, TreeEvent};
use crate::git_status;
use crate::hydra::{Borrowing, Hydra, PaneRequest, STATUS_TTL, StatusCache, account_marks};
use crate::keys::{
    ClosePane, FileTree, NewTerminal, NextTab, NextTerminal, PreviousTab, PreviousTerminal,
    TestDialog,
};
use crate::pane_view::{PaneEvent, PaneView, TermStyle};
use crate::task_board::TaskBoard;
use crate::task_board_view::TaskBoardView;
use busy::PaneActivity;
use tree_ops::NameFor;

/// The terminal list's width: it starts at the default, can't be dragged narrower than the
/// minimum, and always leaves the middle at least `MIN_PANES_WIDTH` beside the tree.
pub(super) const DEFAULT_LIST_WIDTH: f32 = 280.0;
const MIN_LIST_WIDTH: f32 = 120.0;
/// The divider between the list and the middle.
const LIST_DIVIDER_WIDTH: f32 = 4.0;

/// The list's width with its divider dragged to `x`, the list starting at the window's left
/// edge; `tree_space` is the tree's width plus its divider, 0 when the tree is closed.
fn list_width_at(x: f32, window_width: f32, tree_space: f32) -> f32 {
    clamp_list_width(x, window_width, tree_space)
}

/// `width` kept between the list's minimum and what leaves the middle its own beside the tree
/// and the list's divider, in a window `window_width` wide; the default when it isn't a number.
fn clamp_list_width(width: f32, window_width: f32, tree_space: f32) -> f32 {
    if !width.is_finite() {
        return DEFAULT_LIST_WIDTH;
    }
    let max =
        (window_width - tree_space - LIST_DIVIDER_WIDTH - MIN_PANES_WIDTH).max(MIN_LIST_WIDTH);
    width.clamp(MIN_LIST_WIDTH, max)
}

/// The file tree's width: it starts at the default, can't be dragged narrower than the
/// minimum, and always leaves the middle at least `MIN_PANES_WIDTH` beside the terminal list
/// and the tree's divider.
const DEFAULT_TREE_WIDTH: f32 = 260.0;
const MIN_TREE_WIDTH: f32 = 140.0;
const MIN_PANES_WIDTH: f32 = 200.0;
/// The divider between the middle and the tree.
const TREE_DIVIDER_WIDTH: f32 = 4.0;

/// The tree's width with its divider dragged to `x`, the tree on the right of a window
/// `window_width` wide (env-tabs design §2.5); `list_space` is the list's width plus its divider.
fn tree_width_at(x: f32, window_width: f32, list_space: f32) -> f32 {
    clamp_tree_width(window_width - x, window_width, list_space)
}

/// `width` kept between the tree's minimum and what leaves the middle its own beside the
/// terminal list (`list_space`) and the divider, in a window `window_width` wide; the default
/// when it isn't a number.
fn clamp_tree_width(width: f32, window_width: f32, list_space: f32) -> f32 {
    if !width.is_finite() {
        return DEFAULT_TREE_WIDTH;
    }
    let max =
        (window_width - list_space - TREE_DIVIDER_WIDTH - MIN_PANES_WIDTH).max(MIN_TREE_WIDTH);
    width.clamp(MIN_TREE_WIDTH, max)
}

/// What a new window shows first (spec §7.12).
pub enum StartWith {
    /// The start page.
    Page,
    /// A layout file given on the command line.
    File(PathBuf),
}

pub struct Workspace {
    hydra: Arc<Hydra>,
    ui: UiConfig,
    style: Arc<TermStyle>,
    /// The environments' tabs, their terminals and file panes (env-tabs design §2).
    envs: EnvTabs,
    panes: HashMap<PaneId, Entity<PaneView>>,
    /// File panes (spec §7.8), at most one per environment.
    files: HashMap<PaneId, Entity<FilePane>>,
    file_subs: HashMap<PaneId, Subscription>,
    /// Terminals started by a layout file's hand-written `command`, which is kept when the
    /// layout is saved again (env-tabs design §3).
    commands: HashMap<PaneId, String>,
    tints: HashMap<String, u32>,
    next_pane: PaneId,
    status: StatusCache,
    /// Each environment's borrowed logins and possible lenders, read with its accounts.
    borrowing: HashMap<String, Borrowing>,
    refreshing: HashSet<String>,
    /// The focused pane's repository summary, for the folder it was read in; `None` text when
    /// that folder isn't in a repository.
    git: Option<(PathBuf, Option<git_status::Summary>)>,
    /// A `git status` is running; `git_again` asks for another once it finishes.
    git_busy: bool,
    git_again: bool,
    _git_watch: Task<()>,
    /// The divider between the shown terminal and the file pane is being dragged.
    files_drag: bool,
    /// The middle's bounds at the last paint while a file pane shows, for that drag.
    middle_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    tree: Option<Entity<FileTreeView>>,
    tree_sub: Option<Subscription>,
    tree_width: f32,
    /// The divider between the panes and the tree is being dragged.
    tree_drag: bool,
    list_width: f32,
    /// The divider between the terminal list and the middle is being dragged.
    list_drag: bool,
    /// The terminal list is collapsed: the middle takes its room.
    list_hidden: bool,
    focus: FocusHandle,
    title: String,
    subs: HashMap<PaneId, Subscription>,
    _timer: Task<()>,
    /// The focused terminal's repository, watched so git marks follow changes at once.
    git_watch: Option<crate::git_watch::GitWatch>,
    git_tx: futures::channel::mpsc::UnboundedSender<()>,
    _git_pump: Task<()>,
    /// Each terminal's busy state for the tab strip, checked every second.
    activity: HashMap<PaneId, PaneActivity>,
    /// Terminals whose Claude session finished or asked for you while out of sight: their
    /// environment tab is badged until they are shown.
    attention: HashSet<PaneId>,
    /// The working dot's phase, flipped each second.
    blink: bool,
    _activity_tick: Task<()>,
    /// Whether the status bar's accounts popover is open.
    /// Each environment's latest Claude usage, from its sessions' status lines.
    usage: HashMap<String, crate::mcp::Usage>,
    /// The title bar's Update button (spec §7.11).
    update: crate::updater::UpdateState,
    _update_checks: Option<Task<()>>,
    /// The keys bound at startup, for the shortcuts list ([keys] changes wait for a restart).
    keymap: Vec<(crate::keys::Command, String)>,
    /// The layout file this window keeps saved (spec §7.12); `None` while untitled.
    layout: Option<PathBuf>,
    /// This window's hold on `layout`, so no other window writes it.
    claim: Option<crate::claim::Claim>,
    /// The layout's environments and terminals that didn't open, kept in what autosave
    /// writes.
    unopened: Layout,
    /// A layout is opening: autosave waits (the file already holds it).
    opening: bool,
    /// The start page, while it shows (spec §7.12).
    start: Option<start_page::StartPage>,
    /// The Settings tab, shown in the middle in place of a terminal.
    settings: Option<settings_tab::SettingsTab>,
    /// Terminals with a Tasks tab, and whether it's the one shown (`true`) or the terminal
    /// (task-board design §4).
    tasks_tabs: HashMap<PaneId, bool>,
    /// Orchestrators' Pipeline tabs (orchestrator spec §7.2).
    pipeline_tabs: HashMap<PaneId, pipeline_tab::PipelineTab>,
    /// Orchestrators started from the launcher, with the pipeline file each runs: their cards
    /// show before they have a ticket.
    orchestrators: HashMap<PaneId, String>,
    /// Runs open when an earlier hydraui closed, offered back as cards (orchestrator §13).
    interrupted: Vec<crate::run_resume::Interrupted>,
    /// Pipeline settings being edited, per orchestrator (orchestrator spec §7.4).
    pipeline_settings: HashMap<PaneId, pipeline_settings::SettingsDraft>,
    /// One board per project, shared by every Tasks tab on it; dropped when no tab uses it.
    boards: HashMap<PathBuf, (Entity<TaskBoard>, Subscription)>,
    /// One view per terminal's Tasks tab.
    board_views: HashMap<PaneId, Entity<TaskBoardView>>,
    /// Environments showing Tasks: all projects in place of their terminal.
    all_tasks: HashSet<String>,
    /// Each environment's Tasks: all projects lanes, one per project of its terminals.
    lanes: HashMap<String, Vec<all_tasks::Lane>>,
    /// Keeps the shown lanes in step with the terminals' projects.
    _lane_tick: Task<()>,
    /// Each board view's focus watch, which takes the focus from the file pane.
    board_focus: HashMap<PaneId, Subscription>,
    /// Each Tasks tab's board view events (a commit to show).
    board_events: HashMap<PaneId, Subscription>,
    /// Terminals a card was just sent to, and when: not candidates for another until their
    /// next hook state, or 30 s (auto-pickup design §6).
    reserved: HashMap<PaneId, Instant>,
    /// Each orchestrator's wake-up waiting to be typed, with its run (orchestrator spec §6): the
    /// newest only.
    pending_wakes: HashMap<PaneId, (String, String)>,
    /// Wake-ups typed but not yet seen to start the session working: (run, message, when,
    /// times sent). One that doesn't take (a Claude Code prompt was open) is sent again.
    sent_wakes: HashMap<PaneId, (String, String, Instant, u8)>,
    /// Times a wake-up waiting to be sent again was already sent.
    wake_tries: HashMap<PaneId, u8>,
    /// When each run waiting on product last had its stage look in the Slack thread.
    ask_checks: HashMap<String, std::time::SystemTime>,
    /// Each orchestrator's Linear watcher (orchestrator phase 2 §3).
    linear_watch: HashMap<PaneId, linear::Watch>,
    /// Pull requests a `[slack]` pipeline took and hasn't started yet, by pipeline file.
    slack_queues: HashMap<String, std::collections::VecDeque<crate::slack_watch::Queued>>,
    /// Pull requests a Slack watcher passed over because their author is the person watching,
    /// so GitHub isn't asked about them again.
    slack_mine: std::collections::HashSet<String>,
    /// The pull request each orchestrator was handed and hasn't started yet, to retry once.
    slack_inflight: HashMap<PaneId, crate::slack_watch::Queued>,
    /// Orchestrators whose Claude hasn't finished its first turn: a prompt typed then is lost,
    /// so nothing is handed to them until it has.
    /// (When it opened: after 90 seconds it counts as ready anyway.)
    starting_up: HashMap<PaneId, std::time::Instant>,
    /// Runs' tickets as Linear last said, by identifier: the card's SLA and link (phase 2 §5).
    linear_tickets: HashMap<String, crate::linear::Ticket>,
    /// Cards being offered or just sent, so one card never goes to two sessions.
    in_flight: crate::pickup::InFlight,
    /// Listens for board files the MCP server's sessions create.
    _mcp_pump: Option<gpui::Task<()>>,
    /// Coming back to the window clears the alert of the terminal it shows.
    _activation: Subscription,
}

impl Workspace {
    pub fn new(
        hydra: Arc<Hydra>,
        ui: UiConfig,
        style: TermStyle,
        problems: Vec<String>,
        start: StartWith,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Repository changes: wait for a burst to settle, then one `git status`.
        let (git_tx, mut git_rx) = futures::channel::mpsc::unbounded::<()>();
        let git_pump = cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while git_rx.next().await.is_some() {
                // Wait for 300 ms without changes (at most 2 s), so a long burst (a checkout,
                // a build) costs one `git status` at the end, not one per 300 ms.
                let start = std::time::Instant::now();
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(300))
                        .await;
                    let mut more = false;
                    while git_rx.try_recv().is_ok() {
                        more = true;
                    }
                    if !more || start.elapsed() >= std::time::Duration::from_secs(2) {
                        break;
                    }
                }
                if this.update(cx, |ws, cx| ws.refresh_git(cx)).is_err() {
                    break;
                }
            }
        });
        let activity_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this.update(cx, |ws, cx| ws.tick_activity(cx)).is_err() {
                    break;
                }
            }
        });
        let lane_tick = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this
                    .update_in(cx, |ws, window, cx| {
                        ws.sync_shown_lanes(window, cx);
                        // A draft cleared without a hook: a waiting wake-up may go now.
                        ws.send_wakes(window, cx);
                        // A Linear watcher whose check is due looks (phase 2 §3).
                        ws.linear_tick(window, cx);
                        // A stage waiting on product looks in its Slack thread.
                        ws.asking_tick(window, cx);
                        // A wake-up that didn't take is sent again.
                        ws.recheck_wakes(window, cx);
                        // The Pipeline tab's session times tick.
                        if ws
                            .envs
                            .active()
                            .and_then(|t| t.shown)
                            .is_some_and(|id| ws.pipeline_shown(id))
                        {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_TTL).await;
                if this.update(cx, |ws, cx| ws.refresh_status(cx)).is_err() {
                    break;
                }
            }
        });
        // Picks up edits made outside the terminal (an editor saving, another tool).
        let git_watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(15))
                    .await;
                if this.update(cx, |ws, cx| ws.refresh_git(cx)).is_err() {
                    break;
                }
            }
        });
        // A session created a board file (spec §5): ask the same question the Tasks tab does. A
        // Claude hook said what its session is doing: the terminal's dot shows it.
        let activation = cx.observe_window_activation(window, |ws: &mut Self, window, cx| {
            if window.is_window_active()
                && let Some(id) = ws.envs.active().and_then(|t| t.shown)
                && ws.attention.remove(&id)
            {
                cx.notify();
            }
        });
        if let Some(server) = crate::mcp::server::get() {
            // The weekly share a stage needs (orchestrator spec §5.1), from config.toml.
            crate::mcp::server::lock(server.registry()).min_week_left = ui.min_week_left;
        }
        let mcp_pump = crate::mcp::server::get().map(|server| {
            let mut rx = server.subscribe();
            cx.spawn_in(window, async move |this, cx| {
                use crate::mcp::server::McpEvent;
                use futures::StreamExt;
                while let Some(event) = rx.next().await {
                    let alive = this.update_in(cx, |ws, window, cx| match event {
                        McpEvent::BoardCreated(project) => ws.board_created(project, window, cx),
                        McpEvent::Session {
                            token,
                            state,
                            resumed,
                            ..
                        } => ws.session_state(&token, state, resumed, window, cx),
                        McpEvent::Usage(token, usage) => ws.usage_reported(&token, usage, cx),
                        McpEvent::Verified(token) => ws.session_verified(&token, cx),
                        McpEvent::StageStart(s) => ws.start_stage(s, window, cx),
                        McpEvent::RunChanged(changes) => ws.runs_changed(changes, window, cx),
                        McpEvent::Skill(..) => cx.notify(),
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
        });
        // After the window's Root exists: toasts and dialogs live in it.
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |ws, window, cx| ws.startup(problems, start, window, cx));
        })
        .detach();
        let interrupted = crate::run_resume::interrupted(hydra.paths.root(), crate::claim::alive);
        Self {
            hydra,
            keymap: crate::keys::keymap(&ui.keys).0,
            ui,
            style: Arc::new(style),
            envs: EnvTabs::default(),
            panes: HashMap::new(),
            files: HashMap::new(),
            file_subs: HashMap::new(),
            commands: HashMap::new(),
            tints: HashMap::new(),
            next_pane: 0,
            status: StatusCache::default(),
            borrowing: HashMap::new(),
            refreshing: HashSet::new(),
            git: None,
            git_busy: false,
            git_again: false,
            _git_watch: git_watch,
            files_drag: false,
            middle_bounds: Rc::default(),
            tree: None,
            tree_sub: None,
            tree_width: DEFAULT_TREE_WIDTH,
            tree_drag: false,
            list_width: DEFAULT_LIST_WIDTH,
            list_drag: false,
            list_hidden: false,
            focus: cx.focus_handle(),
            title: String::new(),
            subs: HashMap::new(),
            _timer: timer,
            git_watch: None,
            git_tx,
            _git_pump: git_pump,
            activity: HashMap::new(),
            attention: HashSet::new(),
            blink: true,
            _activity_tick: activity_tick,
            usage: HashMap::new(),
            update: Default::default(),
            _update_checks: None,
            layout: None,
            claim: None,
            unopened: Default::default(),
            opening: false,
            start: None,
            settings: None,
            tasks_tabs: HashMap::new(),
            pipeline_tabs: HashMap::new(),
            pipeline_settings: HashMap::new(),
            orchestrators: HashMap::new(),
            interrupted,
            boards: HashMap::new(),
            board_views: HashMap::new(),
            all_tasks: HashSet::new(),
            lanes: HashMap::new(),
            _lane_tick: lane_tick,
            board_focus: HashMap::new(),
            board_events: HashMap::new(),
            reserved: HashMap::new(),
            pending_wakes: HashMap::new(),
            sent_wakes: HashMap::new(),
            wake_tries: HashMap::new(),
            ask_checks: HashMap::new(),
            linear_watch: HashMap::new(),
            slack_queues: HashMap::new(),
            slack_mine: std::collections::HashSet::new(),
            slack_inflight: HashMap::new(),
            starting_up: HashMap::new(),
            linear_tickets: HashMap::new(),
            in_flight: Default::default(),
            _mcp_pump: mcp_pump,
            _activation: activation,
        }
    }

    /// Shows what was wrong with the settings, then the start page or the layout given.
    fn startup(
        &mut self,
        problems: Vec<String>,
        start: StartWith,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_update_checks(cx);
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        // A ticket left mid-run waits on its environment's card (orchestrator §13).
        for i in &self.interrupted {
            window.push_notification(
                Notification::info(format!(
                    "{} was running when hydraui closed. Open {} to resume it.",
                    i.run.ticket, i.env
                ))
                .title(i.run.pipeline.name.clone()),
                cx,
            );
        }
        // The page first: opening a file leaves it; a file that fails to open leaves the
        // page and its toast (§7.12 Command line); a file another window took meanwhile
        // closes this window, as a start page does.
        self.show_start_page(window, cx);
        if let StartWith::File(path) = start {
            self.open_layout_path(path, window, cx);
        }
        // Pipelines that start themselves (`[autostart]`), watching Linear when they ask to.
        self.autostart_pipelines(window, cx);
        // Each pane's folder as it is when the window closes.
        let ws = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            ws.update(cx, |ws, cx| {
                let saved = ws.finish_board_edits(window, cx);
                ws.autosave(window, cx);
                // A board edit whose save failed stays open, and so does the window; its toast
                // has said why (task board spec §1).
                if !saved {
                    return false;
                }
                // A ticket still running or waiting for a person ends with the window: ask.
                if let Some(warning) = crate::pipeline_card::quit_warning(&ws.open_runs(cx)) {
                    ws.confirm_close_runs(warning, window, cx);
                    return false;
                }
                // Unsaved edits: ask first, and close from the dialog (spec §7.8).
                let all: Vec<PaneId> = ws.files.keys().copied().collect();
                if ws.unsaved_in(&all, cx).is_empty() {
                    orchestrator::closing();
                    return true;
                }
                ws.confirm_unsaved(all, window, cx, |_, w, _| {
                    orchestrator::closing();
                    w.remove_window()
                });
                false
            })
            .unwrap_or(true)
        });
    }

    /// Smoke tests and screenshots can't press Ctrl+Shift: in debug builds F12 opens the dialog
    /// named by HYDRAUI_TEST_DIALOG (open_file, search, launcher, shortcuts).
    fn test_dialog(&mut self, _: &TestDialog, window: &mut Window, cx: &mut Context<Self>) {
        match std::env::var("HYDRAUI_TEST_DIALOG").as_deref() {
            Ok("open_file") => self.open_file_finder(window, cx),
            Ok("search") => self.search_dialog(window, cx),
            Ok("launcher") => self.open_launcher(&crate::keys::Launcher, window, cx),
            Ok("shortcuts") => self.shortcuts(window, cx),
            _ => {}
        }
    }

    /// The environment picker behind **+** and Ctrl+T with no environment open (env-tabs
    /// design §2.1): environments not in the window yet, then New environment…; the chosen
    /// one gets its tab.
    fn pick_env(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let all = self.hydra.env_names();
        if all.is_empty() {
            self.new_env_dialog(window, cx);
            return;
        }
        let envs: Vec<String> = all
            .into_iter()
            .filter(|e| self.envs.get(e).is_none())
            .collect();
        // Each environment with its colour, as on its tab.
        let swatches: Vec<(String, Hsla)> = envs
            .iter()
            .map(|e| (e.clone(), rgb(self.hydra.tint(e)).into()))
            .collect();
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (choices, workspace) = (envs.clone(), workspace.clone());
            dialog.title("Add an environment").child(
                Command::new(&state)
                    .items(
                        swatches
                            .iter()
                            .map(|(e, colour)| env_item(e, Some(*colour)))
                            .chain([env_item("New environment…", None)]),
                    )
                    .placeholder("Environment")
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        match choices.get(ix.row).cloned() {
                            Some(env) => {
                                workspace.update(cx, |ws, cx| ws.add_env(&env, window, cx))
                            }
                            None => workspace.update(cx, |ws, cx| ws.new_env_dialog(window, cx)),
                        }
                    }),
            )
        });
    }

    /// The shell new terminals start: hydraui's `default_shell`, else hydra's, else the platform's.
    fn shell(&self) -> hydra_platform::shell::ShellKind {
        self.hydra.shell(self.ui.default_shell.as_deref())
    }

    /// Where a new terminal in `env` starts: its root folder, else its shown terminal's folder,
    /// else the user's home (env-tabs design §2.2).
    fn env_cwd(&self, env: &str, cx: &App) -> PathBuf {
        let shown = self
            .envs
            .get(env)
            .and_then(|t| t.shown)
            .and_then(|id| self.panes.get(&id))
            .map(|v| v.read(cx).cwd().clone());
        new_terminal_cwd(
            shown.as_deref(),
            self.hydra.env_home(env).as_deref(),
            &self.hydra.user_home,
        )
    }

    /// Adds `env`'s tab, without terminals, and selects it; only selects it when it is open.
    /// True when the tab is new.
    fn add_env_tab(&mut self, env: &str) -> bool {
        if !self.tints.contains_key(env) {
            self.tints.insert(env.to_string(), self.hydra.tint(env));
        }
        self.envs.add_env(env)
    }

    /// Adds `env`'s tab with one new terminal, in its home else the user's (env-tabs design
    /// §2.1), or selects its tab when it is open already.
    pub(super) fn add_env(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.add_env_tab(env) {
            self.shown_changed(window, cx);
            return;
        }
        let cwd = new_terminal_cwd(
            None,
            self.hydra.env_home(env).as_deref(),
            &self.hydra.user_home,
        );
        self.new_terminal(env, Some(cwd), window, cx);
    }

    /// A new terminal in `env`'s open tab, shown and focused; with `cwd` `None` it starts
    /// where `env_cwd` says. `None` when `env` has no tab.
    pub(super) fn new_terminal(
        &mut self,
        env: &str,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        self.envs.get(env)?;
        let cwd = cwd.unwrap_or_else(|| self.env_cwd(env, cx));
        let request = PaneRequest::new(env, cwd, self.shell());
        Some(self.open_terminal(env, request, window, cx))
    }

    /// A terminal running `request` in `env`, adding `env`'s tab when it isn't open (sign-in,
    /// the launcher); shown and focused.
    pub(super) fn open_terminal(
        &mut self,
        env: &str,
        request: PaneRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneId {
        self.add_env_tab(env);
        let id = self.new_pane(request, window, cx);
        self.envs.add_terminal(env, id);
        // The terminal it replaces on screen may have had a board with an edit open.
        self.sync_boards(window, cx);
        self.focus_soon(id, window, cx);
        self.sync_tree(cx);
        self.autosave(window, cx);
        cx.notify();
        id
    }

    /// A terminal running `request` in `env`'s open tab, added without taking focus or changing
    /// what is shown (orchestrator spec §3.2); `None` when `env` has no tab.
    pub(super) fn open_terminal_beside(
        &mut self,
        env: &str,
        request: PaneRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        self.envs.get(env)?;
        let id = self.new_pane(request, window, cx);
        self.envs.add_terminal_quietly(env, id);
        self.autosave(window, cx);
        cx.notify();
        Some(id)
    }

    /// Shows terminal `id` in the middle, selecting its environment, and focuses it.
    pub(super) fn show_terminal(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Its environment goes back to showing a terminal; others keep what they show.
        let left = self
            .envs
            .env_of(id)
            .is_some_and(|env| self.all_tasks.remove(env))
            | self.settings.take().is_some();
        if self.envs.show(id) || left {
            self.shown_changed(window, cx);
        }
    }

    /// After the selected environment or its shown terminal changed: focus, tree, status and
    /// git follow it, and the layout is saved.
    fn shown_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.envs.active().and_then(|t| t.shown) {
            self.attention.remove(&id);
        }
        self.sync_boards(window, cx);
        self.focus_active(window, cx);
        self.sync_tree(cx);
        self.refresh_status(cx);
        self.refresh_git(cx);
        self.autosave(window, cx);
        cx.notify();
    }

    /// The shown terminal's folder, else the user's home.
    fn focused_cwd(&self, cx: &App) -> PathBuf {
        self.focused()
            .map(|v| v.read(cx).cwd().clone())
            .unwrap_or_else(|| self.hydra.user_home.clone())
    }

    fn new_pane(
        &mut self,
        request: PaneRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneId {
        self.next_pane += 1;
        let id = self.next_pane;
        self.create_pane(id, request, window, cx);
        id
    }

    fn create_pane(
        &mut self,
        id: PaneId,
        request: PaneRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tint = self.hydra.tint(&request.env);
        self.tints.insert(request.env.clone(), tint);
        let (hydra, style) = (self.hydra.clone(), self.style.clone());
        let view = cx.new(|cx| PaneView::new(id, request, hydra, style, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            move |ws, view, ev: &PaneEvent, window, cx| {
                if let PaneEvent::OpenPath(path, line, column) = ev {
                    ws.show_file_at(path.clone(), *line, *column, window, cx);
                    return;
                }
                if *ev == PaneEvent::Exited
                    && let Some(token) = view.read(cx).mcp_token().map(str::to_string)
                {
                    // The shell died: a stage it ran fails even with its terminal open.
                    ws.stage_session_gone(&token);
                }
                if *ev == PaneEvent::Focused {
                    // A click in a terminal shows it and selects its environment.
                    let before = ws.shown_place();
                    ws.envs.show(id);
                    if ws.shown_place() != before {
                        ws.sync_boards(window, cx);
                        ws.autosave(window, cx);
                    }
                    ws.refresh_status(cx);
                }
                // The shown terminal's repository: on focus, on `cd`, and when a command
                // finishes.
                let shown = ws.envs.active().and_then(|t| t.shown) == Some(id);
                let back_at_prompt = *ev == PaneEvent::PromptChanged
                    && view.read(cx).prompt() == Some(PromptState::AtPrompt);
                if shown
                    && (matches!(ev, PaneEvent::Focused | PaneEvent::CwdChanged) || back_at_prompt)
                {
                    ws.refresh_git(cx);
                }
                ws.sync_tree(cx);
                cx.notify();
            },
        );
        self.subs.insert(id, sub);
        self.panes.insert(id, view);
    }

    /// The selected environment and its shown terminal: what the layout saves as selected.
    fn shown_place(&self) -> Option<(String, Option<PaneId>)> {
        self.envs.active().map(|t| (t.env.clone(), t.shown))
    }

    /// The selected environment's shown terminal, which stands for the window (status bar,
    /// tree, git) also while the file pane has the focus.
    fn focused(&self) -> Option<Entity<PaneView>> {
        let id = self.envs.active()?.shown?;
        self.panes.get(&id).cloned()
    }

    fn focus_pane(&self, id: PaneId, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.files.get(&id) {
            view.update(cx, |f, cx| f.focus_content(window, cx));
            return;
        }
        // A terminal showing its Pipeline tab: the session the user stepped into, else none of
        // the hidden terminal.
        if self.pipeline_shown(id) {
            match self.pipeline_focus(id, cx).and_then(|p| self.panes.get(&p)) {
                Some(view) => {
                    let handle = view.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
                None => window.focus(&self.focus, cx),
            }
            return;
        }
        // A terminal showing its Tasks tab: the board.
        if let Some(board) = self.shown_board(id) {
            let handle = board.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            return;
        }
        match self.panes.get(&id) {
            Some(view) => {
                let handle = view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            None => window.focus(&self.focus, cx),
        }
    }

    fn create_files_pane(
        &mut self,
        id: PaneId,
        files: &[PathBuf],
        active: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| FilePane::new(files, active, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            move |ws, _, ev: &FilePaneEvent, window, cx| match ev {
                FilePaneEvent::Focused => {
                    if let Some(env) = ws.envs.env_of(id).map(str::to_string)
                        && let Some(tab) = ws.envs.get_mut(&env)
                    {
                        tab.files_focused = true;
                    }
                    cx.notify();
                }
                FilePaneEvent::Changed => ws.autosave(window, cx),
                FilePaneEvent::Empty => ws.remove_files_pane(id, window, cx),
                FilePaneEvent::OpenInEditor(path) => ws.open_file(path, window, cx),
            },
        );
        self.file_subs.insert(id, sub);
        self.files.insert(id, view);
    }

    /// Shows `path` in the selected environment's file pane, opening the pane beside the shown
    /// terminal first when the environment has none (spec §7.8, env-tabs design §2.4).
    fn show_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.show_file_at(path, 0, 0, window, cx);
    }

    /// `show_file` at a 1-based line and column (0: just open it), for quick open, search and
    /// clickable paths (spec §7.8, §7.10).
    fn show_file_at(
        &mut self,
        path: PathBuf,
        line: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.envs.active() else {
            return;
        };
        let env = tab.env.clone();
        let id = match tab.files.filter(|f| self.files.contains_key(f)) {
            Some(id) => {
                if let Some(view) = self.files.get(&id).cloned() {
                    view.update(cx, |f, cx| {
                        if line > 0 {
                            f.open_at(path, line, column, window, cx)
                        } else {
                            f.open(path, window, cx)
                        }
                    });
                }
                id
            }
            None => {
                self.next_pane += 1;
                let id = self.next_pane;
                self.create_files_pane(id, std::slice::from_ref(&path), 0, window, cx);
                if line > 0
                    && let Some(view) = self.files.get(&id).cloned()
                {
                    view.update(cx, |f, cx| f.open_at(path, line, column, window, cx));
                }
                self.envs.set_files(&env, Some(id));
                id
            }
        };
        if let Some(tab) = self.envs.get_mut(&env) {
            tab.files_focused = true;
        }
        self.focus_pane(id, window, cx);
        self.autosave(window, cx);
        cx.notify();
    }

    fn remove_files_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(env) = self.envs.env_of(id).map(str::to_string) {
            self.envs.set_files(&env, None);
        }
        self.files.remove(&id);
        self.file_subs.remove(&id);
        self.focus_active(window, cx);
        self.autosave(window, cx);
        cx.notify();
    }

    /// The files with unsaved edits among `panes`.
    fn unsaved_in(&self, panes: &[PaneId], cx: &App) -> Vec<PathBuf> {
        panes
            .iter()
            .filter_map(|p| self.files.get(p))
            .flat_map(|f| f.read(cx).unsaved())
            .collect()
    }

    /// Runs `then` now, or after asking Save all / Discard / Cancel when files among `panes`
    /// have unsaved edits.
    fn confirm_unsaved(
        &mut self,
        panes: Vec<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let unsaved = self.unsaved_in(&panes, cx);
        if unsaved.is_empty() {
            then(self, window, cx);
            return;
        }
        let names: Vec<String> = unsaved
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        let ws = cx.entity();
        let then = Rc::new(then);
        window.open_dialog(cx, move |dialog, _, _| {
            let (save_ws, discard_ws, panes) = (ws.clone(), ws.clone(), panes.clone());
            let (save_then, discard_then) = (then.clone(), then.clone());
            dialog
                .title("Unsaved edits")
                .child(div().child(format!("Unsaved: {}", names.join(", "))))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("discard")
                                .label("Discard")
                                .on_click(move |_, w, cx| {
                                    w.close_dialog(cx);
                                    discard_ws.update(cx, |ws, cx| discard_then(ws, w, cx));
                                }),
                        )
                        .child(Button::new("save").label("Save all").primary().on_click(
                            move |_, w, cx| {
                                w.close_dialog(cx);
                                save_ws.update(cx, |ws, cx| {
                                    let mut failed = None;
                                    for p in &panes {
                                        if let Some(f) = ws.files.get(p).cloned()
                                            && let Err(e) = f.update(cx, |f, cx| f.save_all(cx))
                                        {
                                            failed = Some(e);
                                        }
                                    }
                                    match failed {
                                        Some(e) => w.push_notification(
                                            Notification::error(e).title("Save"),
                                            cx,
                                        ),
                                        None => save_then(ws, w, cx),
                                    }
                                });
                            },
                        )),
                )
        });
    }

    /// Focuses now and again on the next frame, after a closing dialog has handed the focus
    /// back to where it was.
    fn focus_soon(&self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane(id, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |ws, window, cx| ws.focus_pane(id, window, cx));
        })
        .detach();
    }

    /// Focuses the selected environment's file pane when it had the focus, else its shown
    /// terminal; with neither, the window.
    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if self.settings_shown() {
            window.focus(&self.focus, cx);
            return;
        }
        if self.all_tasks_shown() {
            match self.all_tasks_focus(cx) {
                Some(handle) => window.focus(&handle, cx),
                None => window.focus(&self.focus, cx),
            }
            return;
        }
        let target = self.envs.active().and_then(|t| match t.files {
            Some(f) if t.files_focused => Some(f),
            _ => t.shown,
        });
        match target {
            Some(id) => self.focus_pane(id, window, cx),
            None => window.focus(&self.focus, cx),
        }
    }

    /// Ctrl+T: a terminal in the selected environment; with none open, the picker.
    fn new_terminal_action(
        &mut self,
        _: &NewTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.envs.active().map(|t| t.env.clone()) {
            Some(env) => {
                self.new_terminal(&env, None, window, cx);
            }
            None => self.pick_env(window, cx),
        }
    }

    fn drop_pane(&mut self, id: PaneId) {
        self.pipeline_tabs.remove(&id);
        self.pipeline_settings.remove(&id);
        self.orchestrators.remove(&id);
        // Dropping the view drops its Pane: the job ends the shell's whole process tree.
        self.panes.remove(&id);
        self.subs.remove(&id);
        self.files.remove(&id);
        self.file_subs.remove(&id);
        self.commands.remove(&id);
        self.attention.remove(&id);
    }

    /// Ctrl+W: the file pane when it has the focus, else the shown terminal's Tasks tab when
    /// it's selected, else the shown terminal.
    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.envs.active() else {
            return;
        };
        if tab.files_focused
            && let Some(id) = tab.files
        {
            self.confirm_unsaved(vec![id], window, cx, move |ws, w, cx| {
                ws.remove_files_pane(id, w, cx)
            });
            return;
        }
        if let Some(id) = self.visible_tasks() {
            self.close_tasks(id, window, cx);
            return;
        }
        // Tasks: all projects closes back to the terminal; the cards stay.
        if self.all_tasks_shown() {
            match tab.shown {
                Some(id) => self.show_terminal(id, window, cx),
                None => {
                    self.leave_all_tasks();
                    cx.notify();
                }
            }
            return;
        }
        if let Some(id) = tab.shown {
            self.close_terminal(id, window, cx);
        }
    }

    /// Closes one terminal, asking first when a command is running in it; the next in its
    /// list shows, else the previous. The environment keeps its tab when it was the last
    /// (env-tabs design §2.2).
    pub(super) fn close_terminal(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.panes.get(&id).map(|v| v.read(cx).label()) else {
            return;
        };
        let running = self.running(&[id], cx);
        self.ask_running("Close", &name, &running, window, cx, move |ws, w, cx| {
            ws.close_terminal_now(id, w, cx)
        });
    }

    fn close_terminal_now(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.envs.close_terminal(id) {
            return;
        }
        self.drop_tasks(id, window, cx);
        self.drop_pane(id);
        self.shown_changed(window, cx);
    }

    /// Closes an environment's tab, its terminals and its file pane, asking first about
    /// unsaved edits and running commands (env-tabs design §2.1).
    pub(super) fn close_env(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        let panes: Vec<PaneId> = self
            .envs
            .get(env)
            .map(|t| t.terminals.iter().copied().chain(t.files).collect())
            .unwrap_or_default();
        let env = env.to_string();
        self.confirm_unsaved(panes, window, cx, move |ws, w, cx| {
            let terminals = ws
                .envs
                .get(&env)
                .map(|t| t.terminals.clone())
                .unwrap_or_default();
            let running = ws.running(&terminals, cx);
            let closing = env.clone();
            ws.ask_running("Close", &env, &running, w, cx, move |ws, w, cx| {
                ws.close_env_now(&closing, w, cx)
            });
        });
    }

    fn close_env_now(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        for id in self.envs.close_env(env) {
            self.drop_tasks(id, window, cx);
            self.drop_pane(id);
        }
        self.drop_lanes(env, window, cx);
        crate::window_layout::forget_env(&mut self.unopened, env);
        self.shown_changed(window, cx);
    }

    /// Restarts an environment's terminals so an edited env.toml applies, asking first when
    /// commands are running (env-tabs design §2.1).
    pub(super) fn restart_env(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        let terminals = self
            .envs
            .get(env)
            .map(|t| t.terminals.clone())
            .unwrap_or_default();
        self.restart_asking(env, terminals, window, cx);
    }

    /// Restarts one terminal, asking first when a command is running in it.
    pub(super) fn restart_terminal(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.panes.get(&id).map(|v| v.read(cx).label()) else {
            return;
        };
        self.restart_asking(&name, vec![id], window, cx);
    }

    fn restart_asking(
        &mut self,
        what: &str,
        terminals: Vec<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let running = self.running(&terminals, cx);
        self.ask_running("Restart", what, &running, window, cx, move |ws, _, cx| {
            // Each keeps its place, name and folder; a startup command runs again.
            for id in &terminals {
                if let Some(view) = ws.panes.get(id) {
                    view.update(cx, |v, cx| v.restart(cx));
                }
            }
        });
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.envs.next();
        self.shown_changed(window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.envs.previous();
        self.shown_changed(window, cx);
    }

    fn next_terminal(&mut self, _: &NextTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.step_terminal(1, window, cx);
    }

    fn previous_terminal(
        &mut self,
        _: &PreviousTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_terminal(-1, window, cx);
    }

    /// Checks the focused environment's accounts off the UI thread, at most once a minute.
    /// Reads the focused pane's repository off the UI thread; one `git status` at a time.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_busy {
            self.git_again = true;
            return;
        }
        let cwd = self.tree_folder(cx);
        self.git_busy = true;
        cx.spawn(async move |this, cx| {
            let dir = cwd.clone();
            let status = cx
                .background_spawn(async move { git_status::read(&dir) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(tree) = ws.tree.clone() {
                    let (root, files, renamed) = status
                        .as_ref()
                        .map(|s| (s.root.clone(), s.files.clone(), s.renamed_from.clone()))
                        .unwrap_or_default();
                    tree.update(cx, |t, cx| t.set_git(root, files, renamed, cx));
                }
                // Watch this repository (and only it) for changes.
                let root = status.as_ref().and_then(|s| s.root.clone());
                if ws.git_watch.as_ref().map(|w| &w.root) != root.as_ref() {
                    ws.git_watch =
                        root.and_then(|r| crate::git_watch::GitWatch::new(r, ws.git_tx.clone()));
                }
                let line = status.as_ref().map(git_status::summary_parts);
                ws.git = Some((cwd, line));
                ws.git_busy = false;
                if std::mem::take(&mut ws.git_again) {
                    ws.refresh_git(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        if let Some(env) = self.focused().map(|v| v.read(cx).env().to_string()) {
            self.refresh_status_of(env, cx);
        }
    }

    /// Checks `env`'s accounts in the background unless a recent check stands.
    fn refresh_status_of(&mut self, env: String, cx: &mut Context<Self>) {
        if self.status.is_fresh(&env, Instant::now()) || !self.refreshing.insert(env.clone()) {
            return;
        }
        let hydra = self.hydra.clone();
        cx.spawn(async move |this, cx| {
            let e = env.clone();
            let (reports, borrowing) = cx
                .background_spawn(async move { (hydra.whoami(&e), hydra.borrowing(&e)) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                ws.refreshing.remove(&env);
                ws.borrowing.insert(env.clone(), borrowing);
                ws.status.store(env, reports, Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_tree(&mut self, _: &FileTree, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree.take().is_some() {
            self.tree_sub = None;
        } else {
            let root = self.focused_cwd(cx);
            let view = cx.new(|cx| FileTreeView::new(root, cx));
            self.tree_sub = Some(cx.subscribe_in(
                &view,
                window,
                |ws, _, ev: &TreeEvent, window, cx| ws.on_tree_event(ev, window, cx),
            ));
            self.tree = Some(view);
            // A wide list may leave less room than the stored width.
            let width = f32::from(window.viewport_size().width);
            self.tree_width = clamp_tree_width(self.tree_width, width, self.list_space());
            self.sync_tree(cx);
        }
        self.autosave(window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Collapses or brings back the terminal list. Coming back, it keeps its width unless the
    /// tree now leaves less room.
    fn toggle_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_hidden = !self.list_hidden;
        if !self.list_hidden {
            let width = f32::from(window.viewport_size().width);
            self.list_width = clamp_list_width(self.list_width, width, self.tree_space());
        }
        self.autosave(window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The folder the tree and git marks follow: the shown terminal's, else the selected
    /// environment's home, else the user's home (an environment may have no terminal).
    fn tree_folder(&self, cx: &App) -> PathBuf {
        let shown = self.focused().map(|v| v.read(cx).cwd().clone());
        let home = self.envs.active().and_then(|t| self.hydra.env_home(&t.env));
        shown
            .or(home)
            .unwrap_or_else(|| self.hydra.user_home.clone())
    }

    /// Roots the tree at the focused pane's folder; `cd` only while it is at its prompt.
    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let cwd = self.tree_folder(cx);
        let at_prompt = self
            .focused()
            .is_some_and(|v| v.read(cx).prompt() == Some(PromptState::AtPrompt));
        tree.update(cx, |t, cx| {
            t.set_root(cwd, cx);
            t.set_can_cd(at_prompt, cx);
        });
    }

    fn on_tree_event(&mut self, ev: &TreeEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The file operations don't need a terminal.
        match ev {
            TreeEvent::NewFile(dir) => {
                return self.name_dialog(NameFor::File, dir.clone(), window, cx);
            }
            TreeEvent::NewFolder(dir) => {
                return self.name_dialog(NameFor::Folder, dir.clone(), window, cx);
            }
            TreeEvent::Rename(path) => {
                return self.name_dialog(NameFor::Rename, path.clone(), window, cx);
            }
            TreeEvent::Delete(path) => return self.delete_dialog(path.clone(), window, cx),
            TreeEvent::Reveal(path) => {
                if let Err(e) = crate::file_ops::reveal_in_explorer(path) {
                    window.push_notification(Notification::error(e), cx);
                }
                return;
            }
            TreeEvent::Copy(text) => {
                return cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            }
            // A terminal in the selected environment, starting there (env-tabs design §2.5).
            TreeEvent::NewTerminal(dir) => {
                if let Some(env) = self.envs.active().map(|t| t.env.clone()) {
                    self.new_terminal(&env, Some(dir.clone()), window, cx);
                }
                return;
            }
            _ => {}
        }
        let Some(view) = self.focused() else {
            return;
        };
        match ev {
            TreeEvent::Insert(path) => view.update(cx, |v, cx| {
                let text = format!("{} ", quote_path(v.request.shell, path));
                v.insert_text(&text, cx);
            }),
            TreeEvent::Cd(dir) => view.update(cx, |v, cx| {
                if v.prompt() == Some(PromptState::AtPrompt) {
                    let text = cd_command(v.request.shell, dir);
                    v.insert_text(&text, cx);
                }
            }),
            TreeEvent::Show(path) => self.show_file(path.clone(), window, cx),
            TreeEvent::ShowChanges(path) => self.show_changes(path.clone(), window, cx),
            // Handled above.
            TreeEvent::NewFile(_)
            | TreeEvent::NewFolder(_)
            | TreeEvent::NewTerminal(_)
            | TreeEvent::Reveal(_)
            | TreeEvent::Rename(_)
            | TreeEvent::Delete(_)
            | TreeEvent::Copy(_) => {}
        }
        self.focus_active(window, cx);
    }

    /// Diffs a file against git's last commit off the UI thread, then shows it beside the file
    /// in the environment's file pane (spec §7.3).
    fn show_changes(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some((root, rel, base)) = self.tree.as_ref().and_then(|t| t.read(cx).git_place(&path))
        else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let patch = cx
                .background_spawn(async move { git_status::diff_patch(&root, &rel, &base) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.show_file(path.clone(), window, cx);
                let pane = ws
                    .envs
                    .active()
                    .and_then(|tab| tab.files)
                    .and_then(|p| ws.files.get(&p).cloned());
                if let Some(pane) = pane {
                    pane.update(cx, |f, cx| f.open_diff(path, patch, window, cx));
                }
            });
        })
        .detach();
    }

    /// Shows what `commit` changed in `project` (a Verify card's commit, task board spec §5) in
    /// the selected environment's file pane, opening the pane first when it has none.
    pub(super) fn show_commit(
        &mut self,
        project: PathBuf,
        commit: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let repo = project.clone();
            let hash = commit.clone();
            let patch = cx
                .background_spawn(async move { git_status::commit_patch(&repo, &hash) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                let Some(env) = ws.envs.active().map(|t| t.env.clone()) else {
                    return;
                };
                let id = match ws.envs.get(&env).and_then(|t| t.files) {
                    Some(id) if ws.files.contains_key(&id) => id,
                    _ => {
                        ws.next_pane += 1;
                        let id = ws.next_pane;
                        ws.create_files_pane(id, &[], 0, window, cx);
                        ws.envs.set_files(&env, Some(id));
                        id
                    }
                };
                // The tab is named after the commit: `<hash> (diff)`.
                let path = project.join(&commit);
                if let Some(pane) = ws.files.get(&id).cloned() {
                    pane.update(cx, |f, cx| f.open_diff(path, patch, window, cx));
                }
                if let Some(tab) = ws.envs.get_mut(&env) {
                    tab.files_focused = true;
                }
                ws.focus_pane(id, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens a file in `editor` (a program name or path; `.cmd` launchers such as VS Code's
    /// `code` work), else with the OS default.
    fn open_file(&self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.ui.editor.as_deref() else {
            cx.open_with_system(path);
            return;
        };
        let search = std::env::var_os("PATH").unwrap_or_default();
        let mut cmd = editor_command(
            editor,
            path,
            &search,
            std::env::var_os("PATHEXT").as_deref(),
        );
        if let Err(e) = cmd.spawn() {
            window.push_notification(
                Notification::error(format!("can't start {editor}: {e}")).title("Open file"),
                cx,
            );
        }
    }

    fn tint(&self, env: &str) -> Hsla {
        rgb(self.tints.get(env).copied().unwrap_or(0x808080)).into()
    }

    /// The list's width plus its divider, 0 while it is collapsed.
    fn list_space(&self) -> f32 {
        if self.list_hidden {
            0.0
        } else {
            self.list_width + LIST_DIVIDER_WIDTH
        }
    }

    /// The tree's width plus its divider, 0 while it is closed.
    fn tree_space(&self) -> f32 {
        if self.tree.is_some() {
            self.tree_width + TREE_DIVIDER_WIDTH
        } else {
            0.0
        }
    }

    fn drag_to(&mut self, pos: Point<Pixels>, window_width: f32, cx: &mut Context<Self>) {
        // The list starts at the window's left edge, so its width is the pointer's x.
        if self.list_drag {
            self.list_width = list_width_at(f32::from(pos.x), window_width, self.tree_space());
            cx.notify();
            return;
        }
        // The tree runs to the window's right edge, so its width is measured from there.
        if self.tree_drag {
            self.tree_width = tree_width_at(f32::from(pos.x), window_width, self.list_space());
            cx.notify();
            return;
        }
        if !self.files_drag {
            return;
        }
        let Some(b) = self.middle_bounds.get() else {
            return;
        };
        let ratio = (pos.x - b.origin.x) / b.size.width;
        if let Some(tab) = self.envs.active_mut()
            && ratio.is_finite()
        {
            tab.files_ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
        }
        cx.notify();
    }

    /// Window-wide move/up listeners while a divider is dragged, so the drag keeps going
    /// when the pointer leaves the divider.
    fn drag_listener(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ws = cx.entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let on_move = ws.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let width = f32::from(window.viewport_size().width);
                        on_move.update(cx, |ws, cx| ws.drag_to(ev.position, width, cx));
                    }
                });
                let on_up = ws.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_up.update(cx, |ws, cx| {
                            ws.files_drag = false;
                            // The list's and the tree's widths are saved in the layout.
                            let list = std::mem::take(&mut ws.list_drag);
                            if std::mem::take(&mut ws.tree_drag) || list {
                                ws.autosave(window, cx);
                            }
                            cx.notify();
                        });
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The middle (env-tabs design §2.3, §2.4): the environment's shown terminal outlined in its
    /// colour, or a New terminal button when it has none; its file pane on the right, with a
    /// draggable divider, outlined while it has the focus.
    fn render_env(
        &self,
        tab: &EnvTab,
        border: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A pipeline's terminal (its orchestrator or a stage's session) is framed in the
        // pipeline's own colour, as its block on the left is.
        let tint = tab
            .shown
            .filter(|_| !self.all_tasks.contains(&tab.env))
            .and_then(|id| self.pipeline_tint(id, cx))
            .unwrap_or_else(|| self.tint(&tab.env));
        let terminal = match tab.shown.and_then(|id| Some((id, self.panes.get(&id)?))) {
            _ if self.all_tasks.contains(&tab.env) => {
                self.render_all_tasks(tint, muted, border, cx)
            }
            // With a Tasks tab: its strip above the terminal or its board (task-board §4).
            Some((id, view))
                if self.tasks_tabs.contains_key(&id) || self.pipeline_tabs.contains_key(&id) =>
            {
                let label = view.read(cx).label();
                let content = match self.shown_board(id) {
                    _ if self.pipeline_shown(id) => self.render_pipeline(id, cx),
                    Some(board) => board.clone().into_any_element(),
                    None => view.clone().into_any_element(),
                };
                v_flex()
                    .size_full()
                    .child(self.render_tasks_strip(id, label, tint, muted, border, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .border_1()
                            .border_color(tint)
                            .child(content),
                    )
                    .into_any_element()
            }
            Some((_, view)) => div()
                .size_full()
                .border_1()
                .border_color(tint)
                .child(view.clone())
                .into_any_element(),
            None => {
                let env = tab.env.clone();
                v_flex()
                    .size_full()
                    .gap_2()
                    .items_center()
                    .justify_center()
                    .text_color(muted)
                    .child(format!("No terminals in {env}."))
                    .child(
                        Button::new("empty-new-terminal")
                            .label("New terminal")
                            .primary()
                            .small()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.new_terminal(&env, None, window, cx);
                            })),
                    )
                    .into_any_element()
            }
        };
        let Some(files) = tab.files.and_then(|id| self.files.get(&id)) else {
            return terminal;
        };
        let edge = if tab.files_focused {
            cx.theme().accent
        } else {
            transparent_black()
        };
        let store = self.middle_bounds.clone();
        h_flex()
            .size_full()
            .relative()
            .child(
                canvas(move |bounds, _, _| store.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(
                div()
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .w(relative(tab.files_ratio))
                    .child(terminal),
            )
            .child(
                div()
                    .id("files-divider")
                    .w(px(4.))
                    .h_full()
                    .flex_none()
                    .bg(border)
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, _, cx| {
                            ws.files_drag = true;
                            cx.notify();
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .border_1()
                    .border_color(edge)
                    .child(files.clone()),
            )
            .into_any_element()
    }

    /// One tab per environment, in the layout's order (env-tabs design §2.1): its colour, its
    /// terminals' busiest activity, ×, and a right-click menu; **+** adds an environment.
    fn render_tab_strip(
        &self,
        bg: Hsla,
        border: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.envs.active().map(|t| t.env.clone());
        let mut strip = h_flex()
            .h(px(34.))
            .px_1()
            .gap_1()
            .bg(bg)
            .border_b_1()
            .border_color(border)
            // The terminal list, at the end nearest it; highlighted while shown.
            .child(
                Button::new("toggle-terminal-list")
                    .icon(IconName::PanelLeft)
                    .ghost()
                    .small()
                    .selected(!self.list_hidden)
                    .tooltip("Terminal list")
                    .on_click(cx.listener(|ws, _, window, cx| ws.toggle_list(window, cx))),
            );
        for tab in self.envs.iter() {
            let env = tab.env.clone();
            let tint = self.tint(&env);
            let busy = self.tab_activity(&tab.terminals);
            let dot =
                self.render_tab_dot(SharedString::from(format!("env-dot-{env}")), busy, tint, cx);
            let (click_env, close_env) = (env.clone(), env.clone());
            let close_id = SharedString::from(format!("env-close-{env}"));
            strip = strip.child(
                h_flex()
                    // Keyed by name, so element state stays with its environment when one
                    // before it closes.
                    .id(SharedString::from(format!("env-tab-{env}")))
                    .px_2()
                    .py_1()
                    .gap_2()
                    .rounded_md()
                    .when(active.as_deref() == Some(env.as_str()), |d| {
                        d.bg(tint.opacity(0.25))
                    })
                    .hover(|d| d.bg(tint.opacity(0.15)))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.envs.activate(&click_env);
                        ws.shown_changed(window, cx);
                    }))
                    .child(dot)
                    .child(div().text_sm().child(env.clone()))
                    // A Claude session here finished or needs you, out of sight.
                    .when(
                        tab.terminals.iter().any(|t| self.attention.contains(t)),
                        |d| d.child(div().size(px(7.)).rounded_full().bg(cx.theme().warning)),
                    )
                    .when_some(busy, |d, a| {
                        d.tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(busy::tooltip_text(a))
                                .build(window, cx)
                        })
                    })
                    .context_menu({
                        let ws = cx.entity();
                        move |menu, _, _| {
                            let (new_ws, restart_ws, close_ws) =
                                (ws.clone(), ws.clone(), ws.clone());
                            let (new_env, restart_env, close_env) =
                                (env.clone(), env.clone(), env.clone());
                            menu.item(PopupMenuItem::new("New terminal").on_click(
                                move |_, window, cx| {
                                    new_ws.update(cx, |ws, cx| {
                                        ws.new_terminal(&new_env, None, window, cx);
                                    });
                                },
                            ))
                            .item(PopupMenuItem::new("Restart environment").on_click(
                                move |_, window, cx| {
                                    restart_ws.update(cx, |ws, cx| {
                                        ws.restart_env(&restart_env, window, cx)
                                    });
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Close environment").on_click(
                                    move |_, window, cx| {
                                        close_ws.update(cx, |ws, cx| {
                                            ws.close_env(&close_env, window, cx)
                                        });
                                    },
                                ),
                            )
                        }
                    })
                    .child(
                        div()
                            .id(close_id)
                            .px_1()
                            .text_color(muted)
                            .hover(|d| d.text_color(tint))
                            .child("×")
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                cx.stop_propagation();
                                ws.close_env(&close_env, window, cx);
                            })),
                    ),
            );
        }
        strip
            .child(
                div()
                    .id("add-env")
                    .px_2()
                    .text_color(muted)
                    .child("+")
                    .on_click(cx.listener(|ws, _, window, cx| ws.pick_env(window, cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("open-settings")
                    .icon(IconName::Settings)
                    .ghost()
                    .small()
                    .selected(self.settings.is_some())
                    .tooltip("Settings")
                    .on_click(cx.listener(|ws, _, window, cx| {
                        if ws.settings_shown() {
                            ws.close_settings(window, cx)
                        } else {
                            ws.open_settings(settings_tab::Section::General, window, cx)
                        }
                    })),
            )
            // The file tree, as Ctrl+B, at the end nearest the tree; highlighted while open.
            .child(
                Button::new("toggle-file-tree")
                    .icon(IconName::Folder)
                    .ghost()
                    .small()
                    .selected(self.tree.is_some())
                    .tooltip("File tree (Ctrl+B)")
                    .on_click(
                        cx.listener(|ws, _, window, cx| ws.toggle_tree(&FileTree, window, cx)),
                    ),
            )
    }

    fn render_status(
        &self,
        border: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let mut bar = h_flex()
            .h(px(26.))
            .px_2()
            .gap_3()
            .text_sm()
            .border_t_1()
            .border_color(border)
            .text_color(muted);
        if let Some(view) = self.focused().filter(|_| self.start.is_none()) {
            let env = view.read(cx).env().to_string();
            let env_name = self.render_env_accounts(env.clone(), self.tint(&env), cx);
            let usage = self.render_usage(&env, cx);
            let v = view.read(cx);
            let (ok, bad) = (cx.theme().success, cx.theme().danger);
            let accounts = match self.status.get(&env).map(account_marks) {
                None => h_flex().child("…"),
                Some(marks) => h_flex()
                    .gap_3()
                    .children(marks.into_iter().map(|(tool, good)| {
                        h_flex().gap_1().child(tool).child(
                            div()
                                .text_color(if good { ok } else { bad })
                                .child(if good { "✓" } else { "✗" }),
                        )
                    })),
            };
            // Only a summary read in this pane's current folder; a stale one would mislead.
            let git = match &self.git {
                Some((dir, Some(parts))) if dir == v.cwd() => {
                    Some(git_summary(parts, muted, cx.theme()))
                }
                _ => None,
            };
            // The orchestrator's state (the design's status text), red when it needs you.
            let orchestrator = self.shown_run(cx).map(|(_, run)| {
                let (text, red) = crate::pipeline_card::status_text(&run);
                div()
                    .flex_none()
                    .max_w(px(520.))
                    .truncate()
                    .when(red, |d| d.text_color(cx.theme().danger))
                    .child(text)
            });
            bar = bar
                .child(env_name)
                .children(orchestrator)
                .child(div().flex_1().min_w_0().overflow_hidden().child(accounts))
                .when_some(usage, |b, u| b.child(u))
                .when_some(git, |b, line| b.child(line))
                .child(div().child(v.cwd().display().to_string()));
        }
        bar
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.keep_start_focus(window, cx);
        let title =
            crate::window_layout::window_title(self.layout.as_deref(), self.is_start_page());
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let theme = cx.theme();
        let (bg, fg, muted, border, bar) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.tab_bar,
        );
        let body = match self.envs.active() {
            // Settings opened from the start page shows over it; closing it goes back.
            _ if self.settings.is_some() => self.render_settings(cx),
            _ if self.start.is_some() => self.render_start_page(cx),
            Some(tab) => self.render_env(tab, border, muted, cx),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child("No environments open. + adds one.")
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(Self::new_terminal_action))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::next_terminal))
            .on_action(cx.listener(Self::previous_terminal))
            .on_action(cx.listener(Self::open_launcher))
            .on_action(cx.listener(Self::toggle_tree))
            .on_action(cx.listener(Self::open_file_action))
            .on_action(cx.listener(Self::search_action))
            .on_action(cx.listener(Self::test_dialog))
            .child(self.render_title_bar(cx))
            .when(self.start.is_none(), |d| {
                d.child(self.render_tab_strip(bar, border, muted, cx))
            })
            // The terminal list, the middle, then the tree on the right (env-tabs design §2).
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .when(
                        self.start.is_none() && self.envs.active().is_some() && !self.list_hidden,
                        |d| {
                            d.child(self.render_terminal_list(cx)).child(
                                div()
                                    .id("list-divider")
                                    .w(px(LIST_DIVIDER_WIDTH))
                                    .h_full()
                                    .flex_none()
                                    .bg(border)
                                    .cursor(CursorStyle::ResizeLeftRight)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|ws, _, _, cx| {
                                            ws.list_drag = true;
                                            cx.notify();
                                        }),
                                    ),
                            )
                        },
                    )
                    .child(div().flex_1().min_w_0().h_full().child(body))
                    .when_some(self.tree.clone(), |d, tree| {
                        d.child(
                            div()
                                .id("tree-divider")
                                .w(px(TREE_DIVIDER_WIDTH))
                                .h_full()
                                .flex_none()
                                .bg(border)
                                .cursor(CursorStyle::ResizeLeftRight)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|ws, _, _, cx| {
                                        ws.tree_drag = true;
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .w(px(self.tree_width))
                                .h_full()
                                .flex_none()
                                .child(tree),
                        )
                    }),
            )
            .child(self.render_status(border, muted, cx))
            .when(self.files_drag || self.tree_drag || self.list_drag, |d| {
                d.child(self.drag_listener(cx))
            })
    }
}

/// The status bar's git summary, each piece in its colour: the counts as the file tree
/// colours its marks (staged and untracked green, modified amber, conflicts red),
/// ahead/behind blue, the branch and separator muted, clean green.
fn git_summary(
    parts: &[(String, git_status::GitMark)],
    muted: Hsla,
    theme: &gpui_component::theme::ThemeColor,
) -> gpui::Div {
    use git_status::GitMark::*;
    h_flex()
        .flex_none()
        .gap_1()
        .children(parts.iter().map(|(text, mark)| {
            let colour = match mark {
                Branch | Separator => muted,
                Ahead | Behind => theme.info,
                Staged | Untracked | Clean => theme.success,
                Modified => theme.warning,
                Conflicts => theme.danger,
            };
            div().text_color(colour).child(text.clone())
        }))
}

/// A row of the environment picker: a square in the environment's colour (`+` for the
/// entry that creates one), then its name, which is also what the search matches.
fn env_item(label: &str, colour: Option<Hsla>) -> CommandItem {
    let text = label.to_string();
    CommandItem::new()
        .label(label.to_string())
        .child(move |_, cx| {
            let mark = div()
                .size(px(12.))
                .flex_none()
                .rounded_sm()
                .flex()
                .items_center()
                .justify_center();
            let mark = match colour {
                Some(c) => mark.bg(c),
                None => mark
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("+"),
            };
            h_flex().gap_2().child(mark).child(text.clone())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list's space beside the middle at its default width.
    const LIST_SPACE: f32 = DEFAULT_LIST_WIDTH + LIST_DIVIDER_WIDTH;

    #[test]
    fn the_list_width_follows_the_pointer_within_limits() {
        // The list starts at the window's left edge: its width is the pointer's x.
        assert_eq!(list_width_at(300.0, 1200.0, 0.0), 300.0);
        assert_eq!(list_width_at(50.0, 1200.0, 0.0), MIN_LIST_WIDTH);
        assert_eq!(list_width_at(1150.0, 1200.0, 0.0), 1200.0 - 4.0 - 200.0);
        assert_eq!(list_width_at(f32::NAN, 1200.0, 0.0), DEFAULT_LIST_WIDTH);
        // An open tree (260 + its divider) takes its share.
        assert_eq!(
            list_width_at(1000.0, 1200.0, 264.0),
            1200.0 - 264.0 - 4.0 - 200.0
        );
        // A window too narrow for the middle keeps the list at its minimum.
        assert_eq!(list_width_at(250.0, 300.0, 0.0), MIN_LIST_WIDTH);
    }

    #[test]
    fn the_tree_width_follows_the_pointer_within_limits() {
        // The tree is on the right: its width runs from the pointer to the window's right edge.
        assert_eq!(tree_width_at(900.0, 1200.0, LIST_SPACE), 300.0);
        assert_eq!(tree_width_at(1150.0, 1200.0, LIST_SPACE), MIN_TREE_WIDTH);
        assert_eq!(
            tree_width_at(100.0, 1200.0, LIST_SPACE),
            1200.0 - LIST_SPACE - TREE_DIVIDER_WIDTH - MIN_PANES_WIDTH
        );
        // A window too narrow for both keeps the tree at its minimum.
        assert_eq!(tree_width_at(100.0, 250.0, LIST_SPACE), MIN_TREE_WIDTH);
        assert_eq!(
            tree_width_at(f32::NAN, 1200.0, LIST_SPACE),
            DEFAULT_TREE_WIDTH
        );
    }

    #[test]
    fn a_saved_tree_width_is_kept_within_limits() {
        assert_eq!(clamp_tree_width(300.0, 1200.0, LIST_SPACE), 300.0);
        assert_eq!(clamp_tree_width(50.0, 1200.0, LIST_SPACE), MIN_TREE_WIDTH);
        assert_eq!(
            clamp_tree_width(1150.0, 1200.0, LIST_SPACE),
            1200.0 - LIST_SPACE - TREE_DIVIDER_WIDTH - MIN_PANES_WIDTH
        );
        assert_eq!(
            clamp_tree_width(f32::NAN, 1200.0, LIST_SPACE),
            DEFAULT_TREE_WIDTH
        );
    }

    #[test]
    fn the_middle_keeps_its_minimum_beside_the_list_and_the_tree() {
        for window in [800.0, 1200.0, 2560.0] {
            for list in [MIN_LIST_WIDTH, DEFAULT_LIST_WIDTH, 400.0] {
                let list_space = list + LIST_DIVIDER_WIDTH;
                for x in [0.0, 50.0, 300.0, window / 2.0, window - 10.0] {
                    let tree = tree_width_at(x, window, list_space);
                    let middle = window - list_space - TREE_DIVIDER_WIDTH - tree;
                    assert!(
                        middle >= MIN_PANES_WIDTH,
                        "window {window}, list {list}, pointer {x}: middle {middle}"
                    );
                    // And the list dragged beside that tree.
                    let list = list_width_at(x, window, tree + TREE_DIVIDER_WIDTH);
                    let middle = window - list - LIST_DIVIDER_WIDTH - tree - TREE_DIVIDER_WIDTH;
                    assert!(
                        middle >= MIN_PANES_WIDTH,
                        "window {window}, tree {tree}, pointer {x}: middle {middle}"
                    );
                }
            }
        }
    }
}
