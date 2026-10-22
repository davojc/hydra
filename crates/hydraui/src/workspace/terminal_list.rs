//! The terminal list on the left (env-tabs design §2.2): **+ New**, then the selected
//! environment's terminals in creation order, grouped by project with a line between groups,
//! each with its activity dot, its name and × on hover; click shows one, right-click opens its
//! Tasks tab, renames, restarts or closes it.
use std::cell::Cell;

use gpui::prelude::*;
use gpui::{Context, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::activity::Activity;
use crate::envs::PaneId;
use crate::pane_view::label_for;

/// `terminals` (each with its project) grouped by project: groups in the order their first
/// terminal came, terminals in their order within each.
fn by_project<P: PartialEq + Clone>(terminals: &[(PaneId, P)]) -> Vec<(P, Vec<PaneId>)> {
    let mut groups: Vec<(P, Vec<PaneId>)> = Vec::new();
    for (id, project) in terminals {
        match groups.iter_mut().find(|(p, _)| p == project) {
            Some((_, ids)) => ids.push(*id),
            None => groups.push((project.clone(), vec![*id])),
        }
    }
    groups
}

/// The terminal `by` steps from `shown` in `order`, wrapping; the first when `shown` isn't in it.
fn step(order: &[PaneId], shown: Option<PaneId>, by: isize) -> Option<PaneId> {
    let at = shown
        .and_then(|s| order.iter().position(|&t| t == s))
        .unwrap_or(0) as isize;
    let len = order.len().max(1) as isize;
    order.get((at + by).rem_euclid(len) as usize).copied()
}

impl Workspace {
    /// The selected environment's terminals grouped by project (worktrees of one repository
    /// included), in the order the list shows them.
    fn grouped_terminals(&self, cx: &gpui::App) -> Vec<(std::path::PathBuf, Vec<PaneId>)> {
        let Some(tab) = self.envs.active() else {
            return Vec::new();
        };
        let projects: Vec<(PaneId, std::path::PathBuf)> = tab
            .terminals
            .iter()
            .filter_map(|id| {
                let cwd = self.panes.get(id)?.read(cx).cwd().clone();
                Some((*id, crate::tasks_project::project_root(&cwd)))
            })
            .collect();
        by_project(&projects)
    }

    /// Ctrl+PageDown / Ctrl+PageUp: the next or previous terminal down the list, wrapping.
    pub(super) fn step_terminal(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order: Vec<PaneId> = self
            .grouped_terminals(cx)
            .into_iter()
            .flat_map(|(_, ids)| ids)
            .collect();
        let shown = self.envs.active().and_then(|t| t.shown);
        if let Some(id) = step(&order, shown, by) {
            self.show_terminal(id, window, cx);
        }
    }

    /// The selected environment's terminals, the shown one highlighted in the environment's
    /// colour; a row's tooltip is the title its program set, when it set one.
    pub(super) fn render_terminal_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // The terminals scroll; the allowance meter stays at the panel's foot.
        let panel = v_flex().w(px(self.list_width)).h_full().flex_none();
        let mut list = v_flex()
            .id("terminal-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_1()
            .gap_0p5();
        let Some(tab) = self.envs.active() else {
            return panel.child(list);
        };
        let (env, tint) = (tab.env.clone(), self.tint(&tab.env));
        // Pinned above the terminals: the environment's projects' boards (task board §10).
        list = list
            .child(self.render_all_tasks_entry(tint, cx))
            .child(div().h(px(1.)).mx_2().my_0p5().bg(cx.theme().border));
        list = list.child(
            Button::new("list-new-terminal")
                .label("+ New")
                .ghost()
                .small()
                .tooltip("New terminal (Ctrl+T)")
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.new_terminal(&env, None, window, cx);
                })),
        );
        // Terminals in the same project sit together, a line between one project's and the
        // next's.
        let all_tasks = self.all_tasks_shown();
        let rows: Vec<(bool, PaneId)> = self
            .grouped_terminals(cx)
            .into_iter()
            .enumerate()
            .flat_map(|(g, (_, ids))| {
                ids.into_iter()
                    .enumerate()
                    .map(move |(i, id)| (g > 0 && i == 0, id))
            })
            .collect();
        // A run's orchestrator and stage terminals sit under its pipeline card, not in the
        // project groups (orchestrator spec §7.1).
        let cards = self.pipeline_cards(cx);
        let carded: std::collections::HashSet<PaneId> = cards
            .iter()
            .flat_map(|(_, _, panes)| panes.iter().copied())
            .collect();
        let shown = tab.shown.filter(|_| !all_tasks);
        for (orch, card, panes) in &cards {
            let active = panes.iter().any(|p| Some(*p) == shown);
            list = list.child(self.render_pipeline_card(*orch, card, active, cx));
            let orch = *orch;
            let history_shown = self.pipeline_shown(orch)
                && self.pipeline_tabs.get(&orch).map(|t| t.view)
                    == Some(super::pipeline_tab::TabView::History);
            list = list.child(
                div()
                    .id(SharedString::from(format!("pipeline-history-link-{orch}")))
                    .ml(px(26.))
                    .mr_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(12.5))
                    .text_color(cx.theme().muted_foreground)
                    .when(history_shown, |d| d.bg(tint.opacity(0.2)))
                    .hover(|d| d.bg(tint.opacity(0.12)))
                    .cursor_pointer()
                    .child("Ticket history")
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.show_pipeline_view(
                            orch,
                            super::pipeline_tab::TabView::History,
                            window,
                            cx,
                        )
                    })),
            );
            let settings_shown = self.pipeline_shown(orch)
                && self.pipeline_tabs.get(&orch).map(|t| t.view)
                    == Some(super::pipeline_tab::TabView::Settings);
            list = list.child(
                div()
                    .id(SharedString::from(format!("pipeline-settings-link-{orch}")))
                    .ml(px(26.))
                    .mr_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(12.5))
                    .text_color(cx.theme().muted_foreground)
                    .when(settings_shown, |d| d.bg(tint.opacity(0.2)))
                    .hover(|d| d.bg(tint.opacity(0.12)))
                    .cursor_pointer()
                    .child("Pipeline settings")
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.open_pipeline_settings(orch, window, cx)
                    })),
            );
            for id in panes {
                if let Some(row) = self.terminal_row(*id, shown == Some(*id), tint, true, cx) {
                    list = list.child(row);
                }
            }
        }
        for (divided, id) in rows.into_iter().filter(|(_, id)| !carded.contains(id)) {
            if divided {
                list = list.child(
                    div()
                        .h(px(2.))
                        .mx_1()
                        .my_2()
                        .rounded_full()
                        .bg(tint.opacity(0.6)),
                );
            }
            if let Some(row) = self.terminal_row(id, shown == Some(id), tint, false, cx) {
                list = list.child(row);
            }
        }
        let meter = if cards.is_empty() {
            None
        } else {
            self.render_allowance_meter(&tab.env)
        };
        panel
            .child(list)
            .children(meter.map(|m| div().flex_none().p_1().pb_3().child(m)))
    }

    /// Terminal `id`'s row: its activity dot, name, Claude's state and × on hover;
    /// `highlighted` when it is the one shown, `indent` under a pipeline card.
    fn terminal_row(
        &self,
        id: PaneId,
        highlighted: bool,
        tint: gpui::Hsla,
        indent: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let muted = cx.theme().muted_foreground;
        let view = self.panes.get(&id)?;
        let v = view.read(cx);
        let (label, program) = (v.label(), v.program_title().map(str::to_string));
        let activity = self.activity.get(&id).copied();
        let dot = self.render_tab_dot(activity, tint, cx);
        // What a Claude session here says it is doing (its hooks), beside its name.
        let claude = activity.filter(|a| a.claude).map(|a| match a.state {
            Activity::Working => ("working", muted),
            Activity::Waiting => ("needs you", cx.theme().warning),
            Activity::Idle => ("idle", muted),
        });
        // Keyed by the terminal, so element state stays with it when another closes.
        let group = SharedString::from(format!("terminal-row-{id}"));
        Some(
            h_flex()
                .id(SharedString::from(format!("terminal-{id}")))
                .group(group.clone())
                .pl(px(if indent { 22. } else { 8. }))
                .pr_2()
                .py_1()
                .gap_2()
                .rounded_md()
                .when(highlighted, |d| d.bg(tint.opacity(0.25)))
                .hover(|d| d.bg(tint.opacity(0.15)))
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.show_terminal(id, window, cx);
                    // A row is the terminal itself: an orchestrator's Pipeline tab is the
                    // card's (orchestrator spec §7.1).
                    if ws.pipeline_shown(id) {
                        ws.show_pipeline(id, false, window, cx);
                    }
                }))
                .when_some(program, |d, title| {
                    d.tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                    })
                })
                .context_menu({
                    let ws = cx.entity();
                    move |menu, _, _| {
                        let (tasks_ws, rename_ws, restart_ws, close_ws) =
                            (ws.clone(), ws.clone(), ws.clone(), ws.clone());
                        // Its project's board as a Tasks tab beside it (task-board §4).
                        menu.item(PopupMenuItem::new("Tasks").on_click(move |_, window, cx| {
                            tasks_ws.update(cx, |ws, cx| {
                                if ws.envs.active().and_then(|t| t.shown) != Some(id) {
                                    ws.show_terminal(id, window, cx);
                                }
                                ws.open_tasks(id, window, cx);
                            });
                        }))
                        .item(
                            PopupMenuItem::new("Rename…").on_click(move |_, window, cx| {
                                rename_ws.update(cx, |ws, cx| ws.rename_dialog(id, window, cx));
                            }),
                        )
                        .item(
                            PopupMenuItem::new("Restart").on_click(move |_, window, cx| {
                                restart_ws.update(cx, |ws, cx| ws.restart_terminal(id, window, cx));
                            }),
                        )
                        .item(PopupMenuItem::new("Close").on_click(move |_, window, cx| {
                            close_ws.update(cx, |ws, cx| ws.close_terminal(id, window, cx));
                        }))
                    }
                })
                .child(dot)
                .child(div().flex_1().min_w_0().text_sm().truncate().child(label))
                .children(
                    claude.map(|(text, color)| {
                        div().flex_none().text_xs().text_color(color).child(text)
                    }),
                )
                // Out of sight when its Claude session finished or asked for you.
                .when(self.attention.contains(&id), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .size(px(7.))
                            .rounded_full()
                            .bg(cx.theme().warning),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!("terminal-close-{id}")))
                        .px_1()
                        .text_color(muted)
                        .invisible()
                        .group_hover(group, |d| d.visible())
                        .hover(|d| d.text_color(tint))
                        .child("×")
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.close_terminal(id, window, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// Asks for terminal `id`'s name (env-tabs design §2.2); Enter confirms, and an empty name
    /// goes back to the folder's name.
    pub(super) fn rename_dialog(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.panes.get(&id) else {
            return;
        };
        let (current, folder) = {
            let v = view.read(cx);
            (v.name().map(str::to_string), label_for(None, v.cwd()))
        };
        let input = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder(folder.clone());
            match &current {
                Some(name) => input.default_value(name.clone()),
                None => input,
            }
        });
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let typed = input.read(cx).value().to_string();
            let (ok_ws, ok_name) = (workspace.clone(), typed.clone());
            let (button_ws, button_name) = (workspace.clone(), typed);
            dialog
                .title("Rename terminal")
                .on_ok(move |_, window, cx| {
                    let name = ok_name.clone();
                    ok_ws.update(cx, |ws, cx| ws.rename(id, name, window, cx));
                    false
                })
                .child(
                    v_flex().gap_2().child(Input::new(&input)).child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Empty: named after its folder, {folder}")),
                    ),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("ok").label("Rename").primary().on_click(
                            move |_, window, cx| {
                                let name = button_name.clone();
                                button_ws.update(cx, |ws, cx| ws.rename(id, name, window, cx));
                            },
                        )),
                )
        });
    }

    /// Names terminal `id` and saves the layout: names are the user's and are kept, unlike the
    /// titles programs set, which don't save.
    fn rename(&mut self, id: PaneId, name: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(view) = self.panes.get(&id) {
            view.update(cx, |v, cx| v.set_name(Some(name), cx));
        }
        self.autosave(window, cx);
        // Again on the next frame, after the closing dialog hands the focus back.
        if let Some(shown) = self.envs.active().and_then(|t| t.shown) {
            self.focus_soon(shown, window, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_group_by_project_in_first_seen_order() {
        let got = by_project(&[
            (1, "api"),
            (2, "blog"),
            (3, "api"),
            (4, "site"),
            (5, "blog"),
        ]);
        assert_eq!(
            got,
            [("api", vec![1, 3]), ("blog", vec![2, 5]), ("site", vec![4])]
        );
        assert!(by_project::<&str>(&[]).is_empty());
    }

    #[test]
    fn stepping_follows_the_list_and_wraps() {
        let order = [1, 3, 2];
        assert_eq!(step(&order, Some(1), 1), Some(3));
        assert_eq!(step(&order, Some(2), 1), Some(1));
        assert_eq!(step(&order, Some(1), -1), Some(2));
        assert_eq!(step(&order, None, 1), Some(3));
        assert_eq!(step(&[], Some(1), 1), None);
    }
}
