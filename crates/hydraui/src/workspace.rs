//! The window's content (spec §7.1, §7.5, §7.6): the tab strip, the active tab's split panes
//! with draggable dividers, the status bar, and the commands bound to keys.
mod launcher;
mod sets;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, DispatchPhase, Entity, FocusHandle, Focusable,
    Hsla, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Subscription,
    Task, Window, canvas, div, px, relative, rgb, transparent_black,
};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, WindowExt, h_flex, v_flex};
use hydraui_sets::LoadedSets;

use crate::config::UiConfig;
use crate::hydra::{Hydra, PaneRequest, STATUS_TTL, StatusCache, accounts_summary};
use crate::keys::{
    ClosePane, FocusDown, FocusLeft, FocusRight, FocusUp, NewTab, NextTab, PreviousTab, SplitDown,
    SplitRight,
};
use crate::pane_view::{PaneEvent, PaneView, TermStyle};
use crate::split::{Axis, Dir, PaneId, Tree};
use crate::tabs::{Closed, TabId, Tabs};

/// A divider being dragged: the split's path in the active tab's tree.
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
    tints: HashMap<String, u32>,
    next_pane: PaneId,
    status: StatusCache,
    refreshing: HashSet<String>,
    drag: Option<Drag>,
    loaded: LoadedSets,
    /// Loaded sets whose file changed since loading.
    changed_sets: HashSet<String>,
    /// Set groups folded away in the tab strip.
    collapsed: HashSet<String>,
    _set_watch: Task<()>,
    /// Each split's on-screen bounds at the last paint, by path, for divider drags.
    split_bounds: Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>,
    focus: FocusHandle,
    title: String,
    subs: HashMap<PaneId, Subscription>,
    _timer: Task<()>,
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
            tints: HashMap::new(),
            next_pane: 0,
            status: StatusCache::default(),
            refreshing: HashSet::new(),
            drag: None,
            loaded: LoadedSets::default(),
            changed_sets: HashSet::new(),
            collapsed: HashSet::new(),
            _set_watch: set_watch,
            split_bounds: Rc::default(),
            focus: cx.focus_handle(),
            title: String::new(),
            subs: HashMap::new(),
            _timer: timer,
        }
    }

    /// Shows what was wrong with the settings, then opens the first tab in the home folder.
    fn startup(&mut self, problems: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        // The sets open at last exit (spec §5.4); a plain tab when there were none.
        if !self.restore_session(window, cx) {
            let home = self.hydra.user_home.clone();
            self.new_tab_in(home, window, cx);
        }
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
            window.push_notification(
                Notification::warning(
                    "No hydra environments yet: create one with `hydra env new <name>`.",
                )
                .title("hydraui"),
                cx,
            );
            return;
        }
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
                    .items(envs.iter().map(|e| CommandItem::new().label(e.clone())))
                    .placeholder("Environment")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(env) = choices.get(ix.row).cloned() {
                            window.close_dialog(cx);
                            workspace
                                .update(cx, |ws, cx| ws.open_tab(env, cwd.clone(), window, cx));
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
        let sub = cx.subscribe_in(&view, window, move |ws, _, ev: &PaneEvent, _, cx| {
            if *ev == PaneEvent::Focused {
                ws.tabs.focus_pane(id);
                ws.refresh_status(cx);
            }
            cx.notify();
        });
        self.subs.insert(id, sub);
        self.panes.insert(id, view);
    }

    fn focused(&self) -> Option<Entity<PaneView>> {
        self.tabs
            .active()
            .and_then(|t| self.panes.get(&t.focused))
            .cloned()
    }

    fn focus_pane(&self, id: PaneId, window: &mut Window, cx: &mut App) {
        match self.panes.get(&id) {
            Some(view) => {
                let handle = view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            None => window.focus(&self.focus, cx),
        }
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
        self.tabs.split_focused(axis, id);
        self.focus_pane(id, window, cx);
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
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.active().map(|t| t.focused) else {
            return;
        };
        if let Closed::Tab(tab) = self.tabs.close_pane(id) {
            self.tab_gone(tab, window, cx);
        }
        self.drop_pane(id);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        for id in self.tabs.close_tab(tab) {
            self.drop_pane(id);
        }
        self.tab_gone(tab, window, cx);
        self.focus_active(window, cx);
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
            let summary = cx
                .background_spawn(async move { accounts_summary(&hydra.whoami(&e)) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                ws.refreshing.remove(&env);
                ws.status.store(env, summary, Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    fn tint(&self, env: &str) -> Hsla {
        rgb(self.tints.get(env).copied().unwrap_or(0x808080)).into()
    }

    fn drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
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
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_move.update(cx, |ws, cx| ws.drag_to(ev.position, cx));
                    }
                });
                let on_up = ws.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_up.update(cx, |ws, cx| {
                            ws.drag = None;
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
            .border_color(border);
        for (set, ids) in self.tabs.groups() {
            strip = match &set {
                Some(name) => strip.child(self.render_set_header(name, muted, cx)),
                None => strip.child(div().px_2().text_sm().text_color(muted).child("Loose")),
            };
            if set.as_ref().is_some_and(|s| self.collapsed.contains(s)) {
                continue;
            }
            for id in ids {
                let Some(tab) = self.tabs.get(id) else {
                    continue;
                };
                let (label, env) = self
                    .panes
                    .get(&tab.focused)
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
            let v = view.read(cx);
            let env = v.env().to_string();
            let accounts = self.status.get(&env).unwrap_or("…").to_string();
            bar = bar
                .child(div().text_color(self.tint(&env)).child(env))
                .child(div().flex_1().truncate().child(accounts))
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
            .child(self.render_tab_strip(bar, border, muted, cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status(border, muted, cx))
            .when(self.drag.is_some(), |d| d.child(self.drag_listener(cx)))
    }
}
