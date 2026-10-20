//! All boards (task board spec §10): one read-only look at every project with a terminal open
//! in the window, its Pickup, InProgress and Verify cards; clicking a project opens its Tasks tab.
use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{Context, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::tasks::{Board, Card, Column};
use crate::tasks_project::{load, project_root};

/// The columns the overview shows: work approved, under way, and waiting to be checked.
const SHOWN: [Column; 3] = [Column::Pickup, Column::InProgress, Column::Verify];

/// `board`'s cards in the shown columns, leaving out empty columns.
fn active_cards(board: &Board) -> Vec<(Column, Vec<Card>)> {
    SHOWN
        .into_iter()
        .map(|col| (col, board.cards(col).to_vec()))
        .filter(|(_, cards)| !cards.is_empty())
        .collect()
}

impl Workspace {
    /// Every project a terminal in the window is in, with a terminal to open its board from:
    /// the shown terminal of the selected environment when it is in that project.
    fn open_projects(&self, cx: &gpui::App) -> BTreeMap<PathBuf, PaneId> {
        let shown = self.envs.active().and_then(|t| t.shown);
        let mut projects = BTreeMap::new();
        for (id, view) in &self.panes {
            let project = project_root(view.read(cx).cwd());
            let entry = projects.entry(project).or_insert(*id);
            if Some(*id) == shown {
                *entry = *id;
            }
        }
        projects
    }

    /// Opens the All boards dialog.
    pub(super) fn all_boards(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let projects: Vec<(PathBuf, PaneId, Board)> = self
            .open_projects(cx)
            .into_iter()
            .filter_map(|(project, id)| {
                let (board, _) = load(&project).ok()?;
                Some((project, id, board))
            })
            .collect();
        let ws = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let mut list = v_flex().gap_3();
            let mut any = false;
            for (i, (project, id, board)) in projects.iter().enumerate() {
                let columns = active_cards(board);
                if columns.is_empty() {
                    continue;
                }
                any = true;
                let name = project
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| project.display().to_string());
                let (ws, id) = (ws.clone(), *id);
                let mut block = v_flex().gap_1().child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(("all-boards-project", i))
                                .label(SharedString::from(name))
                                .ghost()
                                .small()
                                .tooltip("Open this project's Tasks tab")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    ws.update(cx, |ws, cx| {
                                        ws.show_terminal(id, window, cx);
                                        ws.open_tasks(id, window, cx);
                                    });
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(project.display().to_string()),
                        ),
                );
                for (col, cards) in columns {
                    let colour = crate::task_board_view::accent(col, cx);
                    block = block.child(div().pl_3().text_xs().text_color(colour).child(format!(
                        "{} ({})",
                        col.heading(),
                        cards.len()
                    )));
                    for card in cards {
                        let taker = card.taker.map(|t| format!("  [{t}]")).unwrap_or_default();
                        block = block.child(
                            div()
                                .pl_5()
                                .text_sm()
                                .child(format!("• {}{taker}", card.text)),
                        );
                    }
                }
                list = list.child(block);
            }
            if !any {
                list = list.child(
                    div()
                        .text_color(muted)
                        .child("No cards in Pickup, InProgress or Verify in the open projects."),
                );
            }
            dialog.title("All boards").w(px(640.)).child(
                div()
                    .id("all-boards")
                    .max_h(px(560.))
                    .overflow_y_scroll()
                    .child(list),
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pickup_inprogress_and_verify_show() {
        let board = crate::tasks::parse(
            "## Backlog\n- idea\n## Pickup\n- next\n## InProgress\n## Verify\n- [c @beef] check\n## Done\n- old\n",
        );
        let got: Vec<(Column, Vec<String>)> = active_cards(&board)
            .into_iter()
            .map(|(c, cards)| (c, cards.into_iter().map(|k| k.text).collect()))
            .collect();
        assert_eq!(
            got,
            [
                (Column::Pickup, vec!["next".to_string()]),
                (Column::Verify, vec!["check".to_string()])
            ]
        );
    }
}
