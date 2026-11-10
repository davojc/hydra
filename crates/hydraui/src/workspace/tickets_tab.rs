//! Tickets (core spec §53): the Linear tickets in the states the person chose, each with **Run
//! pipeline**, which hands that ticket to a pipeline's orchestrator (starting one when none is
//! open). Opened by the Tickets button between Projects and History.

use std::cell::Cell;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::notification::Notification;
use gpui_component::{Selectable, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::linear::{self, Http, LinearError, Ticket};
use crate::orchestrator_theme::{self as theme, color};

pub struct TicketsTab {
    /// The environment whose Linear key and pipelines it uses.
    pub env: String,
    /// Every team the key sees, as (key, name); empty until read.
    pub teams: Vec<(String, String)>,
    /// The chosen teams' state names, in Linear's order; empty until read.
    pub states: Vec<String>,
    /// The filter being chosen; **Save filter** keeps it in config.toml.
    pub pick_teams: Vec<String>,
    pub pick_states: Vec<String>,
    pub tickets: Vec<Ticket>,
    /// Tickets with a run still in progress, upper-case.
    pub running: Vec<String>,
    pub loading: bool,
    pub error: Option<String>,
}

impl Workspace {
    pub(super) fn tickets_shown(&self) -> bool {
        self.tickets.is_some()
    }

    /// The Tickets button, between Projects and History.
    pub(super) fn tickets_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.tickets.is_some();
        h_flex()
            .id("open-tickets")
            .flex_none()
            .gap_2()
            .items_center()
            .h(px(26.))
            .px_3()
            .mr_1()
            .rounded_md()
            .border_1()
            .text_sm()
            .cursor_pointer()
            .border_color(color(if open {
                theme::TEAL
            } else {
                theme::BORDER_STRONG
            }))
            .bg(color(if open {
                theme::TEAL_TINT
            } else {
                theme::RAISED
            }))
            .text_color(color(if open { theme::TEAL } else { theme::TEXT }))
            .hover(|s| s.border_color(color(theme::TEAL)))
            .child("Tickets")
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("Linear tickets to hand to a pipeline")
                    .build(window, cx)
            })
            .on_click(cx.listener(|ws, _, window, cx| {
                if ws.tickets_shown() {
                    ws.close_tickets(window, cx)
                } else {
                    ws.open_tickets(window, cx)
                }
            }))
            .into_any_element()
    }

    /// Opens Tickets for the environment shown, else the first with a Linear key set up.
    pub(super) fn open_tickets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.projects = None;
        self.history = None;
        let shown = self.envs.active().map(|t| t.env.clone());
        let env = shown
            .clone()
            .filter(|e| self.hydra.linear_key(e).is_ok())
            .or_else(|| {
                self.hydra
                    .env_names()
                    .into_iter()
                    .find(|e| self.hydra.linear_key(e).is_ok())
            })
            .or(shown)
            .unwrap_or_default();
        self.tickets = Some(TicketsTab {
            env,
            teams: Vec::new(),
            states: Vec::new(),
            pick_teams: self.ui.tickets_teams.clone(),
            pick_states: self.ui.tickets_states.clone(),
            tickets: Vec::new(),
            running: Vec::new(),
            loading: true,
            error: None,
        });
        self.reload_tickets(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_tickets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tickets = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Reads, in the background, Linear's teams; the chosen teams' states; and, once both are
    /// chosen, their tickets in those states, with which tickets have a run in progress.
    fn reload_tickets(&mut self, cx: &mut Context<Self>) {
        let Some(t) = &mut self.tickets else {
            return;
        };
        t.loading = true;
        let (env, teams, states) = (t.env.clone(), t.pick_teams.clone(), t.pick_states.clone());
        let hydra = self.hydra.clone();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    let http = Http {
                        key: hydra.linear_key(&env)?,
                    };
                    let all_teams = linear::read_teams(&http.post(&linear::teams_request())?)?;
                    let names = if teams.is_empty() {
                        Vec::new()
                    } else {
                        linear::read_team_state_names(
                            &http.post(&linear::team_states_request(&teams))?,
                        )?
                    };
                    // Only states the chosen teams have.
                    let states: Vec<String> =
                        states.into_iter().filter(|s| names.contains(s)).collect();
                    let tickets = if teams.is_empty() || states.is_empty() {
                        Vec::new()
                    } else {
                        linear::read_pickable(
                            &http.post(&linear::tickets_in_states_request(&teams, &states))?,
                        )?
                    };
                    let running: Vec<String> = crate::run_history::read(hydra.paths.root(), &[])
                        .into_iter()
                        .filter(|e| e.outcome == crate::run_history::Outcome::InProgress)
                        .map(|e| e.ticket.to_ascii_uppercase())
                        .collect();
                    Ok::<_, LinearError>((all_teams, names, states, tickets, running))
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(t) = &mut ws.tickets {
                    t.loading = false;
                    match read {
                        Ok((teams, names, states, tickets, running)) => {
                            t.teams = teams;
                            t.states = names;
                            t.pick_states = states;
                            t.tickets = tickets;
                            t.running = running;
                            t.error = None;
                        }
                        Err(e) => t.error = Some(e.to_string()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Adds or removes team `key` from the filter being chosen.
    fn toggle_ticket_team(&mut self, key: String, cx: &mut Context<Self>) {
        if let Some(t) = &mut self.tickets {
            toggle(&mut t.pick_teams, key);
        }
        self.reload_tickets(cx);
    }

    /// Adds or removes state `name` from the filter being chosen.
    fn toggle_ticket_state(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(t) = &mut self.tickets {
            toggle(&mut t.pick_states, name);
        }
        self.reload_tickets(cx);
    }

    /// **Save filter**: the chosen teams and states, kept in config.toml as `tickets_teams` and
    /// `tickets_states`, so the page opens on them next time.
    fn save_ticket_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &self.tickets else {
            return;
        };
        let (teams, states) = (t.pick_teams.clone(), t.pick_states.clone());
        for (key, list) in [("tickets_teams", &teams), ("tickets_states", &states)] {
            let value: toml_edit::Array = list.iter().map(String::as_str).collect();
            if let Err(e) = crate::config::set(&self.hydra.paths, key, Some(value.into())) {
                window.push_notification(Notification::error(e).title("Tickets"), cx);
                return;
            }
        }
        self.ui.tickets_teams = teams;
        self.ui.tickets_states = states;
        window.push_notification(
            Notification::info("Saved. Tickets opens on this filter next time.").title("Tickets"),
            cx,
        );
        cx.notify();
    }

    /// **Run pipeline** on `ticket`: picks the pipeline, then hands the ticket over.
    fn run_pipeline_dialog(&mut self, ticket: Ticket, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.tickets.as_ref().map(|t| t.env.clone()) else {
            return;
        };
        let root = self.hydra.paths.root().to_path_buf();
        let names = crate::pipeline::names(&root);
        let labels = super::launcher::pipeline_labels(&root, &names);
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        let title = format!("Run {} through", ticket.identifier);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (names, workspace, env, ticket) = (
                names.clone(),
                workspace.clone(),
                env.clone(),
                ticket.clone(),
            );
            dialog.title(title.clone()).child(
                Command::new(&state)
                    .items(labels.iter().map(|l| CommandItem::new().label(l.clone())))
                    .placeholder("Pipeline")
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        if let Some(name) = names.get(ix.row).cloned() {
                            let (env, ticket) = (env.clone(), ticket.clone());
                            workspace.update(cx, |ws, cx| {
                                ws.run_ticket(&env, &name, ticket, window, cx)
                            });
                        }
                    }),
            )
        });
    }

    /// Hands `ticket` to pipeline `pipeline` in `env`: to an orchestrator of it with room for
    /// another ticket, else a new one, which gets it once Claude is ready.
    pub(super) fn run_ticket(
        &mut self,
        env: &str,
        pipeline: &str,
        ticket: Ticket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(p) = crate::pipeline::load(self.hydra.paths.root(), pipeline) else {
            window.push_notification(
                Notification::error(format!("Couldn't read the pipeline {pipeline}"))
                    .title("Run pipeline"),
                cx,
            );
            return;
        };
        let open = self
            .orchestrators
            .iter()
            .filter(|(id, f)| {
                *f == pipeline && self.panes.get(id).is_some_and(|v| v.read(cx).env() == env)
            })
            .map(|(id, _)| *id)
            .min();
        let slot = open
            .and_then(|o| self.slot_for_work(o, window, cx))
            .unwrap_or_else(|| self.open_orchestrator_slot(env, pipeline, window, cx));
        self.linear_tickets
            .insert(ticket.identifier.to_ascii_uppercase(), ticket.clone());
        let prompt = crate::linear_watch::start_prompt(&p, &ticket);
        self.pending_wakes
            .insert(slot, (ticket.identifier.clone(), prompt));
        self.send_wakes(window, cx);
        if let Some(t) = &mut self.tickets {
            t.running.push(ticket.identifier.to_ascii_uppercase());
        }
        crate::action_log::write(format!(
            "tickets: {} handed to {pipeline} in {env}",
            ticket.identifier
        ));
        window.push_notification(
            Notification::info(format!(
                "{} goes to {}. Its card shows on the Pipelines tab.",
                ticket.identifier, p.name
            ))
            .title("Run pipeline"),
            cx,
        );
        cx.notify();
    }

    pub(super) fn render_tickets_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(t) = &self.tickets else {
            return div().into_any_element();
        };
        let chip = |id: String, label: String, on: bool| {
            Button::new(SharedString::from(id))
                .label(label)
                .small()
                .outline()
                .selected(on)
        };
        let team_chips =
            h_flex()
                .gap_1p5()
                .flex_wrap()
                .children(t.teams.iter().map(|(key, name)| {
                    let k = key.clone();
                    chip(
                        format!("tickets-team-{key}"),
                        format!("{key} · {name}"),
                        t.pick_teams.contains(key),
                    )
                    .on_click(cx.listener(move |ws, _, _, cx| ws.toggle_ticket_team(k.clone(), cx)))
                }));
        let state_chips = h_flex()
            .gap_1p5()
            .flex_wrap()
            .children(t.states.iter().enumerate().map(|(i, name)| {
                let n = name.clone();
                chip(
                    format!("tickets-state-{i}"),
                    name.clone(),
                    t.pick_states.contains(name),
                )
                .on_click(cx.listener(move |ws, _, _, cx| ws.toggle_ticket_state(n.clone(), cx)))
            }));
        let saved =
            t.pick_teams == self.ui.tickets_teams && t.pick_states == self.ui.tickets_states;
        let summary = if let Some(e) = &t.error {
            format!("Couldn't read Linear for {}: {e}", t.env)
        } else if t.loading {
            "Reading Linear…".to_string()
        } else if t.pick_teams.is_empty() {
            "Choose the teams (boards) to list tickets from.".to_string()
        } else if t.pick_states.is_empty() {
            "Choose the statuses to list.".to_string()
        } else {
            format!(
                "{} ticket{} · Linear key of {}",
                t.tickets.len(),
                if t.tickets.len() == 1 { "" } else { "s" },
                t.env
            )
        };
        // One section per chosen status, in Linear's order, each headed with its count.
        let mut sections = v_flex().gap_1();
        if !t.pick_teams.is_empty() {
            let mut n = 0;
            for state in t.states.iter().filter(|s| t.pick_states.contains(s)) {
                let in_state: Vec<&Ticket> =
                    t.tickets.iter().filter(|k| &k.state == state).collect();
                sections = sections.child(
                    h_flex()
                        .pt_3()
                        .pb_1()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(color(theme::BORDER))
                        .child(
                            div()
                                .text_sm()
                                .text_color(color(theme::TEXT))
                                .child(state.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(color(theme::TEXT_MUTED))
                                .child(in_state.len().to_string()),
                        ),
                );
                if in_state.is_empty() && !t.loading {
                    sections = sections.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(color(theme::TEXT_MUTED))
                            .child("None"),
                    );
                }
                for ticket in in_state {
                    let running = t.running.contains(&ticket.identifier.to_ascii_uppercase());
                    sections = sections.child(self.ticket_row(n, ticket, running, cx));
                    n += 1;
                }
            }
        }
        let header = h_flex()
            .justify_between()
            .items_center()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xl()
                            .text_color(color(theme::TEXT))
                            .child("Tickets"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(if t.error.is_some() {
                                theme::RED
                            } else {
                                theme::TEXT_SECONDARY
                            }))
                            .child(summary),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("tickets-save")
                            .label(if saved { "Filter saved" } else { "Save filter" })
                            .small()
                            .when(!saved, |b| b.primary())
                            .when(saved, |b| b.ghost())
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                if !saved {
                                    ws.save_ticket_filter(window, cx)
                                }
                            })),
                    )
                    .child(
                        Button::new("tickets-refresh")
                            .label("Refresh")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|ws, _, _, cx| ws.reload_tickets(cx))),
                    ),
            );
        let label = |text: &'static str| {
            div()
                .pt_1()
                .text_xs()
                .text_color(color(theme::TEXT_MUTED))
                .child(text)
        };
        div()
            .id("tickets-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_5()
            .child(
                v_flex()
                    .max_w(px(1080.))
                    .gap_2()
                    .child(header)
                    .child(label("Teams"))
                    .child(team_chips)
                    .when(!t.pick_teams.is_empty(), |d| {
                        d.child(label("Statuses")).child(state_chips)
                    })
                    .child(sections),
            )
            .into_any_element()
    }

    /// One ticket under its status heading: its key, title and labels; **Run pipeline**, or Running while a run
    /// of it is in progress. A click on the key opens it in Linear.
    fn ticket_row(
        &self,
        n: usize,
        ticket: &Ticket,
        running: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let url = ticket.url.clone();
        let t = ticket.clone();
        h_flex()
            .id(SharedString::from(format!("ticket-row-{n}")))
            .gap_3()
            .px_2()
            .py_1p5()
            .items_center()
            .rounded_md()
            .hover(|d| d.bg(color(theme::RAISED)))
            .child(
                div()
                    .id(SharedString::from(format!("ticket-key-{n}")))
                    .w(px(96.))
                    .flex_none()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(color(theme::TEAL))
                    .child(ticket.identifier.clone())
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(color(theme::TEXT))
                    .child(ticket.title.clone()),
            )
            .when(!ticket.labels.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .max_w(px(200.))
                        .truncate()
                        .text_xs()
                        .text_color(color(theme::TEXT_MUTED))
                        .child(ticket.labels.join(" · ")),
                )
            })
            .child(if running {
                div()
                    .w(px(110.))
                    .flex_none()
                    .text_xs()
                    .text_color(color(theme::AMBER))
                    .child("Running")
                    .into_any_element()
            } else {
                Button::new(SharedString::from(format!("ticket-run-{n}")))
                    .label("Run pipeline")
                    .small()
                    .outline()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.run_pipeline_dialog(t.clone(), window, cx)
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }
}

/// Adds `item` to `list`, or takes it out when it is there.
fn toggle(list: &mut Vec<String>, item: String) {
    match list.iter().position(|x| *x == item) {
        Some(i) => {
            list.remove(i);
        }
        None => list.push(item),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_chip_toggles_its_item_in_and_out() {
        let mut list = vec!["PRE".to_string()];
        super::toggle(&mut list, "INTH".into());
        assert_eq!(list, vec!["PRE", "INTH"]);
        super::toggle(&mut list, "PRE".into());
        assert_eq!(list, vec!["INTH"]);
    }
}
