//! The orchestrator's Linear watcher in the app (phase 2 spec §3, §4): the card's
//! Automatic/Manual switch, the checks it makes, and the writes back when a run starts or ends. The decisions
//! are `linear_watch`'s; the calls are `linear::Http`'s, made off the UI thread.

use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::switch::Switch;
use gpui_component::{Sizable, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::linear::{Http, LinearError};
use crate::linear_watch::{self, Failures, SlaLevel};
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::pipeline::{LinearSettings, Pipeline};
use crate::runs::{Change, Decided};

/// One orchestrator's watcher.
#[derive(Debug, Default)]
pub(super) struct Watch {
    pub on: bool,
    /// When it last looked.
    pub last: Option<SystemTime>,
    pub failures: Failures,
    /// A check is out.
    pub checking: bool,
    /// What the last check found, for the card.
    pub status: Option<String>,
    /// Why it switched itself off, until a person switches it again.
    pub stopped: Option<String>,
    /// The check out was asked for with **Pick next ticket**, so its result is announced.
    pub by_hand: bool,
    /// A `[slack]` pipeline's pull request the orchestrator was told to start, and when: none
    /// other starts until its run opens.
    pub starting: Option<(String, SystemTime)>,
}

/// The Linear side of a run that started (`start`) or ended, off the UI thread: what was
/// written, and the ticket as Linear has it now when the run started.
fn linear_job(
    hydra: &crate::hydra::Hydra,
    env: &str,
    settings: &LinearSettings,
    run: &crate::runs::Run,
    start: bool,
) -> Result<(Vec<&'static str>, Option<crate::linear::Ticket>), LinearError> {
    use crate::linear::LinearApi;
    let api = Http {
        key: hydra.linear_key(env)?,
    };
    if start {
        let ticket = api.ticket(&run.ticket)?;
        let claimed = linear_watch::claim_named(&api, settings, &run.pipeline, &run.ticket)?;
        Ok((if claimed { vec!["claim"] } else { Vec::new() }, ticket))
    } else {
        Ok((linear_watch::end_writes(&api, settings, run)?, None))
    }
}

impl Workspace {
    /// `orch`'s pipeline with its `[linear]` table, and its environment; `None` without one.
    fn linear_of(
        &self,
        orch: PaneId,
        cx: &gpui::App,
    ) -> Option<(Pipeline, LinearSettings, String)> {
        let (file, env, _) = self.settings_target(orch, cx)?;
        let pipeline = crate::pipeline::load(self.hydra.paths.root(), &file).ok()?;
        let settings = pipeline.linear.clone()?;
        Some((pipeline, settings, env))
    }

    /// Whether `orch` has a run open (one ticket at a time).
    pub(super) fn has_open_run(&self, orch: PaneId, cx: &gpui::App) -> bool {
        let token = self
            .panes
            .get(&orch)
            .and_then(|v| v.read(cx).mcp_token().map(str::to_string));
        match (token, server::get()) {
            (Some(token), Some(sv)) => server::lock(sv.registry())
                .runs
                .current(&token)
                .is_some_and(|r| r.state.open()),
            _ => false,
        }
    }

    /// Whether `orch`'s run waits for product's answer to a stage's question.
    fn run_asking(&self, orch: PaneId, cx: &gpui::App) -> bool {
        let Some(token) = self
            .panes
            .get(&orch)
            .and_then(|v| v.read(cx).mcp_token().map(str::to_string))
        else {
            return false;
        };
        server::get().is_some_and(|sv| {
            server::lock(sv.registry())
                .runs
                .current(&token)
                .is_some_and(|r| r.asking.is_some())
        })
    }

    /// The ticket of `orch`'s run in progress.
    fn open_run_ticket(&self, orch: PaneId, cx: &gpui::App) -> Option<String> {
        let token = self.panes.get(&orch)?.read(cx).mcp_token()?.to_string();
        let sv = server::get()?;
        let reg = server::lock(sv.registry());
        reg.runs
            .current(&token)
            .filter(|r| r.state.open())
            .map(|r| r.ticket.clone())
    }

    /// **Pick tickets automatically**: on looks at once; only one orchestrator may watch a team
    /// (§3). Either way the choice is logged, so a card that changed mode can be traced.
    pub(super) fn toggle_linear_watch(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on = self.linear_watch.get(&orch).is_some_and(|w| w.on);
        let file = self.orchestrators.get(&orch).cloned().unwrap_or_default();
        if on {
            if let Some(w) = self.linear_watch.get_mut(&orch) {
                w.on = false;
                w.status = None;
                w.stopped = None;
            }
            crate::action_log::write(format!("linear: {file}: switched to manual"));
            cx.notify();
            return;
        }
        // What it watches, a Linear team or a Slack channel, and who else watches the same.
        let (watched, label) = if let Some((_, s, _)) = self.linear_of(orch, cx) {
            (s.team.clone(), s.team)
        } else if let Some((_, s, _)) = self.slack_of(orch, cx) {
            (s.channel.clone(), s.label)
        } else {
            return;
        };
        let others: Vec<(PaneId, String)> = self
            .linear_watch
            .iter()
            .filter(|(id, w)| w.on && self.panes.contains_key(id))
            .filter_map(|(id, _)| {
                let what = match self.linear_of(*id, cx) {
                    Some((_, s, _)) => s.team,
                    None => self.slack_of(*id, cx)?.1.channel,
                };
                Some((*id, what))
            })
            .collect();
        let other = linear_watch::watched_by(
            &watched,
            &orch,
            others.iter().map(|(id, t)| (id, t.as_str())),
        );
        if other.is_some() {
            window.push_notification(
                Notification::warning(format!(
                    "Another orchestrator already watches {label}: only one may, or they'd race \
                     for its work."
                ))
                .title("Automatic"),
                cx,
            );
            return;
        }
        let w = self.linear_watch.entry(orch).or_default();
        w.on = true;
        w.last = None;
        w.failures.clear();
        w.status = None;
        w.stopped = None;
        crate::action_log::write(format!("linear: {file}: switched to automatic"));
        self.linear_tick(window, cx);
    }

    /// **Pick now**: one pick, whatever the mode.
    pub(super) fn check_linear_now(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_open_run(orch, cx) {
            window.push_notification(
                Notification::info("A ticket is running: the next is picked when it ends.")
                    .title("Check Linear"),
                cx,
            );
            return;
        }
        if self.linear_watch.get(&orch).is_some_and(|w| w.checking) {
            return;
        }
        // Pull requests already taken go first.
        if let Some((p, s, _)) = self.slack_of(orch, cx)
            && !self.slack_queue(&p.file).is_empty()
        {
            return self.start_next_pr(orch, &p, &s, window, cx);
        }
        self.start_linear_check(orch, window, cx);
        if let Some(w) = self.linear_watch.get_mut(&orch) {
            w.by_hand = true;
        }
    }

    /// Each second: a watcher whose check is due starts one (§3).
    pub(super) fn linear_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = SystemTime::now();
        // A Slack watcher starts what it took before it looks again.
        let slack: Vec<PaneId> = self
            .linear_watch
            .iter()
            .filter(|(id, w)| w.on && !w.checking && self.panes.contains_key(id))
            .map(|(id, _)| *id)
            .collect();
        let mut held: Vec<PaneId> = Vec::new();
        for orch in slack {
            if self.slack_of(orch, cx).is_some() && !self.slack_tick(orch, window, cx) {
                held.push(orch);
            }
        }
        let due: Vec<PaneId> = self
            .linear_watch
            .iter()
            .filter(|(id, w)| w.on && !w.checking && self.panes.contains_key(id))
            .map(|(id, _)| *id)
            .filter(|id| !held.contains(id))
            .filter(|id| {
                let every = match self.linear_of(*id, cx) {
                    Some((_, s, _)) => s.every_minutes,
                    None => self
                        .slack_of(*id, cx)
                        .map_or(5, |(_, s, _)| s.every_minutes),
                };
                let w = &self.linear_watch[id];
                linear_watch::due(
                    w.on,
                    self.has_open_run(*id, cx),
                    w.last,
                    now,
                    Duration::from_secs(u64::from(every) * 60),
                )
            })
            .collect();
        for orch in due {
            self.start_linear_check(orch, window, cx);
        }
    }

    /// A check, off the UI thread: the environment's key, then find and claim a ticket; the
    /// orchestrator is told to start it (§3, §4).
    fn start_linear_check(&mut self, orch: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.slack_of(orch, cx).is_some() {
            return self.start_slack_check(orch, window, cx);
        }
        let Some((pipeline, settings, env)) = self.linear_of(orch, cx) else {
            return;
        };
        let w = self.linear_watch.entry(orch).or_default();
        w.checking = true;
        w.last = Some(SystemTime::now());
        crate::action_log::write(format!(
            "linear: {}: checking {}",
            pipeline.file, settings.team
        ));
        let hydra = self.hydra.clone();
        let (p, s) = (pipeline.clone(), settings.clone());
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let key = hydra.linear_key(&env)?;
                    // Every ticket a run log names: hydraui ran it before (§3).
                    let ran: Vec<String> = crate::run_history::read(hydra.paths.root(), &[])
                        .into_iter()
                        .map(|e| e.ticket)
                        .collect();
                    linear_watch::find_and_claim(&Http { key }, &s, &p, &ran)
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.linear_checked(orch, &pipeline, found, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn linear_checked(
        &mut self,
        orch: PaneId,
        pipeline: &Pipeline,
        found: Result<linear_watch::Check, LinearError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.linear_watch.get_mut(&orch) else {
            return;
        };
        w.checking = false;
        if let (true, Some(settings)) = (std::mem::take(&mut w.by_hand), &pipeline.linear) {
            let (ok, text) = linear_watch::pick_result(settings, &found);
            let note = if ok {
                Notification::info(text)
            } else {
                Notification::error(text)
            };
            window.push_notification(note.title(pipeline.name.clone()), cx);
        }
        let skipped = |check: &linear_watch::Check| {
            if check.skipped.is_empty() {
                String::new()
            } else {
                format!(" · skipped {} (run here before)", check.skipped.join(", "))
            }
        };
        match found {
            Ok(check) if check.claimed.is_some() => {
                let also = skipped(&check);
                let ticket = check.claimed.expect("matched Some");
                w.failures.clear();
                w.status = Some(format!("Picked {}", ticket.identifier));
                self.linear_tickets
                    .insert(ticket.identifier.to_ascii_uppercase(), ticket.clone());
                crate::action_log::write(format!(
                    "linear: claimed {} for the {} orchestrator{also}",
                    ticket.identifier, pipeline.file
                ));
                let prompt = linear_watch::start_prompt(pipeline, &ticket);
                self.pending_wakes
                    .insert(orch, (ticket.identifier.clone(), prompt));
                self.send_wakes(window, cx);
            }
            Ok(check) => {
                w.failures.clear();
                w.status = Some("Nothing new".to_string());
                crate::action_log::write(format!(
                    "linear: {}: nothing to pick{}",
                    pipeline.file,
                    skipped(&check)
                ));
            }
            // A pick by hand that failed says so itself and doesn't count against Automatic.
            Err(e) if !w.on => crate::action_log::write(format!("linear: {e}")),
            Err(e) => {
                crate::action_log::write(format!("linear: {e}"));
                match w.failures.record(&e) {
                    Some(reason) => {
                        w.on = false;
                        w.status = None;
                        w.stopped = Some(reason.clone());
                        window.push_notification(
                            Notification::error(format!("Stopped watching Linear: {reason}"))
                                .title(pipeline.name.clone()),
                            cx,
                        );
                    }
                    // The reason is in the log; the card has room for the fact.
                    None => w.status = Some("Last look failed".to_string()),
                }
            }
        }
        cx.notify();
    }

    /// Writes back for runs that started or ended (§4): a hand-started INTH run is claimed, an
    /// ended one gets its comment (and the passed state). Each write is noted in the run's log.
    pub(super) fn linear_runs_changed(&mut self, changes: &[Change], cx: &mut Context<Self>) {
        let Some(sv) = server::get() else {
            return;
        };
        let jobs: Vec<(crate::runs::Run, bool)> = {
            let reg = server::lock(sv.registry());
            changes
                .iter()
                .filter_map(|c| {
                    let start = match c.what {
                        Decided::RunStarted => true,
                        Decided::RunFinished | Decided::RunStopped(_) => false,
                        _ => return None,
                    };
                    let run = reg.runs.run(&c.run)?;
                    let team = &run.pipeline.linear.as_ref()?.team;
                    linear_watch::in_team(&run.ticket, team).then(|| (run.clone(), start))
                })
                .collect()
        };
        for (run, start) in jobs {
            // A watcher whose run ended looks for the next ticket at once (§3).
            if !start {
                let orch = self
                    .panes
                    .iter()
                    .find(|(_, v)| v.read(cx).mcp_token() == Some(run.orchestrator.as_str()))
                    .map(|(id, _)| *id);
                if let Some(w) = orch.and_then(|o| self.linear_watch.get_mut(&o)) {
                    w.last = None;
                }
            }
            let env = self.panes.values().find_map(|v| {
                let v = v.read(cx);
                (v.mcp_token() == Some(run.orchestrator.as_str())).then(|| v.request.env.clone())
            });
            let (Some(env), Some(settings)) = (env, run.pipeline.linear.clone()) else {
                continue;
            };
            // Whether it ended inside its SLA, from the ticket as it was when the run started.
            if !start
                && let Some(breaches) = self
                    .linear_tickets
                    .get(&run.ticket.to_ascii_uppercase())
                    .and_then(|t| t.sla_breaches_at.clone())
                && let Some(inside) = linear_watch::inside_sla(&breaches, SystemTime::now())
            {
                super::orchestrator::note_run(
                    &run.id,
                    "linear sla",
                    serde_json::json!({"inside": inside, "breaches_at": breaches}),
                );
            }
            let hydra = self.hydra.clone();
            cx.spawn(async move |this, cx| {
                let (run, done) = cx
                    .background_spawn(async move {
                        let done = linear_job(&hydra, &env, &settings, &run, start);
                        (run, done)
                    })
                    .await;
                let wrote = match done {
                    Ok((wrote, ticket)) => {
                        if let Some(t) = ticket {
                            super::orchestrator::note_run(
                                &run.id,
                                "linear ticket",
                                serde_json::json!({"url": t.url,
                                    "sla_breaches_at": t.sla_breaches_at,
                                    "sla_high_risk_at": t.sla_high_risk_at}),
                            );
                            let _ = this.update(cx, |ws, cx| {
                                ws.linear_tickets
                                    .insert(t.identifier.to_ascii_uppercase(), t);
                                cx.notify();
                            });
                        }
                        Ok(wrote)
                    }
                    Err(e) => Err(e),
                };
                match wrote {
                    Ok(wrote) => {
                        for what in wrote {
                            super::orchestrator::note_run(
                                &run.id,
                                &format!("linear {what}"),
                                serde_json::json!({"ticket": run.ticket}),
                            );
                        }
                    }
                    Err(e) => {
                        crate::action_log::write(format!("linear: run {}: {e}", run.id));
                        super::orchestrator::note_run(
                            &run.id,
                            "linear failed",
                            serde_json::json!({"reason": e.to_string()}),
                        );
                    }
                }
            })
            .detach();
        }
    }

    /// A run's ticket under its card and on its Pipeline tab: the SLA pill and **Open in
    /// Linear**, then each PR its stages reported, one per line, labelled with the ticket.
    pub(super) fn render_ticket_link(&self, ticket: &str, prs: &[String]) -> Option<AnyElement> {
        let t = self.linear_tickets.get(&ticket.to_ascii_uppercase());
        let pr_rows: Vec<AnyElement> = prs
            .iter()
            .filter_map(|url| {
                let label = linear_watch::pr_label(ticket, url)?;
                let url = url.clone();
                Some(
                    div()
                        .id(SharedString::from(format!("pr-link-{ticket}-{url}")))
                        .text_xs()
                        .text_color(color(theme::TEAL))
                        .cursor_pointer()
                        .hover(|d| d.underline())
                        .truncate()
                        .child(format!("{label} ↗"))
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .into_any_element(),
                )
            })
            .collect();
        if t.is_none() && pr_rows.is_empty() {
            return None;
        }
        let linear = t.map(|t| {
            let pill = t.sla_breaches_at.as_deref().and_then(|b| {
                linear_watch::sla_pill(b, t.sla_high_risk_at.as_deref(), SystemTime::now())
            });
            let url = t.url.clone();
            h_flex()
                .gap_2()
                .children(pill.map(|(text, level)| {
                    let (fg, bg) = match level {
                        SlaLevel::Ok => (theme::TEXT_SECONDARY, theme::RAISED),
                        SlaLevel::Risk => (theme::AMBER, theme::AMBER_TINT),
                        SlaLevel::Breached => (theme::RED, theme::RED_TINT),
                    };
                    div()
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(px(4.))
                        .text_xs()
                        .bg(color(bg))
                        .text_color(color(fg))
                        .child(text)
                }))
                .when(!url.is_empty(), |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("linear-link-{ticket}")))
                            .text_xs()
                            .text_color(color(theme::TEAL))
                            .cursor_pointer()
                            .hover(|d| d.underline())
                            .child("Open in Linear ↗")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                })
        });
        Some(
            v_flex()
                .gap(px(3.))
                .min_w_0()
                .children(linear)
                .children(pr_rows)
                .into_any_element(),
        )
    }

    /// The ticket of `orch`'s current run and the PRs its stages reported.
    pub(super) fn current_ticket(
        &self,
        orch: PaneId,
        cx: &gpui::App,
    ) -> Option<(String, Vec<String>)> {
        let token = self.panes.get(&orch)?.read(cx).mcp_token()?.to_string();
        let sv = server::get()?;
        let reg = server::lock(sv.registry());
        reg.runs
            .current(&token)
            .map(|r| (r.ticket.clone(), linear_watch::pr_links(r)))
    }

    /// The Linear lines under an orchestrator's card: the Automatic/Manual switch, its state,
    /// **Pick now**, and one line on what that means now; `None` when its pipeline has no
    /// `[linear]` table.
    pub(super) fn render_linear_row(
        &self,
        orch: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // What the card says about where work comes from: Linear or a Slack channel.
        struct Source {
            looking: String,
            every_minutes: u32,
            noun: &'static str,
            waiting: usize,
            tooltip: String,
            pick: &'static str,
            pick_tooltip: String,
            /// A Slack trigger offers Connect Slack while it's stopped (a missing or bad token).
            env: Option<String>,
        }
        let source = match (self.linear_of(orch, cx), self.slack_of(orch, cx)) {
            (Some((_, s, _)), _) => Source {
                looking: format!("Looking in Linear for the next {} ticket…", s.team),
                every_minutes: s.every_minutes,
                noun: "ticket",
                waiting: 0,
                tooltip: linear_watch::trigger_tooltip(&s),
                env: None,
                pick: "Pick next ticket",
                pick_tooltip: format!(
                    "Start the {} ticket in {} whose SLA breaches first, now. Once: it doesn't \
                     switch Automatic on or off.",
                    s.team, s.pick
                ),
            },
            (None, Some((p, s, env))) => Source {
                env: Some(env),
                looking: crate::slack_watch::looking(&s),
                every_minutes: s.every_minutes,
                noun: "PR",
                waiting: self.slack_waiting(&p.file),
                tooltip: crate::slack_watch::tooltip(&s),
                pick: "Take next PR",
                pick_tooltip: format!(
                    "Start the next pull request: one already taken, else look in {} now. \
                     Once: it doesn't switch Automatic on or off.",
                    s.label
                ),
            },
            (None, None) => return None,
        };
        let w = self.linear_watch.get(&orch);
        let open_run = self.open_run_ticket(orch, cx);
        let (mode, line) = linear_watch::trigger(&linear_watch::TriggerFacts {
            on: w.is_some_and(|w| w.on),
            checking: w.is_some_and(|w| w.checking),
            stopped: w.and_then(|w| w.stopped.as_deref()),
            open_run: open_run.as_deref(),
            asking: self.run_asking(orch, cx),
            found: w.and_then(|w| w.status.as_deref()),
            last: w.and_then(|w| w.last),
            now: SystemTime::now(),
            looking: &source.looking,
            every_minutes: source.every_minutes,
            noun: source.noun,
            waiting: source.waiting,
        });
        let (fg, bg, dot) = match mode {
            linear_watch::Mode::Automatic => (theme::GREEN, theme::GREEN_TINT, "●"),
            linear_watch::Mode::Manual => (theme::TEXT_SECONDARY, theme::RAISED, "○"),
            linear_watch::Mode::Stopped => (theme::RED, theme::RED_TINT, "■"),
        };
        let on = mode == linear_watch::Mode::Automatic;
        let checking = w.is_some_and(|w| w.checking);
        let tooltip = source.tooltip;
        Some(
            v_flex()
                .id(SharedString::from(format!("linear-row-{orch}")))
                .ml(px(26.))
                .mr_1()
                .my_1()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Switch::new(SharedString::from(format!("linear-watch-{orch}")))
                                .checked(on)
                                .small()
                                .tooltip(tooltip)
                                .on_click(cx.listener(move |ws, _: &bool, window, cx| {
                                    ws.toggle_linear_watch(orch, window, cx)
                                })),
                        )
                        .child(
                            div()
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .text_xs()
                                .bg(color(bg))
                                .text_color(color(fg))
                                .child(format!("{dot} {}", mode.label())),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(if mode == linear_watch::Mode::Stopped && !checking {
                            theme::RED
                        } else {
                            theme::TEXT_SECONDARY
                        }))
                        .child(line),
                )
                .when_some(
                    source
                        .env
                        .clone()
                        .filter(|_| mode == linear_watch::Mode::Stopped),
                    |d, env| {
                        d.child(
                            h_flex().child(
                                Button::new(SharedString::from(format!("slack-connect-{orch}")))
                                    .label("Connect Slack")
                                    .primary()
                                    .xsmall()
                                    .tooltip(
                                        "Sign in to Slack in your browser; the token is kept in \
                                         your keychain and Automatic comes back on.",
                                    )
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.connect_slack(env.clone(), window, cx)
                                    })),
                            ),
                        )
                    },
                )
                // Automatic picks by itself; a pick by hand is for Manual (or Stopped).
                .when(!on && open_run.is_none(), |d| {
                    d.child(
                        h_flex().child(
                            Button::new(SharedString::from(format!("linear-now-{orch}")))
                                .label(source.pick)
                                .outline()
                                .xsmall()
                                .loading(checking)
                                .tooltip(source.pick_tooltip)
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.check_linear_now(orch, window, cx)
                                })),
                        ),
                    )
                })
                .into_any_element(),
        )
    }
}
