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
use gpui_component::{ActiveTheme, Selectable, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::activity::Activity;
use crate::envs::PaneId;
use crate::orchestrator_theme::color;
use crate::pane_view::label_for;
use crate::setup::{self, ToolState};

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

/// Where a row sits in the list: on its own, as a pipeline's orchestrator, or as one of the
/// sessions that orchestrator started.
#[derive(Clone, Copy, PartialEq)]
enum Nest {
    Plain,
    Orchestrator,
    Stage,
}

/// Which of the list's two tabs is shown: the environment's own sessions, or its pipelines
/// with their orchestrators and the sessions they started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ListTab {
    Sessions,
    Pipelines,
}

/// A muted line explaining an empty tab.
fn list_hint(text: &'static str) -> gpui::Div {
    div()
        .px_3()
        .py_2()
        .text_xs()
        .text_color(crate::orchestrator_theme::color(
            crate::orchestrator_theme::TEXT_MUTED,
        ))
        .whitespace_normal()
        .child(text)
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
        // Until the environment's essential tools are added and signed in, a checklist says
        // what's left, each with its next step (spec §48).
        let tools = self.hydra.tools(&env);
        let reports = self.status.get(&env);
        if !self.ui.setup_hidden.contains(&env) {
            list = list.children(self.render_setup_checklist(&env, &tools, reports, tint, cx));
        }
        // Pipelines start where Claude is set up: their orchestrator is a Claude session.
        let claude_state = setup::tool_state("claude", &tools, reports);
        let claude = claude_state == ToolState::Ready;
        let (start_env, new_env) = (env.clone(), env.clone());
        let open = Button::new("list-new-terminal")
            .label("Open session")
            .outline()
            .small()
            .w_full()
            .tooltip("A new terminal in this environment (Ctrl+T)")
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.new_terminal(&env, None, window, cx);
            }));
        // Start pipeline filled: it's what most people come here for; Create pipeline beside it.
        let pipeline_actions = claude.then(|| {
            h_flex()
                .px_2()
                .pt_1()
                .pb_1p5()
                .gap_1p5()
                .child(
                    Button::new("list-start-pipeline")
                        .label("▶  Start pipeline")
                        .primary()
                        .small()
                        .flex_1()
                        .tooltip("Start an orchestrator on one of your pipelines")
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.choose_pipeline(start_env.clone(), window, cx);
                        })),
                )
                .child(
                    Button::new("list-new-pipeline")
                        .label("Create pipeline")
                        .outline()
                        .small()
                        .flex_1()
                        .tooltip("Make a pipeline, blank or from a copy, and edit its settings")
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.new_pipeline(Some(new_env.clone()), window, cx);
                        })),
                )
        });
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
        // Runs an earlier hydraui closed on, in this environment, to pick back up (§13).
        let interrupted: Vec<usize> = self
            .interrupted
            .iter()
            .enumerate()
            .filter(|(_, i)| i.env == tab.env)
            .map(|(idx, _)| idx)
            .collect();
        // Sessions and pipelines are kept apart: two tabs, each with its own actions.
        let plain = rows.iter().filter(|(_, id)| !carded.contains(id)).count();
        let tab_now = self
            .list_tab
            .unwrap_or(if cards.is_empty() && interrupted.is_empty() {
                ListTab::Sessions
            } else {
                ListTab::Pipelines
            });
        list = list.child(self.render_list_tabs(tab_now, plain, cards.len(), tint, cx));
        if tab_now == ListTab::Pipelines && self.ui.pipelines_condensed {
            // Condensed: only what's running, one line per ticket (spec §49).
            list = list.child(self.render_condensed_pipelines(&cards, shown, cx));
        } else if tab_now == ListTab::Pipelines {
            list = list.children(pipeline_actions);
            // Whether the team's plugins (the skill router) are on where stages will run.
            let plugin_lines = self.plugin_lines(&tab.env, "list-plugin", cx);
            if !plugin_lines.is_empty() {
                list = list.child(v_flex().px_2().pb_1p5().gap_0p5().children(plugin_lines));
            }
            for &idx in &interrupted {
                list = list.child(self.render_interrupted_card(idx, cx));
            }
            // Each pipeline is one framed block: its card, links, then each orchestrator's row with
            // the sessions it started nested under it, so pipelines' sessions never mix. A pipeline
            // running several tickets at once has several orchestrators in its one block.
            for (orch, card, panes) in &cards {
                let slots = self.slots(*orch, cx);
                if slots[0] != *orch {
                    continue;
                }
                // Folded: one line, enough to see whether it needs anything.
                if self
                    .orchestrators
                    .get(orch)
                    .is_some_and(|name| self.collapsed_pipelines.contains(name))
                {
                    list = list.child(self.render_collapsed_pipeline(&slots, &cards, shown, cx));
                    continue;
                }
                if slots.len() > 1 {
                    list = list.child(self.render_pipeline_group(&slots, &cards, shown, cx));
                    continue;
                }
                let active = panes.iter().any(|p| Some(*p) == shown);
                let lanes = self.lanes(*orch, cx);
                let mut block = v_flex()
                    .id(SharedString::from(format!("pipeline-block-{orch}")))
                    .mx_1()
                    .my_1()
                    .pb_1()
                    .gap_0p5()
                    .rounded(px(10.))
                    .border_1()
                    // The pipeline's own colour, so pipelines are told apart at a glance.
                    .border_color(color(card.accent).opacity(if active { 0.55 } else { 0.3 }))
                    .child(self.render_pipeline_card(*orch, card, active, true, cx))
                    // With several tickets, each one's question is in its own box.
                    .children(
                        lanes
                            .is_empty()
                            .then(|| self.render_question(*orch, None, cx))
                            .flatten(),
                    )
                    .children(self.render_linear_row(*orch, card.accent, cx));
                let orch = *orch;
                if let Some(row) = self.terminal_row(
                    orch,
                    shown == Some(orch),
                    color(card.accent),
                    Nest::Orchestrator,
                    cx,
                ) {
                    block = block.child(row);
                }
                // Several tickets under the one orchestrator: each is a box holding its own
                // sessions, hanging off the orchestrator's guide line, so it's plain which
                // sessions work on which ticket and that the orchestrator oversees them all.
                let several = !lanes.is_empty();
                if several {
                    block =
                        block.child(self.render_lanes(orch, lanes, shown, color(card.accent), cx));
                }
                let stage_rows: Vec<_> = panes
                    .iter()
                    .filter(|id| **id != orch && !several)
                    .filter_map(|id| {
                        self.terminal_row(
                            *id,
                            shown == Some(*id),
                            color(card.accent),
                            Nest::Stage,
                            cx,
                        )
                    })
                    .collect();
                if !stage_rows.is_empty() {
                    // A guide line down from the orchestrator ties its sessions to it.
                    block = block.child(
                        v_flex()
                            .ml(px(18.))
                            .mr_1()
                            .pl(px(6.))
                            .gap_0p5()
                            .border_l_2()
                            .border_color(color(card.accent).opacity(0.45))
                            .children(stage_rows),
                    );
                }
                list = list.child(block);
            }
            if cards.is_empty() && interrupted.is_empty() {
                list = list.child(list_hint(setup::pipeline_hint(claude_state)));
                if !claude {
                    let env = tab.env.clone();
                    let label = if claude_state == ToolState::NotAdded {
                        "Add Claude"
                    } else {
                        "Sign in to Claude"
                    };
                    list = list.child(
                        div().px_2().pb_1p5().child(
                            Button::new("list-claude-next")
                                .label(label)
                                .primary()
                                .small()
                                .w_full()
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.set_up_tool(env.clone(), "claude".into(), window, cx);
                                })),
                        ),
                    );
                }
            }
        } else {
            list = list.child(div().px_2().pt_1().pb_1p5().child(open));
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
                if let Some(row) = self.terminal_row(id, shown == Some(id), tint, Nest::Plain, cx) {
                    list = list.child(row);
                }
            }
            if plain == 0 {
                list = list.child(list_hint(
                    "No sessions open. Open one for a terminal of your own; a pipeline's sessions \
                     are under Pipelines.",
                ));
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

    /// A folded pipeline (orchestrators `slots`): its name and state pill, then how many sessions
    /// it has open and how its trigger is set. Its arrow unfolds it; the rest opens it.
    fn render_collapsed_pipeline(
        &self,
        slots: &[PaneId],
        cards: &[(PaneId, crate::pipeline_card::Card, Vec<PaneId>)],
        shown: Option<PaneId>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let members: Vec<&(PaneId, crate::pipeline_card::Card, Vec<PaneId>)> = slots
            .iter()
            .filter_map(|s| cards.iter().find(|(o, _, _)| o == s))
            .collect();
        let lead = slots[0];
        let c = if members.len() == 1 {
            members[0].1.clone()
        } else {
            crate::pipeline_card::group_card(&members.iter().map(|(_, c, _)| c).collect::<Vec<_>>())
        };
        let sessions = members
            .iter()
            .map(|(o, _, panes)| panes.iter().filter(|p| *p != o).count())
            .sum();
        let mode = self.trigger_mode(lead, cx).map(|m| m.label());
        let waiting = self
            .slack_of(lead, cx)
            .map_or(0, |(p, _, _)| self.slack_waiting(&p.file));
        let active = members
            .iter()
            .any(|(_, _, panes)| panes.iter().any(|p| Some(*p) == shown));
        let (fg, bg) = super::orchestrator::pill_colors(c.state, c.accent);
        v_flex()
            .id(SharedString::from(format!("pipeline-folded-{lead}")))
            .mx_1()
            .my_1()
            .px(px(10.))
            .py(px(7.))
            .gap(px(3.))
            .rounded(px(10.))
            .border_1()
            .border_color(color(c.accent).opacity(if active { 0.55 } else { 0.3 }))
            .bg(color(crate::orchestrator_theme::SURFACE))
            .cursor_pointer()
            .on_click(cx.listener(move |ws, _, window, cx| ws.open_pipeline(lead, window, cx)))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(color(c.accent)).child("◆"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(color(crate::orchestrator_theme::TEXT))
                            .child(c.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .text_xs()
                            .bg(bg)
                            .text_color(fg)
                            .child(c.state.word()),
                    )
                    .child(self.collapse_toggle(lead, true, cx)),
            )
            .child(
                div()
                    .pl(px(18.))
                    .truncate()
                    .text_xs()
                    .text_color(color(crate::orchestrator_theme::TEXT_SECONDARY))
                    .child(crate::pipeline_card::collapsed_line(
                        sessions, mode, waiting,
                    )),
            )
            .into_any_element()
    }

    /// The arrow that folds or unfolds orchestrator `orch`'s pipeline in the sidebar.
    pub(super) fn collapse_toggle(
        &self,
        orch: PaneId,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        Button::new(SharedString::from(format!("pipeline-fold-{orch}")))
            .icon(if collapsed {
                gpui_component::IconName::ChevronRight
            } else {
                gpui_component::IconName::ChevronDown
            })
            .outline()
            .xsmall()
            .tooltip(if collapsed {
                "Expand: show its sessions"
            } else {
                "Collapse to one line"
            })
            .on_click(cx.listener(move |ws, _, _, cx| {
                // The card under it opens the pipeline; the arrow only folds.
                cx.stop_propagation();
                if let Some(name) = ws.orchestrators.get(&orch).cloned()
                    && !ws.collapsed_pipelines.remove(&name)
                {
                    ws.collapsed_pipelines.insert(name);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// The block of a pipeline running several tickets at once: one card for the pipeline, its
    /// trigger, then each orchestrator (`orchestrator 1`, `2`, …) with its ticket and the
    /// sessions it started nested under it.
    fn render_pipeline_group(
        &self,
        slots: &[PaneId],
        cards: &[(PaneId, crate::pipeline_card::Card, Vec<PaneId>)],
        shown: Option<PaneId>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let members: Vec<&(PaneId, crate::pipeline_card::Card, Vec<PaneId>)> = slots
            .iter()
            .filter_map(|s| cards.iter().find(|(o, _, _)| o == s))
            .collect();
        let lead = slots[0];
        let group = crate::pipeline_card::group_card(
            &members.iter().map(|(_, c, _)| c).collect::<Vec<_>>(),
        );
        let active = members
            .iter()
            .any(|(_, _, panes)| panes.iter().any(|p| Some(*p) == shown));
        let accent = color(group.accent);
        let mut block = v_flex()
            .id(SharedString::from(format!("pipeline-group-{lead}")))
            .mx_1()
            .my_1()
            .pb_1()
            .gap_0p5()
            .rounded(px(10.))
            .border_1()
            .border_color(accent.opacity(if active { 0.55 } else { 0.3 }))
            .child(self.render_pipeline_card(lead, &group, active, false, cx))
            .children(self.render_linear_row(lead, group.accent, cx));
        for (orch, _, panes) in members {
            let orch = *orch;
            if let Some(row) =
                self.terminal_row(orch, shown == Some(orch), accent, Nest::Orchestrator, cx)
            {
                block = block.child(row);
            }
            // Its tickets, each a box holding its sessions.
            let lanes = self.open_lanes(orch, cx);
            if !lanes.is_empty() {
                block = block.child(self.render_lanes(orch, lanes, shown, accent, cx));
                continue;
            }
            let stage_rows: Vec<_> = panes
                .iter()
                .filter(|id| **id != orch)
                .filter_map(|id| {
                    self.terminal_row(*id, shown == Some(*id), accent, Nest::Stage, cx)
                })
                .collect();
            if !stage_rows.is_empty() {
                block = block.child(
                    v_flex()
                        .ml(px(18.))
                        .mr_1()
                        .pl(px(6.))
                        .gap_0p5()
                        .border_l_2()
                        .border_color(accent.opacity(0.45))
                        .children(stage_rows),
                );
            }
        }
        block.into_any_element()
    }

    /// The ticket boxes under an orchestrator (core spec §46): a guide line down from it, a
    /// muted line saying how many it coordinates when more than one, then one box per ticket
    /// with its sessions inside.
    fn render_lanes(
        &self,
        orch: PaneId,
        lanes: Vec<super::orchestrator::Lane>,
        shown: Option<PaneId>,
        accent: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let count = lanes.len();
        let all_prs = lanes
            .iter()
            .all(|(r, _, _)| r.link.as_deref().is_some_and(|l| l.contains("/pull/")));
        let mut boxes = Vec::new();
        for (run, lane, panes) in &lanes {
            let rows: Vec<_> = panes
                .iter()
                .filter_map(|id| {
                    self.terminal_row(*id, shown == Some(*id), accent, Nest::Stage, cx)
                })
                .collect();
            boxes.push(self.render_lane(orch, run, lane, rows, cx));
        }
        v_flex()
            .ml(px(16.))
            .pl(px(8.))
            .pb_1()
            .border_l_2()
            .border_color(accent.opacity(0.45))
            .when(count > 1, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(crate::orchestrator_theme::color(
                            crate::orchestrator_theme::TEXT_MUTED,
                        ))
                        .child(format!(
                            "Coordinating {count} {}",
                            if all_prs { "pull requests" } else { "tickets" }
                        )),
                )
            })
            .children(boxes)
            .into_any_element()
    }

    /// Switches the list to the tab terminal `id` sits under once its environment shows, so a
    /// terminal shown from a toast or a link is in sight; whether the tab changed.
    pub(super) fn reveal_in_list(&mut self, id: PaneId, cx: &gpui::App) -> bool {
        let Some(env) = self.envs.env_of(id) else {
            return false;
        };
        // Only the selected environment's cards are known; another's waits until it is shown.
        if self.envs.active().is_none_or(|t| t.env != env) {
            return false;
        }
        let want = if self.pipeline_of(id, cx).is_some() {
            ListTab::Pipelines
        } else {
            ListTab::Sessions
        };
        let changed = self.list_tab != Some(want);
        self.list_tab = Some(want);
        changed
    }

    /// **Sessions · Pipelines**: the list's two tabs, each with how many it holds.
    fn render_list_tabs(
        &self,
        now: ListTab,
        sessions: usize,
        pipelines: usize,
        tint: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tab = |which: ListTab, label: &str, n: usize| {
            let on = which == now;
            h_flex()
                .id(SharedString::from(format!("list-tab-{label}")))
                .flex_1()
                .justify_center()
                .gap_1p5()
                .py_1()
                .rounded(px(6.))
                .text_sm()
                .cursor_pointer()
                .when(on, |d| {
                    d.bg(tint.opacity(0.22))
                        .font_weight(gpui::FontWeight::MEDIUM)
                })
                .when(!on, |d| {
                    d.text_color(cx.theme().muted_foreground)
                        .hover(|d| d.bg(tint.opacity(0.1)))
                })
                .child(label.to_string())
                .child(div().text_xs().opacity(0.7).child(n.to_string()))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    ws.list_tab = Some(which);
                    cx.notify();
                }))
        };
        h_flex()
            .mx_1()
            .mb_1()
            .p_0p5()
            .gap_0p5()
            .rounded(px(8.))
            .bg(cx.theme().muted.opacity(0.5))
            .child(tab(ListTab::Sessions, "Sessions", sessions))
            .child(tab(ListTab::Pipelines, "Pipelines", pipelines))
            .when(now == ListTab::Pipelines, |row| {
                let on = self.ui.pipelines_condensed;
                row.child(
                    Button::new("list-condensed")
                        .label("≡")
                        .ghost()
                        .xsmall()
                        .selected(on)
                        .tooltip(if on {
                            "Show full pipeline cards"
                        } else {
                            "Condensed: just what's running, one line each"
                        })
                        .on_click(cx.listener(|ws, _, window, cx| ws.toggle_condensed(window, cx))),
                )
            })
            .into_any_element()
    }

    /// Terminal `id`'s row: its activity dot, name, Claude's state and × on hover;
    /// `highlighted` when it is the one shown; `nest` sets how it sits in a pipeline's block.
    fn terminal_row(
        &self,
        id: PaneId,
        highlighted: bool,
        tint: gpui::Hsla,
        nest: Nest,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let muted = cx.theme().muted_foreground;
        let view = self.panes.get(&id)?;
        let v = view.read(cx);
        let (label, program) = (v.label(), v.program_title().map(str::to_string));
        let activity = self.activity.get(&id).copied();
        // A stage in progress breathes even while its session waits (on a deploy, a monitor).
        let at_work = nest == Nest::Stage && self.stage_at_work(id, cx);
        let dot = self.render_tab_dot(
            SharedString::from(format!("row-dot-{id}")),
            activity,
            at_work,
            tint,
            cx,
        );
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
                .pl(px(8.))
                .when(nest == Nest::Orchestrator, |d| d.mx_1())
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
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .map(|d| match nest {
                            // The orchestrator leads its block; its sessions read as under it.
                            Nest::Orchestrator => {
                                d.text_sm().font_weight(gpui::FontWeight::SEMIBOLD)
                            }
                            Nest::Stage => d
                                .text_size(px(13.))
                                .when(!highlighted, |d| d.text_color(muted)),
                            Nest::Plain => d.text_sm(),
                        })
                        .child(label),
                )
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

impl Workspace {
    /// The environment's set-up checklist (spec §48): each essential tool with where it stands
    /// and a button for its next step; nothing once they're all ready.
    fn render_setup_checklist(
        &self,
        env: &str,
        tools: &[String],
        reports: Option<&[hydra_core::provider::IdentityReport]>,
        tint: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let plugins: Vec<(String, setup::PluginState)> = self
            .plugin_states(env)
            .into_iter()
            .map(|(_, label, state)| (label, state))
            .collect();
        let items = setup::checklist(tools, reports, &plugins);
        if items.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (ok, muted, warn) = (theme.success, theme.muted_foreground, theme.warning);
        let hide_env = env.to_string();
        let mut block =
            v_flex()
                .mx_1()
                .my_1()
                .p_2()
                .gap_1()
                .rounded(px(8.))
                .border_1()
                .border_color(tint.opacity(0.45))
                .bg(tint.opacity(0.06))
                .child(
                    h_flex()
                        .justify_between()
                        .child(div().text_sm().child(format!("Set up {env}")))
                        .child(
                            Button::new("setup-hide")
                                .label("Hide")
                                .ghost()
                                .xsmall()
                                .tooltip("Hide this list for this environment")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.hide_setup(&hide_env, window, cx);
                                })),
                        ),
                )
                .child(div().text_xs().text_color(muted).whitespace_normal().child(
                    "Each environment has its own logins: add each tool here, then sign in.",
                ));
        for item in items {
            let (mark, mark_color) = match item.state {
                ToolState::Ready => ("✓", ok),
                ToolState::NotSignedIn => ("!", warn),
                ToolState::NotAdded => ("○", muted),
            };
            let mut row = h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(12.)).text_color(mark_color).child(mark))
                .child(div().text_sm().child(item.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(muted)
                        .child(setup::state_text(&item)),
                );
            if let Some(action) = setup::action_text(&item) {
                let (env, subject) = (env.to_string(), item.subject);
                let id = match subject {
                    setup::Subject::Tool(t) => format!("setup-{t}"),
                    setup::Subject::Plugin(i, _) => format!("setup-plugin-{i}"),
                };
                let mut button = Button::new(SharedString::from(id))
                    .label(action)
                    .outline()
                    .xsmall();
                if let setup::Subject::Plugin(..) = subject {
                    button = button
                        .tooltip("Installs it in your own Claude, so every environment has it");
                }
                row = row.child(button.on_click(cx.listener(
                    move |ws, _, window, cx| match subject {
                        setup::Subject::Tool(t) => {
                            ws.set_up_tool(env.clone(), t.to_string(), window, cx)
                        }
                        setup::Subject::Plugin(i, _) => ws.install_plugin(&env, i, window, cx),
                    },
                )));
            }
            block = block.child(row);
        }
        Some(block.into_any_element())
    }

    /// Each of config.toml's `setup_plugins` with its label and how it stands in `env`. One not
    /// read yet counts as on, so nothing flashes up as missing while the check runs.
    pub(super) fn plugin_states(&self, env: &str) -> Vec<(usize, String, setup::PluginState)> {
        self.ui
            .setup_plugins
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let state = self.plugins_on.get(env).and_then(|v| v.get(i).copied());
                (
                    i,
                    setup::plugin_label(p),
                    state.unwrap_or(setup::PluginState::On),
                )
            })
            .collect()
    }

    /// One line per team plugin, saying whether it is on in `env`, with Install or Reinstall
    /// when it isn't (spec §48). Shown on the Pipelines tab and the environment's Settings
    /// page, so it stays visible after the set-up checklist goes.
    pub(super) fn plugin_lines(
        &self,
        env: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = cx.theme();
        let (ok, muted, warn) = (theme.success, theme.muted_foreground, theme.warning);
        self.plugin_states(env)
            .into_iter()
            .map(|(i, label, state)| {
                let on = state == setup::PluginState::On;
                let plugin = self.ui.setup_plugins[i].plugin.clone();
                let mut row = h_flex()
                    .id(SharedString::from(format!("{id}-{i}")))
                    .gap_1p5()
                    .items_center()
                    .text_xs()
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(format!(
                            "Claude plugin {plugin}, from hydraui's config. Shared by every \
                             environment."
                        ))
                        .build(window, cx)
                    })
                    .child(div().text_color(if on { ok } else { warn }).child(if on {
                        "✓"
                    } else {
                        "!"
                    }))
                    .child(div().child(label))
                    .child(
                        div()
                            .flex_1()
                            .text_color(muted)
                            .child(setup::plugin_state_text(state)),
                    );
                if !on {
                    let env = env.to_string();
                    let action = if state == setup::PluginState::Missing {
                        "Reinstall"
                    } else {
                        "Install"
                    };
                    row = row.child(
                        Button::new(SharedString::from(format!("{id}-install-{i}")))
                            .label(action)
                            .outline()
                            .xsmall()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.install_plugin(&env, i, window, cx)
                            })),
                    );
                }
                row.into_any_element()
            })
            .collect()
    }

    /// The condensed Pipelines tab (spec §49): every open ticket in this environment's
    /// pipelines, one line each (its pipeline's colour, the ticket, its current stage, and its
    /// state when it isn't simply working). A click shows the current stage's session, else the
    /// orchestrator.
    fn render_condensed_pipelines(
        &self,
        cards: &[(PaneId, crate::pipeline_card::Card, Vec<PaneId>)],
        shown: Option<PaneId>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use crate::pipeline_card::{CardState, glance, ticket_title};
        let theme = cx.theme();
        let (muted, warn, hover) = (theme.muted_foreground, theme.warning, theme.muted);
        let mut rows = Vec::new();
        for (orch, card, _) in cards {
            for (run, _, panes) in self.open_lanes(*orch, cx) {
                let g = glance(&run);
                let target = g
                    .session
                    .as_deref()
                    .and_then(|t| {
                        self.panes
                            .iter()
                            .find(|(_, v)| v.read(cx).mcp_token() == Some(t))
                            .map(|(id, _)| *id)
                    })
                    .unwrap_or(*orch);
                let here = shown == Some(target) || panes.iter().any(|p| Some(*p) == shown);
                let accent = color(card.accent);
                let state = (g.state != CardState::Working).then(|| {
                    let tone = match g.state {
                        CardState::NeedsYou | CardState::Checking | CardState::Asking => warn,
                        _ => muted,
                    };
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(tone)
                        .child(g.state.word())
                });
                let pipeline = card.name.clone();
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("condensed-{}", run.id)))
                        .px_2()
                        .py_0p5()
                        .gap_2()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .when(here, |d| d.bg(accent.opacity(0.2)))
                        .hover(|d| d.bg(hover))
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(pipeline.clone())
                                .build(window, cx)
                        })
                        .child(div().flex_none().text_xs().text_color(accent).child("●"))
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(140.))
                                .truncate()
                                .text_sm()
                                .child(ticket_title(&run.ticket, run.link.as_deref())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(g.stage),
                        )
                        .children(state)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.show_terminal(target, window, cx)
                        }))
                        .into_any_element(),
                );
            }
        }
        if rows.is_empty() {
            return list_hint("Nothing running right now.").into_any_element();
        }
        v_flex()
            .px_1()
            .py_1()
            .gap_0p5()
            .children(rows)
            .into_any_element()
    }

    /// Switches the Pipelines tab between full cards and the condensed list, remembered in
    /// config.toml as `pipelines_condensed`.
    fn toggle_condensed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.pipelines_condensed = !self.ui.pipelines_condensed;
        let value = self.ui.pipelines_condensed.then_some(true.into());
        if let Err(e) = crate::config::set(&self.hydra.paths, "pipelines_condensed", value) {
            window.push_notification(
                gpui_component::notification::Notification::error(e).title("Pipelines"),
                cx,
            );
        }
        cx.notify();
    }

    /// Hides `env`'s set-up checklist, remembered in config.toml as `setup_hidden`.
    fn hide_setup(&mut self, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ui.setup_hidden.iter().any(|e| e == env) {
            self.ui.setup_hidden.push(env.to_string());
        }
        let value: toml_edit::Array = self.ui.setup_hidden.iter().map(String::as_str).collect();
        if let Err(e) = crate::config::set(&self.hydra.paths, "setup_hidden", Some(value.into())) {
            window.push_notification(
                gpui_component::notification::Notification::error(e).title("Set up"),
                cx,
            );
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
