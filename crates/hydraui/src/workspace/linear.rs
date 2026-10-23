//! The orchestrator's Linear watcher in the app (phase 2 spec §3, §4): the **Watch Linear**
//! switch, the checks it makes, and the writes back when a run starts or ends. The decisions
//! are `linear_watch`'s; the calls are `linear::Http`'s, made off the UI thread.

use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{Sizable, h_flex};

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
        key: hydra.linear_key(env).map_err(LinearError::Rejected)?,
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
    fn has_open_run(&self, orch: PaneId, cx: &gpui::App) -> bool {
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

    /// **Watch Linear**: on looks at once; only one orchestrator may watch a team (§3).
    pub(super) fn toggle_linear_watch(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on = self.linear_watch.get(&orch).is_some_and(|w| w.on);
        if on {
            if let Some(w) = self.linear_watch.get_mut(&orch) {
                w.on = false;
                w.status = None;
            }
            cx.notify();
            return;
        }
        let Some((_, settings, _)) = self.linear_of(orch, cx) else {
            return;
        };
        let teams: Vec<(PaneId, String)> = self
            .linear_watch
            .iter()
            .filter(|(id, w)| w.on && self.panes.contains_key(id))
            .filter_map(|(id, _)| Some((*id, self.linear_of(*id, cx)?.1.team)))
            .collect();
        let other = linear_watch::watched_by(
            &settings.team,
            &orch,
            teams.iter().map(|(id, t)| (id, t.as_str())),
        );
        if other.is_some() {
            window.push_notification(
                Notification::warning(format!(
                    "Another orchestrator already watches {}: only one may, or they'd race for \
                     tickets.",
                    settings.team
                ))
                .title("Watch Linear"),
                cx,
            );
            return;
        }
        let w = self.linear_watch.entry(orch).or_default();
        w.on = true;
        w.last = None;
        w.failures.clear();
        w.status = Some("Looking…".into());
        self.linear_tick(window, cx);
    }

    /// **Check Linear now**.
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
        self.start_linear_check(orch, window, cx);
    }

    /// Each second: a watcher whose check is due starts one (§3).
    pub(super) fn linear_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = SystemTime::now();
        let due: Vec<PaneId> = self
            .linear_watch
            .iter()
            .filter(|(id, w)| w.on && !w.checking && self.panes.contains_key(id))
            .map(|(id, _)| *id)
            .filter(|id| {
                let every = self
                    .linear_of(*id, cx)
                    .map_or(5, |(_, s, _)| s.every_minutes);
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
        let Some((pipeline, settings, env)) = self.linear_of(orch, cx) else {
            return;
        };
        let w = self.linear_watch.entry(orch).or_default();
        w.checking = true;
        w.last = Some(SystemTime::now());
        let hydra = self.hydra.clone();
        let (p, s) = (pipeline.clone(), settings.clone());
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let key = hydra.linear_key(&env).map_err(LinearError::Rejected)?;
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
                w.status = Some(format!("Picked {}{also}", ticket.identifier));
                self.linear_tickets
                    .insert(ticket.identifier.to_ascii_uppercase(), ticket.clone());
                crate::action_log::write(format!(
                    "linear: claimed {} for the {} orchestrator",
                    ticket.identifier, pipeline.file
                ));
                let prompt = linear_watch::start_prompt(pipeline, &ticket);
                self.pending_wakes
                    .insert(orch, (ticket.identifier.clone(), prompt));
                self.send_wakes(window, cx);
            }
            Ok(check) => {
                w.failures.clear();
                w.status = Some(format!("Nothing to pick{}", skipped(&check)));
            }
            Err(e) => {
                crate::action_log::write(format!("linear: {e}"));
                match w.failures.record(&e) {
                    Some(reason) => {
                        w.on = false;
                        w.status = Some(format!("Stopped: {reason}"));
                        window.push_notification(
                            Notification::error(format!("Stopped watching Linear: {reason}"))
                                .title(pipeline.name.clone()),
                            cx,
                        );
                    }
                    None => w.status = Some(format!("{e} (will try again)")),
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

    /// The ticket's SLA pill and its link to Linear (§5); `None` before Linear has said
    /// anything about `ticket`.
    pub(super) fn render_ticket_link(&self, ticket: &str) -> Option<AnyElement> {
        let t = self.linear_tickets.get(&ticket.to_ascii_uppercase())?;
        let pill = t.sla_breaches_at.as_deref().and_then(|b| {
            linear_watch::sla_pill(b, t.sla_high_risk_at.as_deref(), SystemTime::now())
        });
        let url = t.url.clone();
        Some(
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
                .into_any_element(),
        )
    }

    /// The ticket of `orch`'s current run.
    pub(super) fn current_ticket(&self, orch: PaneId, cx: &gpui::App) -> Option<String> {
        let token = self.panes.get(&orch)?.read(cx).mcp_token()?.to_string();
        let sv = server::get()?;
        let reg = server::lock(sv.registry());
        reg.runs.current(&token).map(|r| r.ticket.clone())
    }

    /// The Linear line under an orchestrator's card: **Watch Linear** and **Check now**, and
    /// what the last check found; `None` when its pipeline has no `[linear]` table.
    pub(super) fn render_linear_row(
        &self,
        orch: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (_, settings, _) = self.linear_of(orch, cx)?;
        let w = self.linear_watch.get(&orch);
        let on = w.is_some_and(|w| w.on);
        let status = match w {
            Some(w) if w.checking => "Looking…".to_string(),
            Some(w) if w.on => {
                let ago = w
                    .last
                    .and_then(|l| SystemTime::now().duration_since(l).ok())
                    .map(|d| format!(" · looked {}m ago", d.as_secs() / 60))
                    .unwrap_or_default();
                format!("{}{ago}", w.status.clone().unwrap_or_default())
            }
            Some(w) => w.status.clone().unwrap_or_default(),
            None => String::new(),
        };
        Some(
            h_flex()
                .id(SharedString::from(format!("linear-row-{orch}")))
                .ml(px(26.))
                .mr_1()
                .gap_1()
                .flex_wrap()
                .child(
                    Button::new(SharedString::from(format!("linear-watch-{orch}")))
                        .label(if on {
                            "Watching Linear".to_string()
                        } else {
                            format!("Watch Linear ({})", settings.team)
                        })
                        .when(on, |b| b.primary())
                        .when(!on, |b| b.outline())
                        .xsmall()
                        .tooltip(format!(
                            "Pick {} tickets in {} every {} min",
                            settings.team, settings.pick, settings.every_minutes
                        ))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.toggle_linear_watch(orch, window, cx)
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("linear-now-{orch}")))
                        .label("Check now")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.check_linear_now(orch, window, cx)
                        })),
                )
                .when(!status.is_empty(), |d| {
                    d.child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(status),
                    )
                })
                .into_any_element(),
        )
    }
}
