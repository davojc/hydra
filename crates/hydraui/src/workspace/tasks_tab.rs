//! A terminal's Tasks tab (spec: docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
//! §3-§4): its project's board beside the terminal, a small strip to switch between the two,
//! one board per project shared by every tab on it, and the commit/private question.
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{Context, Entity, Focusable, Hsla, SharedString, Subscription, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::task_board::{TaskBoard, TaskBoardEvent};
use crate::task_board_view::{TaskBoardView, TaskBoardViewEvent};

/// Where a board view sits: a terminal's Tasks tab, or a lane of an environment's
/// Tasks: all projects.
#[derive(Debug, Clone)]
pub(super) enum BoardHost {
    Tab(PaneId),
    Lane(String),
}

/// The project a terminal in `cwd` keeps its board in (§2): its git work tree, else `cwd`.
fn tasks_project(cwd: &Path) -> PathBuf {
    crate::tasks_project::project_root(cwd)
}

impl Workspace {
    /// Gives terminal `id` a Tasks tab on its project's board and selects it (§4); when it has
    /// one already, just selects it.
    pub(super) fn open_tasks(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tasks_tabs.contains_key(&id) {
            self.show_tasks(id, true, window, cx);
            return;
        }
        let Some(cwd) = self.panes.get(&id).map(|v| v.read(cx).cwd().clone()) else {
            return;
        };
        self.new_board_view(id, tasks_project(&cwd), window, cx);
        self.tasks_tabs.insert(id, true);
        self.leave_files(id);
        self.sync_boards(window, cx);
        self.focus_active(window, cx);
        self.autosave(window, cx);
        cx.notify();
    }

    /// Selects terminal `id`'s Tasks tab (`true`) or the terminal (`false`). Selecting the board
    /// follows the terminal to the project it is in now (§4).
    pub(super) fn show_tasks(
        &mut self,
        id: PaneId,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.tasks_tabs.contains_key(&id) {
            return;
        }
        self.tasks_tabs.insert(id, shown);
        if let Some(pipeline) = self.pipeline_tabs.get_mut(&id) {
            pipeline.shown = false;
        }
        if shown {
            self.leave_files(id);
            self.follow_project(id, window, cx);
        }
        self.sync_boards(window, cx);
        self.focus_active(window, cx);
        self.autosave(window, cx);
        cx.notify();
    }

    /// ✕ on the Tasks tab, or Ctrl+W in the board: the tab closes (the file stays) and the
    /// terminal shows again with the focus. An edit that can't be saved keeps the tab open, so
    /// the idea isn't lost (§1); its toast has said why.
    pub(super) fn close_tasks(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tasks_tabs.contains_key(&id) {
            return;
        }
        if let Some(view) = self.board_views.get(&id).cloned() {
            view.update(cx, |v, cx| v.finish_edit(window, cx));
            if view.read(cx).is_editing() {
                return;
            }
        }
        self.drop_tasks(id, window, cx);
        self.focus_active(window, cx);
        self.autosave(window, cx);
        cx.notify();
    }

    /// Removes terminal `id`'s Tasks tab, if any, saving an edit in progress first; its board
    /// goes when no other tab uses it. For closing the tab and closing the terminal.
    /// A closing terminal takes its tab with it: an edit whose write fails is lost then.
    pub(super) fn drop_tasks(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.tasks_tabs.remove(&id);
        self.board_focus.remove(&id);
        self.board_events.remove(&id);
        let Some(view) = self.board_views.remove(&id) else {
            return;
        };
        view.update(cx, |v, cx| v.finish_edit(window, cx));
        let project = view.read(cx).board().read(cx).project().to_path_buf();
        self.drop_unused_board(&project, cx);
    }

    /// The terminal whose Tasks tab is on screen: the selected environment's shown terminal,
    /// when its Tasks tab is selected.
    pub(super) fn visible_tasks(&self) -> Option<PaneId> {
        if self.all_tasks_shown() {
            return None;
        }
        let id = self.envs.active()?.shown?;
        (self.tasks_tabs.get(&id) == Some(&true) && !self.pipeline_shown(id)).then_some(id)
    }

    /// The board view on screen when terminal `id`'s Tasks tab is selected.
    pub(super) fn shown_board(&self, id: PaneId) -> Option<&Entity<TaskBoardView>> {
        if self.tasks_tabs.get(&id) != Some(&true) || self.pipeline_shown(id) {
            return None;
        }
        self.board_views.get(&id)
    }

    /// After what's on screen changed: boards no longer on screen save their edit in progress
    /// (a click elsewhere doesn't blur the editor), and the one on screen follows its terminal.
    pub(super) fn sync_boards(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_tasks();
        self.finish_edits_except(visible, window, cx);
        if let Some(id) = visible {
            self.follow_project(id, window, cx);
        }
    }

    /// Saves every board's edit in progress, as the window closes; `false` when one couldn't
    /// be saved and is still open, so the window stays and the idea isn't lost (§1).
    pub(super) fn finish_board_edits(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.finish_edits_except(None, window, cx);
        let lanes: Vec<Entity<TaskBoardView>> =
            self.shown_lanes().iter().map(|l| l.view.clone()).collect();
        for view in &lanes {
            view.update(cx, |v, cx| v.finish_edit(window, cx));
        }
        !self
            .board_views
            .values()
            .chain(lanes.iter())
            .any(|v| v.read(cx).is_editing())
    }

    /// Saves the edit in progress of every board view but terminal `keep`'s and the lanes on
    /// screen.
    fn finish_edits_except(
        &mut self,
        keep: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown: Vec<Entity<TaskBoardView>> =
            self.shown_lanes().iter().map(|l| l.view.clone()).collect();
        let editing: Vec<Entity<TaskBoardView>> = self
            .board_views
            .iter()
            .filter(|(id, _)| Some(**id) != keep)
            .map(|(_, v)| v)
            .chain(
                self.lanes
                    .values()
                    .flatten()
                    .map(|l| &l.view)
                    .filter(|v| !shown.contains(v)),
            )
            .filter(|v| v.read(cx).is_editing())
            .cloned()
            .collect();
        for view in editing {
            view.update(cx, |v, cx| v.finish_edit(window, cx));
        }
    }

    /// When terminal `id` has moved to another project, its tab takes that project's board
    /// (§4), its edit in progress saved to the old board first; an edit that can't be saved
    /// keeps the old board, so it isn't lost.
    fn follow_project(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(view), Some(pane)) = (self.board_views.get(&id).cloned(), self.panes.get(&id))
        else {
            return;
        };
        let project = tasks_project(pane.read(cx).cwd());
        let old = view.read(cx).board().read(cx).project().to_path_buf();
        if old == project {
            return;
        }
        view.update(cx, |v, cx| v.finish_edit(window, cx));
        if view.read(cx).is_editing() {
            return;
        }
        self.new_board_view(id, project, window, cx);
        self.drop_unused_board(&old, cx);
    }

    /// A board view for terminal `id`'s Tasks tab on `project`'s board.
    fn new_board_view(
        &mut self,
        id: PaneId,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (view, events, focus) = self.make_board_view(BoardHost::Tab(id), project, window, cx);
        self.board_focus.insert(id, focus);
        self.board_events.insert(id, events);
        self.board_views.insert(id, view);
    }

    /// A view of `project`'s board for `host`, with its events and its focus watch: a focus in
    /// it takes the focus from the file pane, so Ctrl+W closes the board, not a file.
    pub(super) fn make_board_view(
        &mut self,
        host: BoardHost,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TaskBoardView>, Subscription, Subscription) {
        let board = self.board_for(project, window, cx);
        let targets_board = board.clone();
        let view = cx.new(|cx| TaskBoardView::new(board.clone(), window, cx));
        let env_host = host.clone();
        let events = cx.subscribe_in(
            &view,
            window,
            move |ws, _, ev: &TaskBoardViewEvent, window, cx| match ev {
                TaskBoardViewEvent::ShowCommit(hash) => {
                    let project = board.read(cx).project().to_path_buf();
                    ws.show_commit(project, hash.clone(), window, cx)
                }
                TaskBoardViewEvent::ShowTaker(name) => ws.show_taker(name, window, cx),
                TaskBoardViewEvent::TakeInWorktree(text) => {
                    let project = board.read(cx).project().to_path_buf();
                    if let Some(env) = ws.host_env(&env_host) {
                        ws.take_in_worktree(env, project, text.clone(), window, cx)
                    }
                }
                TaskBoardViewEvent::SendToFree(text) => {
                    let project = board.read(cx).project().to_path_buf();
                    ws.offer_card(project, text.clone(), true, window, cx)
                }
                TaskBoardViewEvent::SendTo { text, pane } => {
                    let project = board.read(cx).project().to_path_buf();
                    ws.send_card_to(*pane, project, text.clone(), window, cx)
                }
            },
        );
        // The card menu's Send to lists the project's idle Claude sessions (auto-pickup §8).
        let ws = cx.entity().downgrade();
        view.update(cx, |v, _| {
            v.set_send_targets(move |cx| {
                let Some(ws) = ws.upgrade() else {
                    return Vec::new();
                };
                let project = targets_board.read(cx).project().to_path_buf();
                ws.read(cx).send_targets(&project, cx)
            })
        });
        let handle = view.read(cx).focus_handle(cx);
        let focus = cx.on_focus_in(&handle, window, move |ws, _, cx| {
            if let Some(env) = ws.host_env(&host) {
                ws.leave_env_files(&env);
            }
            cx.notify();
        });
        (view, events, focus)
    }

    /// The environment a board view sits in.
    fn host_env(&self, host: &BoardHost) -> Option<String> {
        match host {
            BoardHost::Tab(id) => self.envs.env_of(*id).map(str::to_string),
            BoardHost::Lane(env) => Some(env.clone()),
        }
    }

    /// Shows the terminal whose sessions write `name` as a card's taker (MCP server spec §4),
    /// or says it isn't open in this window.
    fn show_taker(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = self
            .panes
            .iter()
            .find(|(_, v)| v.read(cx).taker_name().as_deref() == Some(name))
            .map(|(id, _)| *id);
        match id {
            Some(id) => {
                self.show_terminal(id, window, cx);
                if self.tasks_tabs.get(&id) == Some(&true) {
                    self.show_tasks(id, false, window, cx);
                }
            }
            None => window.push_notification(
                Notification::info(format!("No terminal named {name} is open in this window"))
                    .title("Tasks"),
                cx,
            ),
        }
    }

    /// `project`'s board, made the first time a Tasks tab needs it. One subscription per
    /// board, so terminals sharing it see one question and one toast.
    fn board_for(
        &mut self,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TaskBoard> {
        if let Some((board, _)) = self.boards.get(&project) {
            return board.clone();
        }
        let board = cx.new(|cx| TaskBoard::new(project.clone(), cx));
        let sub = cx.subscribe_in(
            &board,
            window,
            |ws, board, ev: &TaskBoardEvent, window, cx| match ev {
                TaskBoardEvent::AskGit => {
                    let project = board.read(cx).project().to_path_buf();
                    ws.ask_git(project, window, cx)
                }
                TaskBoardEvent::Problem(text) => {
                    window.push_notification(Notification::warning(text.clone()).title("Tasks"), cx)
                }
                TaskBoardEvent::MovedToDone(text) => {
                    let project = board.read(cx).project().to_path_buf();
                    ws.card_done(project, text.clone(), window, cx)
                }
                TaskBoardEvent::ArrivedInPickup(text) => {
                    let project = board.read(cx).project().to_path_buf();
                    ws.card_arrived(project, text.clone(), window, cx)
                }
            },
        );
        self.boards.insert(project, (board.clone(), sub));
        board
    }

    /// Drops `project`'s board, and its watcher, when no Tasks tab or lane uses it. At the end
    /// of the effect cycle, so a question its last change raised still arrives.
    pub(super) fn drop_unused_board(&mut self, project: &Path, cx: &mut Context<Self>) {
        let used = self
            .board_views
            .values()
            .chain(self.lanes.values().flatten().map(|l| &l.view))
            .any(|v| v.read(cx).board().read(cx).project() == project);
        if !used && let Some(entry) = self.boards.remove(project) {
            cx.defer(move |_| drop(entry));
        }
    }

    /// A session made `project`'s board file: the same question as for a card made in the tab.
    pub(super) fn board_created(
        &mut self,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::tasks_project::{Privacy, is_repo, privacy};
        if is_repo(&project) && privacy(&project) == Privacy::Committed {
            self.ask_git(project, window, cx);
        }
    }

    /// The first card was written to a new file in a repository: commit it, or keep it out of
    /// git (§3).
    pub(super) fn ask_git(
        &mut self,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = project
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| project.display().to_string());
        window.open_dialog(cx, move |dialog, _, _| {
            let project = project.clone();
            dialog
                .title("Keep tasks private?")
                .child(div().child(format!(
                    "hydraui made .hydra/tasks.md in {name}. Commit it with the code, or keep \
                     it out of git?"
                )))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("commit-tasks")
                                .label("Commit tasks")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("keep-private")
                                .label("Keep private")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    if let Err(e) = crate::tasks_project::keep_private(&project) {
                                        let text = crate::task_board::privacy_problem(&project, &e);
                                        window.push_notification(
                                            Notification::warning(text).title("Tasks"),
                                            cx,
                                        );
                                    }
                                }),
                        ),
                )
        });
    }

    /// Terminal `id`'s environment no longer has its file pane focused.
    fn leave_files(&mut self, id: PaneId) {
        if let Some(env) = self.envs.env_of(id).map(str::to_string) {
            self.leave_env_files(&env);
        }
    }

    /// Environment `env` no longer has its file pane focused.
    fn leave_env_files(&mut self, env: &str) {
        if let Some(tab) = self.envs.get_mut(env) {
            tab.files_focused = false;
        }
    }

    /// The strip above a terminal with a Tasks or Pipeline tab (§4; orchestrator spec §7.1):
    /// the terminal by its name, then **Tasks ✕** and **Pipeline ✕** when open; the selected one
    /// in the environment's colour, as environment tabs are.
    pub(super) fn render_tasks_strip(
        &self,
        id: PaneId,
        label: String,
        tint: Hsla,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // An orchestrator in a pipeline that runs tickets at once sits with the others: each
        // one's terminal and Pipeline tab, numbered, so any of them is a click away.
        let members = if self.orchestrators.contains_key(&id) {
            self.slots(id, cx)
        } else {
            vec![id]
        };
        let numbered = members.len() > 1;
        let tasks = self.tasks_tabs.get(&id).copied();
        let pipeline_shown = self.pipeline_tabs.get(&id).is_some_and(|t| t.shown);
        let tasks_shown = tasks == Some(true) && !pipeline_shown;
        let tab = |key: &str, of: PaneId, selected: bool| {
            h_flex()
                .id(SharedString::from(format!("{key}-{of}")))
                .px_2()
                .gap_2()
                .h_full()
                .rounded_md()
                .text_sm()
                .when(selected, |d| d.bg(tint.opacity(0.25)))
                .hover(|d| d.bg(tint.opacity(0.15)))
        };
        let close = |key: &str, of: PaneId| {
            div()
                .id(SharedString::from(format!("{key}-{of}")))
                .px_1()
                .text_color(muted)
                .hover(|d| d.text_color(tint))
                .child("✕")
        };
        let mut strip = h_flex()
            .h(px(28.))
            .flex_none()
            .px_1()
            .py_0p5()
            .gap_1()
            .border_b_1()
            .border_color(border);
        // First, fixed and not a button: the pipeline these tabs belong to.
        if let Some((name, accent)) = self.pipeline_of(id, cx) {
            strip = strip.child(
                h_flex()
                    .px_2()
                    .gap_1p5()
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(border)
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(
                        div()
                            .text_xs()
                            .text_color(crate::orchestrator_theme::color(accent))
                            .child("◆"),
                    )
                    .child(name),
            );
        }
        for (n, m) in members.into_iter().enumerate() {
            let here = m == id;
            let name = if here {
                label.clone()
            } else {
                self.panes
                    .get(&m)
                    .map_or_else(|| format!("orchestrator {}", n + 1), |v| v.read(cx).label())
            };
            strip = strip.child(
                tab(
                    "tasks-terminal-tab",
                    m,
                    here && !tasks_shown && !pipeline_shown,
                )
                .on_click(cx.listener(move |ws, _, window, cx| {
                    if !here {
                        ws.show_terminal(m, window, cx);
                    }
                    ws.show_pipeline(m, false, window, cx);
                    ws.show_tasks(m, false, window, cx);
                }))
                .child(name),
            );
            if here && tasks.is_some() {
                strip =
                    strip.child(
                        tab("tasks-tab", m, tasks_shown)
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.show_tasks(m, true, window, cx)
                            }))
                            .child("Tasks")
                            .child(close("tasks-close", m).on_click(cx.listener(
                                move |ws, _, window, cx| {
                                    cx.stop_propagation();
                                    ws.close_tasks(m, window, cx);
                                },
                            ))),
                    );
            }
            if self.pipeline_tabs.contains_key(&m) {
                strip = strip.child(
                    tab("pipeline-tab", m, here && pipeline_shown)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            if !here {
                                ws.show_terminal(m, window, cx);
                            }
                            ws.show_pipeline(m, true, window, cx)
                        }))
                        .child(if numbered {
                            format!("Pipeline {}", n + 1)
                        } else {
                            "Pipeline".to_string()
                        })
                        .child(close("pipeline-close", m).on_click(cx.listener(
                            move |ws, _, window, cx| {
                                cx.stop_propagation();
                                ws.close_pipeline(m, window, cx);
                            },
                        ))),
                );
            }
        }
        strip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_in_a_repo_subfolder_has_the_repo_as_its_project() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        let sub = repo.join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join(".git/HEAD"),
            "ref: refs/heads/main
",
        )
        .unwrap();
        assert_eq!(tasks_project(&sub), repo);
        assert_eq!(tasks_project(&repo), repo);
    }
}
