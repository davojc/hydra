//! Tickets (core spec §53): the Linear tickets in the states the person chose, each with **Run
//! pipeline**, which hands that ticket to a pipeline's orchestrator (starting one when none is
//! open). Opened by the Tickets button between Projects and History.

use crate::inbox::{Note, PushNote};
use std::cell::Cell;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Subscription, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::combobox::{Combobox, ComboboxEvent, ComboboxState};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::linear::{self, Http, LinearError, Ticket};
use crate::orchestrator_theme::{self as theme, color};

pub struct TicketsTab {
    /// The environment whose Linear key and pipelines it uses.
    pub env: String,
    /// Every team the key sees, as (key, name); empty until read.
    pub teams: Vec<(String, String)>,
    /// The chosen teams' states (name, type, colour), in Linear's order; empty until read.
    pub states: Vec<linear::StateInfo>,
    /// The labels shown on a row when a ticket has them; the others aren't shown.
    pub pick_labels: Vec<String>,
    /// Every label the chosen teams can use, from Linear; empty when it couldn't be read.
    pub labels: Vec<String>,
    /// The filter's dropdowns, once made.
    filters: Option<Filters>,
    /// Counts reads of Linear: a reply to an older read than the latest is dropped.
    reading: u64,
    /// The filter being chosen; **Save filter** keeps it in config.toml.
    pub pick_teams: Vec<String>,
    pub pick_states: Vec<String>,
    /// Whose tickets: anyone's, unassigned ones, or ones assigned (to the people chosen).
    pub pick_who: linear::Assignee,
    /// The chosen teams' members, as (id, name), for Assigned.
    pub people: Vec<(String, String)>,
    pub tickets: Vec<Ticket>,
    /// Tickets with a run still in progress, upper-case.
    pub running: Vec<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// The team and status chips show; closed, one line names the filter. Opens closed on a
    /// saved filter.
    pub filter_open: bool,
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
        self.actions = None;
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
            pick_labels: self.ui.tickets_labels.clone(),
            labels: Vec::new(),
            filters: None,
            reading: 0,
            pick_who: who_from_config(&self.ui.tickets_assignee, &self.ui.tickets_people),
            people: Vec::new(),
            tickets: Vec::new(),
            running: Vec::new(),
            loading: true,
            error: None,
            filter_open: self.ui.tickets_teams.is_empty() || self.ui.tickets_states.is_empty(),
        });
        let filters = self.make_filters(window, cx);
        if let Some(t) = &mut self.tickets {
            t.filters = Some(filters);
        }
        self.reload_tickets(window, cx);
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
    fn reload_tickets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &mut self.tickets else {
            return;
        };
        t.loading = true;
        t.reading += 1;
        let reading = t.reading;
        let (env, teams, states) = (t.env.clone(), t.pick_teams.clone(), t.pick_states.clone());
        let who = t.pick_who.clone();
        let hydra = self.hydra.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    let http = Http {
                        key: hydra.linear_key(&env)?,
                    };
                    let all_teams = linear::read_teams(&http.post(&linear::teams_request())?)?;
                    let names = if teams.is_empty() {
                        Vec::new()
                    } else {
                        linear::read_team_states(&http.post(&linear::team_states_request(&teams))?)?
                    };
                    let people = if teams.is_empty() {
                        Vec::new()
                    } else {
                        linear::read_team_members(
                            &http.post(&linear::team_members_request(&teams))?,
                        )?
                    };
                    // The labels to offer; without them, those on the tickets listed will do.
                    let labels = if teams.is_empty() {
                        Vec::new()
                    } else {
                        http.post(&linear::team_labels_request(&teams))
                            .and_then(|r| linear::read_label_names(&r))
                            .unwrap_or_default()
                    };
                    // Only states the chosen teams have.
                    let states: Vec<String> = states
                        .into_iter()
                        .filter(|s| names.iter().any(|n| n.name == *s))
                        .collect();
                    let tickets = if teams.is_empty() || states.is_empty() {
                        Vec::new()
                    } else {
                        linear::read_pickable(
                            &http
                                .post(&linear::tickets_in_states_request(&teams, &states, &who))?,
                        )?
                    };
                    let running: Vec<String> = crate::run_history::read(hydra.paths.root(), &[])
                        .into_iter()
                        .filter(|e| e.outcome == crate::run_history::Outcome::InProgress)
                        .map(|e| e.ticket.to_ascii_uppercase())
                        .collect();
                    Ok::<_, LinearError>((
                        all_teams, names, states, people, labels, tickets, running,
                    ))
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                if let Some(t) = &mut ws.tickets {
                    // A newer read is on its way: this one is out of date.
                    if t.reading != reading {
                        return;
                    }
                    t.loading = false;
                    match read {
                        Ok((teams, names, states, people, labels, tickets, running)) => {
                            t.labels = labels;
                            t.teams = teams;
                            t.people = people;
                            t.states = names;
                            t.pick_states = states;
                            t.tickets = tickets;
                            t.running = running;
                            t.error = None;
                        }
                        Err(e) => t.error = Some(e.to_string()),
                    }
                    sync_filters(t, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The filter's dropdowns: teams, statuses, people and labels to pick several of, with a
    /// search; whose tickets to pick one of. A change to any but labels reads Linear again.
    fn make_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Filters {
        let multi = |window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                ComboboxState::new(
                    SearchableVec::new(Vec::<Opt>::new()),
                    Vec::new(),
                    window,
                    cx,
                )
                .multiple(true)
                .searchable(true)
            })
        };
        let (teams, states, people, labels) = (
            multi(window, cx),
            multi(window, cx),
            multi(window, cx),
            multi(window, cx),
        );
        let who_opts = vec![
            Opt::plain("anyone", "Anyone"),
            Opt::plain("unassigned", "Unassigned"),
            Opt::plain("assigned", "Assigned"),
        ];
        let mode = self
            .tickets
            .as_ref()
            .map_or("anyone".to_string(), |t| who_to_config(&t.pick_who).0);
        let who = cx.new(|cx| {
            ComboboxState::new(SearchableVec::new(who_opts), Vec::new(), window, cx)
                .multiple(false)
                .searchable(false)
        });
        who.update(cx, |b, cx| b.set_selected_values(&[mode], window, cx));
        type Ev = ComboboxEvent<SearchableVec<Opt>>;
        let changed = |e: &Ev| match e {
            ComboboxEvent::Change(v) => Some(v.clone()),
            ComboboxEvent::Confirm(_) => None,
        };
        let subs = vec![
            cx.subscribe_in(&teams, window, move |ws, _, e: &Ev, window, cx| {
                if let (Some(v), Some(t)) = (changed(e), &mut ws.tickets) {
                    t.pick_teams = v;
                    ws.reload_tickets(window, cx);
                }
            }),
            cx.subscribe_in(&states, window, move |ws, _, e: &Ev, window, cx| {
                if let (Some(v), Some(t)) = (changed(e), &mut ws.tickets) {
                    t.pick_states = v;
                    ws.reload_tickets(window, cx);
                }
            }),
            cx.subscribe_in(&who, window, move |ws, _, e: &Ev, window, cx| {
                if let (Some(v), Some(t)) = (changed(e), &mut ws.tickets) {
                    let people = match &t.pick_who {
                        linear::Assignee::Assigned(ids) => ids.clone(),
                        _ => Vec::new(),
                    };
                    let mode = v.first().map_or("anyone", String::as_str);
                    t.pick_who = who_from_config(mode, &people);
                    ws.reload_tickets(window, cx);
                }
            }),
            cx.subscribe_in(&people, window, move |ws, _, e: &Ev, window, cx| {
                if let (Some(v), Some(t)) = (changed(e), &mut ws.tickets)
                    && let linear::Assignee::Assigned(ids) = &mut t.pick_who
                {
                    *ids = v;
                    ws.reload_tickets(window, cx);
                }
            }),
            cx.subscribe_in(&labels, window, move |ws, _, e: &Ev, _, cx| {
                if let (Some(v), Some(t)) = (changed(e), &mut ws.tickets) {
                    t.pick_labels = v;
                    cx.notify();
                }
            }),
        ];
        Filters {
            teams,
            states,
            who,
            people,
            labels,
            offered: Default::default(),
            _subs: subs,
        }
    }

    /// **Save filter**: the chosen teams, states and assignees, kept in config.toml as
    /// `tickets_teams`, `tickets_states`, `tickets_assignee` and `tickets_people`, so the page
    /// opens on them next time.
    fn save_ticket_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &self.tickets else {
            return;
        };
        let (teams, states) = (t.pick_teams.clone(), t.pick_states.clone());
        let labels = t.pick_labels.clone();
        let (mode, people) = who_to_config(&t.pick_who);
        if let Err(e) = crate::config::set(
            &self.hydra.paths,
            "tickets_assignee",
            Some(mode.clone().into()),
        ) {
            window.push_note(Note::error(e).title("Tickets"), cx);
            return;
        }
        for (key, list) in [
            ("tickets_teams", &teams),
            ("tickets_states", &states),
            ("tickets_people", &people),
            ("tickets_labels", &labels),
        ] {
            let value: toml_edit::Array = list.iter().map(String::as_str).collect();
            if let Err(e) = crate::config::set(&self.hydra.paths, key, Some(value.into())) {
                window.push_note(Note::error(e).title("Tickets"), cx);
                return;
            }
        }
        self.ui.tickets_teams = teams;
        self.ui.tickets_states = states;
        self.ui.tickets_assignee = mode;
        self.ui.tickets_people = people;
        self.ui.tickets_labels = labels;
        // Saved: out of the way, so the tickets have the page.
        if let Some(t) = &mut self.tickets
            && !t.pick_teams.is_empty()
            && !t.pick_states.is_empty()
        {
            t.filter_open = false;
        }
        window.push_note(
            Note::info("Saved. Tickets opens on this filter next time.").title("Tickets"),
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
            window.push_note(
                Note::error(format!("Couldn't read the pipeline {pipeline}")).title("Run pipeline"),
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
        window.push_note(
            Note::info(format!(
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
        // Each filter is the same kind of dropdown, with its name above it.
        let field = |name: &'static str, body: AnyElement| {
            v_flex()
                .w(px(300.))
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT_MUTED))
                        .child(name),
                )
                .child(body)
        };
        let dropdown = |b: &Entity<ComboboxState<SearchableVec<Opt>>>,
                        placeholder: &'static str| {
            Combobox::new(b)
                .placeholder(placeholder)
                .search_placeholder("Search")
                .cleanable(true)
                .menu_max_h(px(340.))
                .into_any_element()
        };
        let assigned = matches!(t.pick_who, linear::Assignee::Assigned(_));
        let fields = t.filters.as_ref().map(|f| {
            h_flex()
                .gap_4()
                .flex_wrap()
                .items_start()
                .child(field("Teams", dropdown(&f.teams, "Choose teams")))
                .when(!t.pick_teams.is_empty(), |d| {
                    d.child(field("Statuses", dropdown(&f.states, "Choose statuses")))
                        .child(field(
                            "Assignee",
                            Combobox::new(&f.who)
                                .placeholder("Anyone")
                                .into_any_element(),
                        ))
                        .when(assigned, |d| {
                            d.child(field(
                                "Assigned to (none: anyone assigned)",
                                dropdown(&f.people, "Anyone assigned"),
                            ))
                        })
                        .child(field(
                            "Labels to show on rows",
                            dropdown(&f.labels, "Choose labels"),
                        ))
                })
        });
        let saved = t.pick_teams == self.ui.tickets_teams
            && t.pick_labels == self.ui.tickets_labels
            && t.pick_states == self.ui.tickets_states
            && t.pick_who == who_from_config(&self.ui.tickets_assignee, &self.ui.tickets_people);
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
            for state in t.states.iter().filter(|s| t.pick_states.contains(&s.name)) {
                let in_state: Vec<&Ticket> =
                    t.tickets.iter().filter(|k| k.state == state.name).collect();
                sections = sections.child(
                    h_flex()
                        .pt_3()
                        .pb_1()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(color(theme::BORDER))
                        .child(status_icon(state))
                        .child(
                            div()
                                .text_sm()
                                .text_color(color(theme::TEXT))
                                .child(state.name.clone()),
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
                    let shown: Vec<&String> = ticket
                        .labels
                        .iter()
                        .filter(|l| t.pick_labels.contains(l))
                        .collect();
                    sections =
                        sections.child(self.ticket_row(n, ticket, state, &shown, running, cx));
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
                            .on_click(
                                cx.listener(|ws, _, window, cx| ws.reload_tickets(window, cx)),
                            ),
                    ),
            );
        // The filter in one line, with Hide filter / Edit filter.
        let filter_open = t.filter_open;
        let filter_line = if t.pick_teams.is_empty() {
            "No teams chosen".to_string()
        } else if t.pick_states.is_empty() {
            format!("{} · no statuses chosen", t.pick_teams.join(", "))
        } else {
            format!(
                "{} · {}{}",
                t.pick_teams.join(", "),
                t.pick_states.join(", "),
                who_text(&t.pick_who, &t.people)
            )
        };
        let filter_bar = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("tickets-filter-toggle")
                    .label(if filter_open {
                        "▾ Hide filter"
                    } else {
                        "▸ Edit filter"
                    })
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(|ws, _, _, cx| {
                        if let Some(t) = &mut ws.tickets {
                            t.filter_open = !t.filter_open;
                        }
                        cx.notify();
                    })),
            )
            .when(!filter_open, |d| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(filter_line),
                )
            });
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
                    .child(filter_bar)
                    .when(t.filter_open, |d| d.children(fields))
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
        state: &linear::StateInfo,
        labels: &[&String],
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
            .child({
                let tip = state.name.clone();
                div()
                    .id(SharedString::from(format!("ticket-status-{n}")))
                    .child(status_icon(state))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
            })
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
            .children(labels.iter().map(|l| {
                div()
                    .flex_none()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .text_xs()
                    .bg(color(theme::AMBER_TINT))
                    .text_color(color(theme::AMBER))
                    .child((*l).clone())
            }))
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

/// The saved assignee filter: `tickets_assignee` is `anyone`, `unassigned` or `assigned`, with
/// `tickets_people` the user ids for `assigned`.
fn who_from_config(mode: &str, people: &[String]) -> linear::Assignee {
    match mode {
        "unassigned" => linear::Assignee::Unassigned,
        "assigned" => linear::Assignee::Assigned(people.to_vec()),
        _ => linear::Assignee::Anyone,
    }
}

fn who_to_config(who: &linear::Assignee) -> (String, Vec<String>) {
    match who {
        linear::Assignee::Anyone => ("anyone".into(), Vec::new()),
        linear::Assignee::Unassigned => ("unassigned".into(), Vec::new()),
        linear::Assignee::Assigned(ids) => ("assigned".into(), ids.clone()),
    }
}

/// The folded filter's words for `who`, naming the people from `people` (id, name).
fn who_text(who: &linear::Assignee, people: &[(String, String)]) -> String {
    match who {
        linear::Assignee::Anyone => String::new(),
        linear::Assignee::Unassigned => " · unassigned".into(),
        linear::Assignee::Assigned(ids) if ids.is_empty() => " · assigned".into(),
        linear::Assignee::Assigned(ids) => {
            let names: Vec<&str> = ids
                .iter()
                .map(|id| {
                    people
                        .iter()
                        .find(|(i, _)| i == id)
                        .map_or("someone", |(_, n)| n.as_str())
                })
                .collect();
            format!(" · assigned to {}", names.join(", "))
        }
    }
}

/// A status's icon as Linear draws it, by its type: a dotted ring for backlog, an empty ring to
/// do, a half-filled one in progress, a filled one done, a crossed one canceled.
fn status_glyph(kind: &str) -> &'static str {
    match kind {
        "triage" => "◇",
        "backlog" => "◌",
        "unstarted" => "○",
        "started" => "◐",
        "completed" => "●",
        "canceled" => "⊘",
        _ => "○",
    }
}

/// `#rrggbb` as a colour, when it is one.
fn hex_color(hex: &str) -> Option<u32> {
    let h = hex.strip_prefix('#')?;
    (h.len() == 6)
        .then(|| u32::from_str_radix(h, 16).ok())
        .flatten()
}

/// A status's icon in its own colour (muted when Linear gave none).
fn status_icon(state: &linear::StateInfo) -> gpui::Div {
    div()
        .flex_none()
        .w(px(14.))
        .text_sm()
        .text_color(color(hex_color(&state.color).unwrap_or(theme::TEXT_MUTED)))
        .child(status_glyph(&state.kind))
}

/// The labels to offer: `labels` (the teams'), those on `tickets`, and `chosen`, each once,
/// by name ignoring case.
fn label_choices(labels: &[String], tickets: &[Ticket], chosen: &[String]) -> Vec<String> {
    let mut all: Vec<String> = labels
        .iter()
        .cloned()
        .chain(tickets.iter().flat_map(|k| k.labels.iter().cloned()))
        .chain(chosen.iter().cloned())
        .collect();
    all.sort_by_key(|l| (l.to_lowercase(), l.clone()));
    all.dedup();
    all
}

/// One option in a filter dropdown: the value kept, what it shows, and (for a status) its icon.
#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    value: String,
    title: String,
    icon: Option<(&'static str, u32)>,
}

impl Opt {
    fn plain(value: &str, title: &str) -> Self {
        Self {
            value: value.to_string(),
            title: title.to_string(),
            icon: None,
        }
    }
}

impl SearchableListItem for Opt {
    type Value = String;

    fn title(&self) -> SharedString {
        self.title.clone().into()
    }

    fn value(&self) -> &String {
        &self.value
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        h_flex()
            .gap_2()
            .children(
                self.icon
                    .map(|(glyph, c)| div().w(px(14.)).text_color(color(c)).child(glyph)),
            )
            .child(self.title.clone())
    }
}

/// The filter's dropdowns, and the options each was last given.
pub struct Filters {
    teams: Entity<ComboboxState<SearchableVec<Opt>>>,
    states: Entity<ComboboxState<SearchableVec<Opt>>>,
    who: Entity<ComboboxState<SearchableVec<Opt>>>,
    people: Entity<ComboboxState<SearchableVec<Opt>>>,
    labels: Entity<ComboboxState<SearchableVec<Opt>>>,
    offered: [Vec<Opt>; 4],
    _subs: Vec<Subscription>,
}

/// Each dropdown's options from what was read: teams as `KEY · Name`, statuses with their
/// icon, people by name, labels (the teams', the tickets', the chosen ones).
fn filter_options(t: &TicketsTab) -> [Vec<Opt>; 4] {
    [
        t.teams
            .iter()
            .map(|(k, n)| Opt::plain(k, &format!("{k} · {n}")))
            .collect(),
        t.states
            .iter()
            .map(|st| Opt {
                value: st.name.clone(),
                title: st.name.clone(),
                icon: Some((
                    status_glyph(&st.kind),
                    hex_color(&st.color).unwrap_or(theme::TEXT_MUTED),
                )),
            })
            .collect(),
        t.people.iter().map(|(id, n)| Opt::plain(id, n)).collect(),
        label_choices(&t.labels, &t.tickets, &t.pick_labels)
            .iter()
            .map(|l| Opt::plain(l, l))
            .collect(),
    ]
}

/// Gives each dropdown its options when they changed, keeping what is chosen. One whose
/// options didn't change is left alone, so a search being typed in it isn't cleared.
fn sync_filters(t: &mut TicketsTab, window: &mut Window, cx: &mut Context<Workspace>) {
    let options = filter_options(t);
    let people = match &t.pick_who {
        linear::Assignee::Assigned(ids) => ids.clone(),
        _ => Vec::new(),
    };
    let chosen = [
        t.pick_teams.clone(),
        t.pick_states.clone(),
        people,
        t.pick_labels.clone(),
    ];
    let Some(f) = &mut t.filters else {
        return;
    };
    let boxes = [&f.teams, &f.states, &f.people, &f.labels];
    for (i, (b, opts)) in boxes.into_iter().zip(options).enumerate() {
        if f.offered[i] == opts {
            continue;
        }
        let pick = chosen[i].clone();
        b.update(cx, |b, cx| {
            b.set_items(SearchableVec::new(opts.clone()), window, cx);
            b.set_selected_values(&pick, window, cx);
        });
        f.offered[i] = opts;
    }
}

#[cfg(test)]
mod tests {
    use crate::linear::Assignee;

    #[test]
    fn the_assignee_filter_saves_and_reads_back_and_names_its_people() {
        for who in [
            Assignee::Anyone,
            Assignee::Unassigned,
            Assignee::Assigned(vec![]),
            Assignee::Assigned(vec!["u2".into()]),
        ] {
            let (mode, people) = super::who_to_config(&who);
            assert_eq!(super::who_from_config(&mode, &people), who);
        }
        assert_eq!(super::who_from_config("nonsense", &[]), Assignee::Anyone);
        let people = vec![("u2".to_string(), "Jordan Miller".to_string())];
        assert_eq!(
            super::who_text(&Assignee::Assigned(vec!["u2".into()]), &people),
            " · assigned to Jordan Miller"
        );
        assert_eq!(
            super::who_text(&Assignee::Unassigned, &people),
            " · unassigned"
        );
        assert_eq!(super::who_text(&Assignee::Anyone, &people), "");
    }

    #[test]
    fn each_dropdown_offers_what_was_read_teams_named_and_statuses_with_their_icon() {
        let t = super::TicketsTab {
            env: "iov42".into(),
            teams: vec![("PRE".into(), "Product & Engineering".into())],
            states: vec![crate::linear::StateInfo {
                name: "In Progress".into(),
                kind: "started".into(),
                color: "#f2c94c".into(),
            }],
            pick_labels: vec!["Old".into()],
            labels: vec!["Bug".into()],
            filters: None,
            reading: 0,
            pick_teams: vec![],
            pick_states: vec![],
            pick_who: Assignee::Anyone,
            people: vec![("u2".into(), "Jordan Miller".into())],
            tickets: vec![],
            running: vec![],
            loading: false,
            error: None,
            filter_open: true,
        };
        let [teams, states, people, labels] = super::filter_options(&t);
        assert_eq!(
            teams,
            vec![super::Opt::plain("PRE", "PRE · Product & Engineering")]
        );
        assert_eq!(states[0].value, "In Progress");
        assert_eq!(states[0].icon, Some(("◐", 0xf2c94c)));
        assert_eq!(people, vec![super::Opt::plain("u2", "Jordan Miller")]);
        let names: Vec<&str> = labels.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(names, vec!["Bug", "Old"]);
    }

    #[test]
    fn labels_offered_are_the_teams_the_tickets_and_the_chosen_ones_once_each() {
        let ticket = crate::linear::Ticket {
            id: "1".into(),
            identifier: "PRE-1".into(),
            title: String::new(),
            url: String::new(),
            sla_breaches_at: None,
            sla_high_risk_at: None,
            assignee: None,
            state: "Todo".into(),
            priority: 0,
            created_at: String::new(),
            labels: vec!["hydra-claimed".into(), "Bug".into()],
        };
        assert_eq!(
            super::label_choices(&["bug".into(), "Bug".into()], &[ticket], &["Old".into()]),
            vec!["Bug", "bug", "hydra-claimed", "Old"]
        );
    }

    #[test]
    fn a_status_is_drawn_by_its_type_in_its_colour() {
        assert_eq!(super::status_glyph("started"), "◐");
        assert_eq!(super::status_glyph("completed"), "●");
        assert_eq!(super::status_glyph("odd"), "○");
        assert_eq!(super::hex_color("#5e6ad2"), Some(0x5e6ad2));
        assert_eq!(super::hex_color("5e6ad2"), None);
        assert_eq!(super::hex_color("#5e6"), None);
    }
}
