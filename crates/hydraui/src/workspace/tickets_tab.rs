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
    /// Every state name Linear has, for the filter; empty until read.
    pub states: Vec<String>,
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
            states: Vec::new(),
            tickets: Vec::new(),
            running: Vec::new(),
            loading: true,
            error: None,
        });
        self.reload_tickets(true, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_tickets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tickets = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Reads the tickets in the chosen states (and Linear's state names, with `states`) in the
    /// background, with which tickets have a run in progress.
    fn reload_tickets(&mut self, states: bool, cx: &mut Context<Self>) {
        let Some(t) = &mut self.tickets else {
            return;
        };
        t.loading = true;
        let env = t.env.clone();
        let chosen = self.ui.tickets_states.clone();
        let hydra = self.hydra.clone();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    let http = Http {
                        key: hydra.linear_key(&env)?,
                    };
                    let names = if states {
                        Some(linear::read_state_names(
                            &http.post(&linear::state_names_request())?,
                        )?)
                    } else {
                        None
                    };
                    let tickets = if chosen.is_empty() {
                        Vec::new()
                    } else {
                        linear::read_pickable(
                            &http.post(&linear::tickets_in_states_request(&chosen))?,
                        )?
                    };
                    let running: Vec<String> = crate::run_history::read(hydra.paths.root(), &[])
                        .into_iter()
                        .filter(|e| e.outcome == crate::run_history::Outcome::InProgress)
                        .map(|e| e.ticket.to_ascii_uppercase())
                        .collect();
                    Ok::<_, LinearError>((names, tickets, running))
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(t) = &mut ws.tickets {
                    t.loading = false;
                    match read {
                        Ok((names, tickets, running)) => {
                            if let Some(n) = names {
                                t.states = n;
                            }
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

    /// Adds or removes state `name` from the filter, kept in config.toml as `tickets_states`.
    fn toggle_ticket_state(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let list = &mut self.ui.tickets_states;
        match list.iter().position(|s| *s == name) {
            Some(i) => {
                list.remove(i);
            }
            None => list.push(name),
        }
        let value: toml_edit::Array = list.iter().map(String::as_str).collect();
        if let Err(e) = crate::config::set(&self.hydra.paths, "tickets_states", Some(value.into()))
        {
            window.push_notification(Notification::error(e).title("Tickets"), cx);
        }
        self.reload_tickets(false, cx);
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
        let chosen = self.ui.tickets_states.clone();
        let chips = h_flex()
            .gap_1p5()
            .flex_wrap()
            .children(t.states.iter().enumerate().map(|(i, name)| {
                let on = chosen.contains(name);
                let n = name.clone();
                Button::new(SharedString::from(format!("tickets-state-{i}")))
                    .label(name.clone())
                    .small()
                    .outline()
                    .selected(on)
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.toggle_ticket_state(n.clone(), window, cx)
                    }))
            }));
        let summary = if let Some(e) = &t.error {
            format!("Couldn't read Linear for {}: {e}", t.env)
        } else if t.loading {
            "Reading Linear…".to_string()
        } else if chosen.is_empty() {
            "Pick the states to list.".to_string()
        } else {
            format!(
                "{} ticket{} in {} · Linear key of {}",
                t.tickets.len(),
                if t.tickets.len() == 1 { "" } else { "s" },
                chosen.join(", "),
                t.env
            )
        };
        let rows: Vec<AnyElement> = t
            .tickets
            .iter()
            .enumerate()
            .map(|(n, ticket)| {
                let running = t.running.contains(&ticket.identifier.to_ascii_uppercase());
                self.ticket_row(n, ticket, running, cx)
            })
            .collect();
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
                Button::new("tickets-refresh")
                    .label("Refresh")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|ws, _, _, cx| ws.reload_tickets(true, cx))),
            );
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
                    .gap_3()
                    .child(header)
                    .child(chips)
                    .child(v_flex().gap_0p5().children(rows)),
            )
            .into_any_element()
    }

    /// One ticket: its key, title, state and labels; **Run pipeline**, or Running while a run
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
            .child(
                div()
                    .w(px(100.))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(ticket.state.clone()),
            )
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
