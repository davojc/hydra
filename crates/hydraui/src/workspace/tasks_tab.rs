//! A terminal's Tasks tab (spec: docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
//! §3-§4): its project's board beside the terminal, a small strip to switch between the two,
//! one board per project shared by every tab on it, and the commit/private question.
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{Context, Entity, Focusable, Hsla, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::task_board::{TaskBoard, TaskBoardEvent};
use crate::task_board_view::TaskBoardView;

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
        let id = self.envs.active()?.shown?;
        (self.tasks_tabs.get(&id) == Some(&true)).then_some(id)
    }

    /// The board view on screen when terminal `id`'s Tasks tab is selected.
    pub(super) fn shown_board(&self, id: PaneId) -> Option<&Entity<TaskBoardView>> {
        if self.tasks_tabs.get(&id) != Some(&true) {
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
        !self.board_views.values().any(|v| v.read(cx).is_editing())
    }

    /// Saves the edit in progress of every board view but terminal `keep`'s.
    fn finish_edits_except(
        &mut self,
        keep: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editing: Vec<Entity<TaskBoardView>> = self
            .board_views
            .iter()
            .filter(|(id, v)| Some(**id) != keep && v.read(cx).is_editing())
            .map(|(_, v)| v.clone())
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

    /// A board view for terminal `id` on `project`'s board; a focus in it takes the focus
    /// from the file pane, so Ctrl+W closes the Tasks tab.
    fn new_board_view(
        &mut self,
        id: PaneId,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let board = self.board_for(project, window, cx);
        let view = cx.new(|cx| TaskBoardView::new(board, window, cx));
        let handle = view.read(cx).focus_handle(cx);
        let focus = cx.on_focus_in(&handle, window, move |ws, _, cx| {
            ws.leave_files(id);
            cx.notify();
        });
        self.board_focus.insert(id, focus);
        self.board_views.insert(id, view);
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
                TaskBoardEvent::AskGit => ws.ask_git(board.clone(), window, cx),
                TaskBoardEvent::Problem(text) => {
                    window.push_notification(Notification::warning(text.clone()).title("Tasks"), cx)
                }
            },
        );
        self.boards.insert(project, (board.clone(), sub));
        board
    }

    /// Drops `project`'s board, and its watcher, when no Tasks tab uses it. At the end of the
    /// effect cycle, so a question its last change raised still arrives.
    fn drop_unused_board(&mut self, project: &Path, cx: &mut Context<Self>) {
        let used = self
            .board_views
            .values()
            .any(|v| v.read(cx).board().read(cx).project() == project);
        if !used && let Some(entry) = self.boards.remove(project) {
            cx.defer(move |_| drop(entry));
        }
    }

    /// The first card was written to a new file in a repository: commit it, or keep it out of
    /// git (§3).
    fn ask_git(&mut self, board: Entity<TaskBoard>, window: &mut Window, cx: &mut Context<Self>) {
        let project = board.read(cx).project().to_path_buf();
        let name = project
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| project.display().to_string());
        window.open_dialog(cx, move |dialog, _, _| {
            let board = board.clone();
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
                                    // A failure comes back as a Problem toast.
                                    board.update(cx, |b, cx| b.set_private(true, cx));
                                }),
                        ),
                )
        });
    }

    /// Terminal `id`'s environment no longer has its file pane focused.
    fn leave_files(&mut self, id: PaneId) {
        if let Some(env) = self.envs.env_of(id).map(str::to_string)
            && let Some(tab) = self.envs.get_mut(&env)
        {
            tab.files_focused = false;
        }
    }

    /// The strip above a terminal with a Tasks tab (§4): the terminal by its name, then
    /// **Tasks ✕**; the selected one in the environment's colour, as environment tabs are.
    pub(super) fn render_tasks_strip(
        &self,
        id: PaneId,
        label: String,
        tint: Hsla,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shown = self.tasks_tabs.get(&id) == Some(&true);
        let tab = |key: &str, selected: bool| {
            h_flex()
                .id(SharedString::from(format!("{key}-{id}")))
                .px_2()
                .gap_2()
                .h_full()
                .rounded_md()
                .text_sm()
                .when(selected, |d| d.bg(tint.opacity(0.25)))
                .hover(|d| d.bg(tint.opacity(0.15)))
        };
        h_flex()
            .h(px(28.))
            .flex_none()
            .px_1()
            .py_0p5()
            .gap_1()
            .border_b_1()
            .border_color(border)
            .child(
                tab("tasks-terminal-tab", !shown)
                    .on_click(
                        cx.listener(move |ws, _, window, cx| ws.show_tasks(id, false, window, cx)),
                    )
                    .child(label),
            )
            .child(
                tab("tasks-tab", shown)
                    .on_click(
                        cx.listener(move |ws, _, window, cx| ws.show_tasks(id, true, window, cx)),
                    )
                    .child("Tasks")
                    .child(
                        div()
                            .id(SharedString::from(format!("tasks-close-{id}")))
                            .px_1()
                            .text_color(muted)
                            .hover(|d| d.text_color(tint))
                            .child("✕")
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                cx.stop_propagation();
                                ws.close_tasks(id, window, cx);
                            })),
                    ),
            )
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
