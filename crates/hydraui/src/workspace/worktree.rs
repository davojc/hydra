//! A worktree per task card (task board spec §8): "Take in new worktree" on a Pickup card makes
//! the card's branch and worktree, opens a Claude terminal in it and takes the card for that
//! terminal; moving the card to Done offers to merge the branch back and remove the worktree.
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{Context, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::hydra::{PaneRequest, command_line};
use crate::tasks::Change;
use crate::worktrees::{self, Worktree};

/// What the new terminal's Claude is told to do.
fn prompt(card: &str, wt: &Worktree) -> String {
    format!(
        "Your task-board card, already taken for you (it is in InProgress): \"{card}\". Do the \
         work in this worktree, on branch {}, and commit it there. Then move the card to Verify \
         with tasks_verify, giving the commit.",
        wt.branch
    )
}

impl Workspace {
    /// Where hydraui remembers which card has which worktree.
    fn worktree_records(&self) -> PathBuf {
        self.hydra
            .paths
            .root()
            .join("hydraui")
            .join("worktrees.toml")
    }

    /// "Take in new worktree" on the Pickup card `card` of `project`'s board, from a board in
    /// environment `env`: the worktree is made off the UI thread, then a terminal in `env`
    /// opens in it running Claude, and the card is taken for that terminal.
    pub(super) fn take_in_worktree(
        &mut self,
        env: String,
        project: PathBuf,
        card: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let repo = project.clone();
            let text = card.clone();
            let made = cx
                .background_spawn(async move { worktrees::create(&repo, &text) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| match made {
                Ok(wt) => ws.open_worktree(&env, project, card, wt, window, cx),
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't make a worktree: {e}"))
                        .title("Take in new worktree"),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Opens the Claude terminal in a new worktree and takes the card for it.
    fn open_worktree(
        &mut self,
        env: &str,
        project: PathBuf,
        card: String,
        wt: Worktree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shell = self.shell();
        let mut request = PaneRequest::new(env, wt.path.clone(), shell);
        request.command = Some(command_line(
            shell,
            &["claude".to_string(), prompt(&card, &wt)],
        ));
        let id = self.open_terminal(env, request, window, cx);
        let taker = self
            .panes
            .get(&id)
            .and_then(|v| v.read(cx).taker_name())
            .unwrap_or_else(|| crate::pane_view::label_for(None, &wt.path));
        if let Err(e) = worktrees::remember(&self.worktree_records(), &wt) {
            window.push_notification(
                Notification::warning(format!(
                    "Couldn't record the worktree, so Done won't offer to merge it: {e}"
                ))
                .title("Take in new worktree"),
                cx,
            );
        }
        if let Some(board) = self.boards.get(&project).map(|(b, _)| b.clone()) {
            board.update(cx, |b, cx| {
                b.change(Change::Take { text: card, taker }, cx);
            });
        }
    }

    /// The card `card` of `project`'s board went to Done: when it has a worktree, offer to merge
    /// its branch and remove the worktree.
    pub(super) fn card_done(
        &mut self,
        project: PathBuf,
        card: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(wt) = worktrees::find(&self.worktree_records(), &project, &card) else {
            return;
        };
        let ws = cx.entity();
        let body = format!(
            "Merges {} into {} in {}, then removes its worktree {} and deletes the branch. \
             Terminals open in the worktree are closed.",
            wt.branch,
            wt.base,
            wt.repo.display(),
            wt.path.display()
        );
        let title = format!("Merge {}?", wt.branch);
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, wt) = (ws.clone(), wt.clone());
            dialog
                .title(title.clone())
                .child(v_flex().child(div().child(body.clone())))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("not-now")
                                .label("Not now")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("merge").label("Merge").primary().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let wt = wt.clone();
                                ws.update(cx, |ws, cx| ws.merge_worktree(wt, window, cx));
                            },
                        )),
                )
        });
    }

    /// Closes the terminals in `wt`, then merges and removes it off the UI thread, saying how it
    /// went.
    fn merge_worktree(&mut self, wt: Worktree, window: &mut Window, cx: &mut Context<Self>) {
        let inside: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, v)| {
                let v = v.read(cx);
                v.cwd().starts_with(&wt.path) || v.request.cwd.starts_with(&wt.path)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in inside {
            self.close_terminal_now(id, window, cx);
        }
        let records = self.worktree_records();
        cx.spawn_in(window, async move |this, cx| {
            let target = wt.clone();
            let merged = cx
                .background_spawn(async move {
                    // The closed shells' processes take a moment to let go of the folder.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    worktrees::merge_and_remove(&target)
                })
                .await;
            let _ = this.update_in(cx, |_, window, cx| {
                let note = match merged {
                    Ok(done) => {
                        let _ = worktrees::forget(&records, &wt);
                        Notification::success(done)
                    }
                    Err(e) => Notification::error(e),
                };
                window.push_notification(note.title(format!("Merge {}", wt.branch)), cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_names_the_card_branch_and_verify() {
        let wt = Worktree {
            repo: "C:/r".into(),
            card: "Add orders".into(),
            path: "C:/r-worktrees/add-orders".into(),
            branch: "task/add-orders".into(),
            base: "main".into(),
        };
        let p = prompt("Add orders", &wt);
        assert!(p.contains("\"Add orders\"") && p.contains("task/add-orders"));
        assert!(p.contains("tasks_verify"));
    }
}
