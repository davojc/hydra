//! The window's content (spec §7.1, §7.5, §7.6): the tab strip, the active tab's split panes
//! with draggable dividers, the status bar, and the commands bound to keys.
mod accounts;
mod finder;
mod launcher;
mod sets;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, DispatchPhase, Entity, FocusHandle, Focusable,
    Hsla, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Subscription,
    Task, Window, canvas, div, px, relative, rgb, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex};
use hydraui_sets::LoadedSets;

use hydraui_term::PromptState;

use crate::config::UiConfig;
use crate::file_pane::{FilePane, FilePaneEvent};
use crate::file_tree::{cd_command, editor_command, quote_path};
use crate::file_tree_view::{FileTreeView, TreeEvent};
use crate::git_status;
use crate::hydra::{Borrowing, Hydra, PaneRequest, STATUS_TTL, StatusCache, account_marks};
use crate::keys::{
    ClosePane, FileTree, FocusDown, FocusLeft, FocusRight, FocusUp, NewTab, NextTab, PreviousTab,
    SplitDown, SplitRight,
};
use crate::open_files::{last_terminal_closes_tab, terminal_for};
use crate::pane_view::{PaneEvent, PaneView, TermStyle};
use crate::split::{Axis, Dir, PaneId, Tree};
use crate::tabs::{Closed, TabId, Tabs};

/// A divider being dragged: the split's path in the active tab's tree.
/// The file tree's width: it starts at the default, can't be dragged narrower than the
/// minimum, and always leaves the panes at least `MIN_PANES_WIDTH`.
const DEFAULT_TREE_WIDTH: f32 = 260.0;
const MIN_TREE_WIDTH: f32 = 140.0;
const MIN_PANES_WIDTH: f32 = 200.0;

/// The tree's width with its divider dragged to `x` pixels from the tree's left edge, in a
/// window `window_width` wide.
fn tree_width_at(x: f32, window_width: f32) -> f32 {
    if !x.is_finite() {
        return DEFAULT_TREE_WIDTH;
    }
    let max = (window_width - MIN_PANES_WIDTH).max(MIN_TREE_WIDTH);
    x.clamp(MIN_TREE_WIDTH, max)
}

struct Drag {
    path: Vec<usize>,
    axis: Axis,
}

pub struct Workspace {
    hydra: Arc<Hydra>,
    ui: UiConfig,
    style: Arc<TermStyle>,
    tabs: Tabs,
    panes: HashMap<PaneId, Entity<PaneView>>,
    /// File panes (spec §7.8), at most one per tab; leaves of the same split trees.
    files: HashMap<PaneId, Entity<FilePane>>,
    file_subs: HashMap<PaneId, Subscription>,
    /// Each tab's last focused terminal, which stands for the tab while its file pane has
    /// the focus.
    last_terminal: HashMap<TabId, PaneId>,
    tints: HashMap<String, u32>,
    next_pane: PaneId,
    status: StatusCache,
    /// Each environment's borrowed logins and possible lenders, read with its accounts.
    borrowing: HashMap<String, Borrowing>,
    refreshing: HashSet<String>,
    /// The focused pane's repository summary, for the folder it was read in; `None` text when
    /// that folder isn't in a repository.
    git: Option<(PathBuf, Option<String>)>,
    /// A `git status` is running; `git_again` asks for another once it finishes.
    git_busy: bool,
    git_again: bool,
    _git_watch: Task<()>,
    drag: Option<Drag>,
    loaded: LoadedSets,
    /// Loaded sets whose file changed since loading.
    changed_sets: HashSet<String>,
    /// Set groups folded away in the tab strip.
    collapsed: HashSet<String>,
    _set_watch: Task<()>,
    tree: Option<Entity<FileTreeView>>,
    tree_sub: Option<Subscription>,
    tree_width: f32,
    /// The divider between the tree and the panes is being dragged.
    tree_drag: bool,
    /// The tree panel's bounds at the last paint, for the drag.
    tree_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Each split's on-screen bounds at the last paint, by path, for divider drags.
    split_bounds: Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>,
    focus: FocusHandle,
    title: String,
    subs: HashMap<PaneId, Subscription>,
    _timer: Task<()>,
    /// Whether the status bar's accounts popover is open.
    accounts_open: bool,
}

impl Workspace {
    pub fn new(
        hydra: Arc<Hydra>,
        ui: UiConfig,
        style: TermStyle,
        problems: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_TTL).await;
                if this.update(cx, |ws, cx| ws.refresh_status(cx)).is_err() {
                    break;
                }
            }
        });
        // A loaded set's file edited or deleted: offer a reload (spec §5.4).
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
        let set_watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                if this.update(cx, |ws, cx| ws.check_sets(cx)).is_err() {
                    break;
                }
            }
        });
        // After the window's Root exists: toasts and dialogs live in it.
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |ws, window, cx| ws.startup(problems, window, cx));
        })
        .detach();
        Self {
            hydra,
            ui,
            style: Arc::new(style),
            tabs: Tabs::default(),
            panes: HashMap::new(),
            files: HashMap::new(),
            file_subs: HashMap::new(),
            last_terminal: HashMap::new(),
            tints: HashMap::new(),
            next_pane: 0,
            status: StatusCache::default(),
            borrowing: HashMap::new(),
            refreshing: HashSet::new(),
            git: None,
            git_busy: false,
            git_again: false,
            _git_watch: git_watch,
            drag: None,
            loaded: LoadedSets::default(),
            changed_sets: HashSet::new(),
            collapsed: HashSet::new(),
            _set_watch: set_watch,
            tree: None,
            tree_sub: None,
            tree_width: DEFAULT_TREE_WIDTH,
            tree_drag: false,
            tree_bounds: Rc::default(),
            split_bounds: Rc::default(),
            focus: cx.focus_handle(),
            title: String::new(),
            subs: HashMap::new(),
            _timer: timer,
            accounts_open: false,
        }
    }

    /// Shows what was wrong with the settings, then opens the first tab in the home folder.
    fn startup(&mut self, problems: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        // What was open at last exit (spec §5.4); a plain tab when there was nothing.
        if !self.restore_session(window, cx) {
            let home = self.hydra.user_home.clone();
            self.new_tab_in(home, window, cx);
        }
        // Each pane's folder as it is when the window closes.
        let ws = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            ws.update(cx, |ws, cx| {
                ws.save_session(window, cx);
                // Unsaved edits: ask first, and close from the dialog (spec §7.8).
                let all: Vec<PaneId> = ws.files.keys().copied().collect();
                if ws.unsaved_in(&all, cx).is_empty() {
                    return true;
                }
                ws.confirm_unsaved(all, window, cx, |_, w, _| w.remove_window());
                false
            })
            .unwrap_or(true)
        });
    }

    /// A Loose tab in `cwd`: the folder's environment, else `default_env`, else ask.
    fn new_tab_in(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match self.hydra.adhoc_env(&cwd, self.ui.default_env.as_deref()) {
            Some(env) => self.open_tab(env, cwd, window, cx),
            None => self.pick_env(cwd, window, cx),
        }
    }

    fn pick_env(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let envs = self.hydra.env_names();
        if envs.is_empty() {
            self.new_env_dialog(window, cx);
            return;
        }
        // Each environment with its colour, as on its tabs.
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
            let (choices, workspace, cwd) = (envs.clone(), workspace.clone(), cwd.clone());
            dialog.title("Open a tab in…").child(
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
                            Some(env) => workspace
                                .update(cx, |ws, cx| ws.open_tab(env, cwd.clone(), window, cx)),
                            None => workspace.update(cx, |ws, cx| ws.new_env_dialog(window, cx)),
                        }
                    }),
            )
        });
    }

    fn open_tab(&mut self, env: String, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let shell = self.hydra.shell(self.ui.default_shell.as_deref());
        self.open_request(PaneRequest::new(env, cwd, shell), window, cx);
    }

    /// The focused pane's folder, else the user's home.
    fn focused_cwd(&self, cx: &App) -> PathBuf {
        self.focused()
            .map(|v| v.read(cx).cwd().clone())
            .unwrap_or_else(|| self.hydra.user_home.clone())
    }

    /// A Loose tab running `request`.
    fn open_request(&mut self, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.new_pane(request, window, cx);
        self.tabs.add(None, id);
        self.focus_soon(id, window, cx);
        self.save_session(window, cx);
        cx.notify();
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
                if *ev == PaneEvent::Focused {
                    ws.tabs.focus_pane(id);
                    if let Some(tab) = ws.tabs.tab_of(id) {
                        ws.last_terminal.insert(tab, id);
                    }
                    ws.refresh_status(cx);
                }
                // The focused pane's repository: on focus, on `cd`, and when a command finishes.
                let focused = ws.tabs.active().is_some_and(|t| t.focused == id);
                let back_at_prompt = *ev == PaneEvent::PromptChanged
                    && view.read(cx).prompt() == Some(PromptState::AtPrompt);
                if focused
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

    /// The terminal standing for the active tab: the focused pane if it's a terminal, else
    /// the tab's last focused terminal (spec §7.8).
    fn focused(&self) -> Option<Entity<PaneView>> {
        self.tab_terminal(self.tabs.active()?)
    }

    /// The terminal standing for `tab` (spec §7.8).
    fn tab_terminal(&self, tab: &crate::tabs::Tab) -> Option<Entity<PaneView>> {
        let id = terminal_for(
            tab.focused,
            self.last_terminal.get(&tab.id).copied(),
            &tab.tree.panes(),
            &|p| self.panes.contains_key(&p),
        )?;
        self.panes.get(&id).cloned()
    }

    fn focus_pane(&self, id: PaneId, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.files.get(&id) {
            view.update(cx, |f, cx| f.focus_content(window, cx));
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
                    ws.tabs.focus_pane(id);
                    cx.notify();
                }
                FilePaneEvent::Changed => ws.save_session(window, cx),
                FilePaneEvent::Empty => ws.remove_files_pane(id, window, cx),
                FilePaneEvent::OpenInEditor(path) => ws.open_file(path, window, cx),
            },
        );
        self.file_subs.insert(id, sub);
        self.files.insert(id, view);
    }

    /// Shows `path` in the active tab's file pane, splitting the tab's terminal to the right
    /// first when the tab has none (spec §7.8).
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
        let Some(tab) = self.tabs.active() else {
            return;
        };
        let existing = tab
            .tree
            .panes()
            .into_iter()
            .find(|p| self.files.contains_key(p));
        let id = match existing {
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
                self.tabs.focus_pane(id);
                id
            }
            None => {
                let Some(term) = self.focused().map(|v| v.read(cx).id) else {
                    return;
                };
                self.next_pane += 1;
                let id = self.next_pane;
                self.create_files_pane(id, std::slice::from_ref(&path), 0, window, cx);
                if line > 0
                    && let Some(view) = self.files.get(&id).cloned()
                {
                    view.update(cx, |f, cx| f.open_at(path, line, column, window, cx));
                }
                self.tabs.split_pane(term, Axis::Row, id);
                id
            }
        };
        self.focus_pane(id, window, cx);
        self.save_session(window, cx);
        cx.notify();
    }

    fn remove_files_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Closed::Tab(tab) = self.tabs.close_pane(id) {
            self.tab_gone(tab, window, cx);
        }
        self.files.remove(&id);
        self.file_subs.remove(&id);
        self.focus_active(window, cx);
        self.save_session(window, cx);
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

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        match self.tabs.active() {
            Some(tab) => self.focus_pane(tab.focused, window, cx),
            None => window.focus(&self.focus, cx),
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.focused_cwd(cx);
        self.new_tab_in(cwd, window, cx);
    }

    /// The new pane gets the focused pane's environment and current folder (spec §7.6).
    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.focused() else {
            return;
        };
        let request = {
            let v = from.read(cx);
            PaneRequest::new(v.env(), v.cwd().clone(), v.request.shell)
        };
        let id = self.new_pane(request, window, cx);
        let target = from.read(cx).id;
        self.tabs.split_pane(target, axis, id);
        self.focus_pane(id, window, cx);
        self.save_session(window, cx);
        cx.notify();
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Row, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Column, window, cx);
    }

    fn drop_pane(&mut self, id: PaneId) {
        // Dropping the view drops its Pane: the job ends the shell's whole process tree.
        self.panes.remove(&id);
        self.subs.remove(&id);
        self.files.remove(&id);
        self.file_subs.remove(&id);
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.active() else {
            return;
        };
        let (id, tab_id, panes) = (tab.focused, tab.id, tab.tree.panes());
        if self.files.contains_key(&id) {
            self.confirm_unsaved(vec![id], window, cx, move |ws, w, cx| {
                ws.remove_files_pane(id, w, cx)
            });
            return;
        }
        // A tab's last terminal takes the tab, file pane and all (spec §7.8).
        if last_terminal_closes_tab(&panes, id, &|p| self.panes.contains_key(&p)) {
            self.close_tab(tab_id, window, cx);
            return;
        }
        if let Closed::Tab(tab) = self.tabs.close_pane(id) {
            self.tab_gone(tab, window, cx);
        }
        self.drop_pane(id);
        self.focus_active(window, cx);
        self.save_session(window, cx);
        cx.notify();
    }

    /// Closes a tab, asking first when its file pane has unsaved edits.
    fn close_tab(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self
            .tabs
            .get(tab)
            .map(|t| t.tree.panes())
            .unwrap_or_default();
        self.confirm_unsaved(panes, window, cx, move |ws, w, cx| {
            ws.close_tab_now(tab, w, cx)
        });
    }

    fn close_tab_now(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        for id in self.tabs.close_tab(tab) {
            self.drop_pane(id);
        }
        self.tab_gone(tab, window, cx);
        self.focus_active(window, cx);
        self.save_session(window, cx);
        cx.notify();
    }

    fn move_focus(&mut self, dir: Dir, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.focus_dir(dir) {
            self.focus_pane(id, window, cx);
            cx.notify();
        }
    }

    fn focus_left(&mut self, _: &FocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Left, window, cx);
    }

    fn focus_right(&mut self, _: &FocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Right, window, cx);
    }

    fn focus_up(&mut self, _: &FocusUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Up, window, cx);
    }

    fn focus_down(&mut self, _: &FocusDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Down, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.next();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.previous();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Checks the focused environment's accounts off the UI thread, at most once a minute.
    /// Reads the focused pane's repository off the UI thread; one `git status` at a time.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_busy {
            self.git_again = true;
            return;
        }
        let Some(view) = self.focused() else {
            return;
        };
        let cwd = view.read(cx).cwd().clone();
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
                let line = status.as_ref().map(git_status::summary);
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
        let Some(env) = self.focused().map(|v| v.read(cx).env().to_string()) else {
            return;
        };
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
            self.sync_tree(cx);
        }
        self.save_session(window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Roots the tree at the focused pane's folder; `cd` only while it is at its prompt.
    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let (Some(tree), Some(view)) = (self.tree.clone(), self.focused()) else {
            return;
        };
        let (cwd, at_prompt) = {
            let v = view.read(cx);
            (v.cwd().clone(), v.prompt() == Some(PromptState::AtPrompt))
        };
        tree.update(cx, |t, cx| {
            t.set_root(cwd, cx);
            t.set_can_cd(at_prompt, cx);
        });
    }

    fn on_tree_event(&mut self, ev: &TreeEvent, window: &mut Window, cx: &mut Context<Self>) {
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
        }
        self.focus_active(window, cx);
    }

    /// Diffs a file against git's last commit off the UI thread, then shows it beside the file
    /// in the tab's file pane (spec §7.3).
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
                let pane = ws.tabs.active().and_then(|tab| {
                    tab.tree
                        .panes()
                        .into_iter()
                        .find_map(|p| ws.files.get(&p).cloned())
                });
                if let Some(pane) = pane {
                    pane.update(cx, |f, cx| f.open_diff(path, patch, window, cx));
                }
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

    fn drag_to(&mut self, pos: Point<Pixels>, window_width: f32, cx: &mut Context<Self>) {
        if self.tree_drag {
            if let Some(b) = self.tree_bounds.get() {
                self.tree_width = tree_width_at(f32::from(pos.x - b.origin.x), window_width);
                cx.notify();
            }
            return;
        }
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(b) = self.split_bounds.borrow().get(&drag.path).copied() else {
            return;
        };
        let ratio = match drag.axis {
            Axis::Row => (pos.x - b.origin.x) / b.size.width,
            Axis::Column => (pos.y - b.origin.y) / b.size.height,
        };
        let path = drag.path.clone();
        if let Some(tab) = self.tabs.active_mut() {
            tab.tree.set_ratio(&path, ratio);
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
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_up.update(cx, |ws, cx| {
                            ws.drag = None;
                            ws.tree_drag = false;
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

    fn render_tree(
        &self,
        tree: &Tree,
        path: Vec<usize>,
        focused: PaneId,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match tree {
            Tree::Leaf(id) => {
                if let Some(view) = self.files.get(id) {
                    let edge = if *id == focused {
                        cx.theme().accent
                    } else {
                        transparent_black()
                    };
                    return div()
                        .size_full()
                        .border_1()
                        .border_color(edge)
                        .child(view.clone())
                        .into_any_element();
                }
                let Some(view) = self.panes.get(id) else {
                    return div().into_any_element();
                };
                let edge = if *id == focused {
                    self.tint(view.read(cx).env())
                } else {
                    transparent_black()
                };
                div()
                    .size_full()
                    .border_1()
                    .border_color(edge)
                    .child(view.clone())
                    .into_any_element()
            }
            Tree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let axis = *axis;
                let row = axis == Axis::Row;
                let child_path = |i: usize| {
                    let mut p = path.clone();
                    p.push(i);
                    p
                };
                let store = self.split_bounds.clone();
                let key = path.clone();
                let drag_path = path.clone();
                let divider = div()
                    .id(SharedString::from(format!("divider-{path:?}")))
                    .flex_none()
                    .bg(border)
                    .map(|d| {
                        if row {
                            d.w(px(4.)).h_full().cursor(CursorStyle::ResizeLeftRight)
                        } else {
                            d.h(px(4.)).w_full().cursor(CursorStyle::ResizeUpDown)
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| {
                            ws.drag = Some(Drag {
                                path: drag_path.clone(),
                                axis,
                            });
                            cx.notify();
                        }),
                    );
                let one = div()
                    .flex_none()
                    .overflow_hidden()
                    .map(|d| {
                        if row {
                            d.h_full().w(relative(*ratio))
                        } else {
                            d.w_full().h(relative(*ratio))
                        }
                    })
                    .child(self.render_tree(first, child_path(0), focused, border, cx));
                let two = div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_tree(second, child_path(1), focused, border, cx));
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .map(|d| if row { d.flex_row() } else { d.flex_col() })
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                store.borrow_mut().insert(key, bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .child(one)
                    .child(divider)
                    .child(two)
                    .into_any_element()
            }
        }
    }

    fn render_tab_strip(
        &self,
        bg: Hsla,
        border: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.tabs.active_id();
        let mut strip = h_flex()
            .h(px(34.))
            .px_1()
            .gap_1()
            .bg(bg)
            .border_b_1()
            .border_color(border)
            // The file tree, as Ctrl+B; highlighted while it is open.
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
            );
        // "Loose" only tells tabs apart from a set's, so it shows only beside one.
        let has_sets = self.tabs.has_sets();
        for (set, ids) in self.tabs.groups() {
            strip = match &set {
                Some(name) => strip.child(self.render_set_header(name, muted, cx)),
                None if has_sets => {
                    strip.child(div().px_2().text_sm().text_color(muted).child("Loose"))
                }
                None => strip,
            };
            if set.as_ref().is_some_and(|s| self.collapsed.contains(s)) {
                continue;
            }
            for id in ids {
                let Some(tab) = self.tabs.get(id) else {
                    continue;
                };
                let (label, env) = self
                    .tab_terminal(tab)
                    .map(|v| {
                        let v = v.read(cx);
                        (v.label(), v.env().to_string())
                    })
                    .unwrap_or_default();
                let tint = self.tint(&env);
                let label = tab.title.clone().unwrap_or(label);
                strip = strip.child(
                    h_flex()
                        .id(("tab", id))
                        .px_2()
                        .py_1()
                        .gap_2()
                        .rounded_md()
                        .when(Some(id) == active, |d| d.bg(tint.opacity(0.25)))
                        .hover(|d| d.bg(tint.opacity(0.15)))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.tabs.activate(id);
                            ws.focus_active(window, cx);
                            cx.notify();
                        }))
                        .child(div().size(px(8.)).rounded_full().bg(tint))
                        .child(div().text_sm().child(label))
                        .context_menu({
                            let ws = cx.entity();
                            move |menu, _, _| {
                                let (save_ws, close_ws) = (ws.clone(), ws.clone());
                                menu.item(PopupMenuItem::new("Save tab as group…").on_click(
                                    move |_, window, cx| {
                                        save_ws.update(cx, |ws, cx| {
                                            ws.save_tab_as_group(id, window, cx)
                                        });
                                    },
                                ))
                                .item(
                                    PopupMenuItem::new("Close tab").on_click(
                                        move |_, window, cx| {
                                            close_ws
                                                .update(cx, |ws, cx| ws.close_tab(id, window, cx));
                                        },
                                    ),
                                )
                            }
                        })
                        .child(
                            div()
                                .id(("close", id))
                                .px_1()
                                .text_color(muted)
                                .hover(|d| d.text_color(tint))
                                .child("×")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    cx.stop_propagation();
                                    ws.close_tab(id, window, cx);
                                })),
                        ),
                );
            }
        }
        strip.child(
            div()
                .id("new-tab")
                .px_2()
                .text_color(muted)
                .child("+")
                .on_click(cx.listener(|ws, _, window, cx| ws.new_tab(&NewTab, window, cx))),
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
        if let Some(view) = self.focused() {
            let env = view.read(cx).env().to_string();
            let env_name = self.render_env_accounts(env.clone(), self.tint(&env), cx);
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
                Some((dir, Some(line))) if dir == v.cwd() => Some(line.clone()),
                _ => None,
            };
            bar = bar
                .child(env_name)
                .child(div().flex_1().min_w_0().overflow_hidden().child(accounts))
                .when_some(git, |b, line| b.child(div().flex_none().child(line)))
                .child(div().child(v.cwd().display().to_string()));
        }
        bar
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.split_bounds.borrow_mut().clear();
        let title = match self.focused() {
            Some(v) => format!("hydraui - {}", v.read(cx).label()),
            None => "hydraui".to_string(),
        };
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
        let body = match self.tabs.active() {
            Some(tab) => self.render_tree(&tab.tree, Vec::new(), tab.focused, border, cx),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child("No tabs open. Ctrl+T opens one.")
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .on_action(cx.listener(Self::focus_up))
            .on_action(cx.listener(Self::focus_down))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::open_launcher))
            .on_action(cx.listener(Self::toggle_tree))
            .on_action(cx.listener(Self::open_file_action))
            .on_action(cx.listener(Self::search_action))
            .child(self.render_tab_strip(bar, border, muted, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .when_some(self.tree.clone(), |d, tree| {
                        let store = self.tree_bounds.clone();
                        d.child(
                            div()
                                .w(px(self.tree_width))
                                .h_full()
                                .flex_none()
                                .relative()
                                .child(
                                    canvas(
                                        move |bounds, _, _| store.set(Some(bounds)),
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                                )
                                .child(tree),
                        )
                        .child(
                            div()
                                .id("tree-divider")
                                .w(px(4.))
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
                    })
                    .child(div().flex_1().min_w_0().h_full().child(body)),
            )
            .child(self.render_status(border, muted, cx))
            .when(self.drag.is_some() || self.tree_drag, |d| {
                d.child(self.drag_listener(cx))
            })
    }
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

    #[test]
    fn the_tree_width_follows_the_pointer_within_limits() {
        assert_eq!(tree_width_at(300.0, 1200.0), 300.0);
        assert_eq!(tree_width_at(50.0, 1200.0), MIN_TREE_WIDTH);
        assert_eq!(tree_width_at(1150.0, 1200.0), 1200.0 - MIN_PANES_WIDTH);
        // A window too narrow for both keeps the tree at its minimum.
        assert_eq!(tree_width_at(300.0, 250.0), MIN_TREE_WIDTH);
        assert_eq!(tree_width_at(f32::NAN, 1200.0), DEFAULT_TREE_WIDTH);
    }
}
