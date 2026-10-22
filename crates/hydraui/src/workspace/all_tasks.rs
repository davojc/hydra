//! Tasks: all projects (task board spec §10): a pinned entry at the top of the terminal list
//! shows, in place of a terminal, one swimlane per project a terminal of the selected
//! environment is in. Each lane is that project's working board, without its Done column.
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{Context, Entity, Focusable, Hsla, SharedString, Subscription, Window, div, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};

use super::Workspace;
use super::tasks_tab::BoardHost;
use crate::task_board_view::TaskBoardView;
use crate::tasks::Column;

/// The list entry's label.
pub(super) const ALL_TASKS: &str = "Tasks: all projects";

/// A lane's least height: a column header and a few cards. Lanes share the height left
/// beyond that, and the board scrolls when they don't fit.
const LANE_MIN_HEIGHT: f32 = 260.;

/// One project's lane: its board, without Done.
pub(super) struct Lane {
    pub project: PathBuf,
    pub view: Entity<TaskBoardView>,
    _events: Subscription,
    _focus: Subscription,
}

/// `projects` without repeats, in the order they first come.
fn first_seen(projects: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in projects {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// A project's name for its lane: its folder's name.
fn project_name(project: &std::path::Path) -> String {
    project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.display().to_string())
}

impl Workspace {
    /// Whether the selected environment shows Tasks: all projects rather than a terminal.
    pub(super) fn all_tasks_shown(&self) -> bool {
        self.envs
            .active()
            .is_some_and(|t| self.all_tasks.contains(&t.env))
    }

    /// The projects `env`'s terminals are in, in the terminal list's order.
    fn env_projects(&self, env: &str, cx: &gpui::App) -> Vec<PathBuf> {
        let Some(tab) = self.envs.get(env) else {
            return Vec::new();
        };
        first_seen(tab.terminals.iter().filter_map(|id| {
            let cwd = self.panes.get(id)?.read(cx).cwd().clone();
            Some(crate::tasks_project::project_root(&cwd))
        }))
    }

    /// The pinned entry, or the launcher's Tasks: all projects: the selected environment shows
    /// its projects' boards.
    pub(super) fn show_all_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.envs.active().map(|t| t.env.clone()) else {
            return;
        };
        self.all_tasks.insert(env.clone());
        self.settings = None;
        self.sync_lanes(&env, window, cx);
        self.sync_boards(window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The selected environment goes back to showing its terminal (one was picked).
    pub(super) fn leave_all_tasks(&mut self) {
        if let Some(env) = self.envs.active().map(|t| t.env.clone()) {
            self.all_tasks.remove(&env);
        }
    }

    /// Keeps the shown lanes in step with the terminals: a terminal opened, closed or `cd`'d
    /// into another project. Checked every second.
    pub(super) fn sync_shown_lanes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.envs.active().map(|t| t.env.clone()) else {
            return;
        };
        if !self.all_tasks.contains(&env) {
            return;
        }
        let have: Vec<PathBuf> = self
            .lanes
            .get(&env)
            .map(|l| l.iter().map(|l| l.project.clone()).collect())
            .unwrap_or_default();
        if have != self.env_projects(&env, cx) {
            self.sync_lanes(&env, window, cx);
            cx.notify();
        }
    }

    /// `env`'s lanes become one per project of its terminals: kept ones keep their view, new
    /// ones are made, and gone ones save their edit and let their board go. A lane whose edit
    /// can't be saved stays, at the end, so the idea isn't lost.
    fn sync_lanes(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.env_projects(env, cx);
        let mut old = self.lanes.remove(env).unwrap_or_default();
        let mut lanes = Vec::new();
        for project in want {
            match old.iter().position(|l| l.project == project) {
                Some(i) => lanes.push(old.remove(i)),
                None => lanes.push(self.new_lane(env, project, window, cx)),
            }
        }
        let mut gone = Vec::new();
        for lane in old {
            lane.view.update(cx, |v, cx| v.finish_edit(window, cx));
            if lane.view.read(cx).is_editing() {
                lanes.push(lane);
            } else {
                gone.push(lane.project);
            }
        }
        self.lanes.insert(env.to_string(), lanes);
        for project in gone {
            self.drop_unused_board(&project, cx);
        }
    }

    fn new_lane(
        &mut self,
        env: &str,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Lane {
        let (view, events, focus) = self.make_board_view(
            BoardHost::Lane(env.to_string()),
            project.clone(),
            window,
            cx,
        );
        view.update(cx, |v, _| v.hide_done());
        Lane {
            project,
            view,
            _events: events,
            _focus: focus,
        }
    }

    /// Environment `env` closed: its lanes save their edits and let their boards go.
    pub(super) fn drop_lanes(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.all_tasks.remove(env);
        for lane in self.lanes.remove(env).unwrap_or_default() {
            lane.view.update(cx, |v, cx| v.finish_edit(window, cx));
            self.drop_unused_board(&lane.project, cx);
        }
    }

    /// The selected environment's lanes on screen, for focus and saving edits.
    pub(super) fn shown_lanes(&self) -> &[Lane] {
        match self.envs.active() {
            Some(t) if self.all_tasks.contains(&t.env) => {
                self.lanes.get(&t.env).map(Vec::as_slice).unwrap_or(&[])
            }
            _ => &[],
        }
    }

    /// The first lane's board, which takes the focus while the lanes show; `None` without one.
    pub(super) fn all_tasks_focus(&self, cx: &gpui::App) -> Option<gpui::FocusHandle> {
        self.shown_lanes()
            .first()
            .map(|l| l.view.read(cx).focus_handle(cx))
    }

    /// The lanes: each project's name, folder and how many Done cards it leaves out, over its
    /// board; scrolling when they don't fit.
    pub(super) fn render_all_tasks(
        &self,
        tint: Hsla,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let lanes = self.shown_lanes();
        if lanes.is_empty() {
            return v_flex()
                .size_full()
                .border_1()
                .border_color(tint)
                .items_center()
                .justify_center()
                .text_color(muted)
                .child("No terminals here, so no projects to show.")
                .into_any_element();
        }
        let mut list = v_flex()
            .id("all-tasks")
            .size_full()
            .border_1()
            .border_color(tint)
            .overflow_y_scroll();
        for (i, lane) in lanes.iter().enumerate() {
            let done = lane
                .view
                .read(cx)
                .board()
                .read(cx)
                .board()
                .cards(Column::Done)
                .len();
            let note = match done {
                0 => "Done isn't shown here".to_string(),
                1 => "Done isn't shown here (1 card)".to_string(),
                n => format!("Done isn't shown here ({n} cards)"),
            };
            list = list.child(
                v_flex()
                    .id(("all-tasks-lane", i))
                    .flex_1()
                    .flex_shrink_0()
                    .min_h(px(LANE_MIN_HEIGHT))
                    .when(i > 0, |d| d.border_t_2().border_color(border))
                    .child(
                        h_flex()
                            .flex_none()
                            .px_2()
                            .py_1()
                            .gap_2()
                            .bg(tint.opacity(0.12))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(project_name(&lane.project))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(lane.project.display().to_string()),
                            )
                            .child(div().flex_none().text_xs().text_color(muted).child(note)),
                    )
                    .child(div().flex_1().min_h_0().child(lane.view.clone())),
            );
        }
        list.into_any_element()
    }

    /// The pinned entry at the top of the terminal list, highlighted while it shows.
    pub(super) fn render_all_tasks_entry(
        &self,
        tint: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shown = self.all_tasks_shown();
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id("all-tasks-entry")
            .px_2()
            .py_1()
            .gap_2()
            .rounded_md()
            .text_sm()
            .when(shown, |d| d.bg(tint.opacity(0.25)))
            .hover(|d| d.bg(tint.opacity(0.15)))
            .child(div().flex_none().text_color(muted).child("☰"))
            .child(div().flex_1().min_w_0().truncate().child(ALL_TASKS))
            .on_click(cx.listener(|ws, _, window, cx| ws.show_all_tasks(window, cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_project_once_in_the_order_its_first_terminal_came() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            first_seen([p("/b"), p("/a"), p("/b"), p("/c"), p("/a")]),
            [p("/b"), p("/a"), p("/c")]
        );
    }
}
