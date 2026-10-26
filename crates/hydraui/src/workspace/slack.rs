//! A `[slack]` pipeline's watcher in the app: the looks at its channel (marking each message it
//! takes), the pull requests waiting their turn, and telling the orchestrator to start the next
//! one when no run is open. It shares the card's Automatic switch and `linear::Watch` with the
//! Linear trigger: a pipeline has one or the other. The decisions are `slack_watch`'s.

use std::collections::VecDeque;
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{Context, Window};
use gpui_component::WindowExt;
use gpui_component::notification::Notification;

use super::Workspace;
use super::linear::START_WITHIN;
use crate::envs::PaneId;
use crate::linear::LinearError;
use crate::pipeline::{Pipeline, SlackSettings};
use crate::slack_watch::{self, Queued};

impl Workspace {
    /// `orch`'s pipeline with its `[slack]` table, and its environment; `None` without one.
    pub(super) fn slack_of(
        &self,
        orch: PaneId,
        cx: &gpui::App,
    ) -> Option<(Pipeline, SlackSettings, String)> {
        let (file, env, _) = self.settings_target(orch, cx)?;
        let pipeline = crate::pipeline::load(self.hydra.paths.root(), &file).ok()?;
        let settings = pipeline.slack.clone()?;
        Some((pipeline, settings, env))
    }

    /// Pipeline `file`'s waiting pull requests, read from its file the first time.
    pub(super) fn slack_queue(&mut self, file: &str) -> &mut VecDeque<Queued> {
        let root = self.hydra.paths.root().to_path_buf();
        self.slack_queues
            .entry(file.to_string())
            .or_insert_with(|| slack_watch::load_queue(&slack_watch::queue_file(&root, file)))
    }

    fn save_slack_queue(&mut self, file: &str) {
        let path = slack_watch::queue_file(self.hydra.paths.root(), file);
        let queue = self.slack_queue(file).clone();
        if let Err(e) = slack_watch::save_queue(&path, &queue) {
            crate::action_log::write(format!("slack: {file}: can't keep its queue: {e}"));
        }
    }

    /// How many pull requests wait for pipeline `file`, as last read.
    pub(super) fn slack_waiting(&self, file: &str) -> usize {
        self.slack_queues.get(file).map_or(0, VecDeque::len)
    }

    /// Each second, for a `[slack]` orchestrator with no run open: the next waiting pull
    /// request is started; with none waiting, a look is due (`true`) by the usual timer.
    pub(super) fn slack_tick(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((pipeline, settings, _)) = self.slack_of(orch, cx) else {
            return false;
        };
        // A slot told to start a pull request that didn't within 5 minutes: skipped, said so.
        for slot in self.slots(orch, cx) {
            let stale = self
                .linear_watch
                .get(&slot)
                .and_then(|w| w.starting.clone())
                .filter(|(_, since)| since.elapsed().unwrap_or_default() >= START_WITHIN);
            let Some((ticket, _)) = stale else {
                continue;
            };
            if let Some(w) = self.linear_watch.get_mut(&slot) {
                w.starting = None;
            }
            crate::action_log::write(format!(
                "slack: {}: the orchestrator didn't start {ticket}; skipped",
                pipeline.file
            ));
            window.push_notification(
                Notification::warning(format!(
                    "The orchestrator didn't start {ticket} within 5 minutes, so it was skipped. \
                     Give it to the orchestrator yourself if it still needs a review."
                ))
                .title(pipeline.name.clone()),
                cx,
            );
        }
        if self.slack_queue(&pipeline.file).is_empty() {
            return self.can_take(orch, cx);
        }
        let Some(slot) = self.slot_for_work(orch, window, cx) else {
            return false;
        };
        self.start_next_pr(orch, slot, &pipeline, &settings, window, cx);
        false
    }

    /// Tells orchestrator `slot` (one of `orch`'s runs at once) to start the next waiting pull
    /// request.
    pub(super) fn start_next_pr(
        &mut self,
        orch: PaneId,
        slot: PaneId,
        pipeline: &Pipeline,
        settings: &SlackSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pr) = self.slack_queue(&pipeline.file).pop_front() else {
            return;
        };
        self.save_slack_queue(&pipeline.file);
        self.linear_watch.entry(slot).or_default().starting =
            Some((pr.ticket.clone(), SystemTime::now()));
        self.linear_watch.entry(orch).or_default().status = Some(format!("Started {}", pr.ticket));
        crate::action_log::write(format!(
            "slack: {}: starting {} ({})",
            pipeline.file, pr.ticket, pr.url
        ));
        let prompt = slack_watch::start_prompt(pipeline, settings, &pr);
        self.pending_wakes.insert(slot, (pr.ticket.clone(), prompt));
        self.send_wakes(window, cx);
        cx.notify();
    }

    /// A look at the channel, off the UI thread: new messages marked and their pull requests
    /// queued; the first starts at the next tick.
    pub(super) fn start_slack_check(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pipeline, settings, env)) = self.slack_of(orch, cx) else {
            return;
        };
        let queued: Vec<String> = self
            .slack_queue(&pipeline.file)
            .iter()
            .map(|q| q.ticket.clone())
            .collect();
        let passed_over: Vec<String> = self.slack_mine.iter().cloned().collect();
        let exe = crate::mcp::server::get()
            .and_then(|sv| crate::mcp::server::lock(sv.registry()).hydra_exe.clone());
        let w = self.linear_watch.entry(orch).or_default();
        w.checking = true;
        w.last = Some(SystemTime::now());
        crate::action_log::write(format!(
            "slack: {}: looking in {}",
            pipeline.file, settings.label
        ));
        let hydra = self.hydra.clone();
        let s = settings.clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let token = hydra.slack_token(&env)?;
                    let mut ran: Vec<String> = crate::run_history::read(hydra.paths.root(), &[])
                        .into_iter()
                        .map(|e| e.ticket)
                        .collect();
                    ran.extend(passed_over);
                    // Pull requests the person watching opened are theirs: GitHub won't let
                    // them approve their own, so they're left alone. Asked of GitHub as the
                    // environment, who it is once per look.
                    let login: std::cell::OnceCell<Result<String, String>> =
                        std::cell::OnceCell::new();
                    let mine = |pr: &crate::slack::Pr| -> Result<bool, LinearError> {
                        let exe = exe.as_deref().ok_or(LinearError::Failed(
                            "hydraui can't find hydra to ask GitHub".into(),
                        ))?;
                        let me = login
                            .get_or_init(|| crate::branch_rule::gh_login(exe, &env))
                            .clone()
                            .map_err(|e| {
                                LinearError::Failed(format!("can't ask GitHub who you are: {e}"))
                            })?;
                        let author =
                            crate::branch_rule::gh_author_of(exe, &env, &pr.url).map_err(|e| {
                                LinearError::Failed(format!(
                                    "can't ask GitHub who opened {}: {e}",
                                    pr.title()
                                ))
                            })?;
                        Ok(author.eq_ignore_ascii_case(&me))
                    };
                    slack_watch::find_and_mark(
                        &crate::slack::Http { token },
                        &s,
                        SystemTime::now(),
                        &ran,
                        &queued,
                        &mine,
                    )
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.slack_checked(orch, &pipeline, &settings, found, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn slack_checked(
        &mut self,
        orch: PaneId,
        pipeline: &Pipeline,
        settings: &SlackSettings,
        found: Result<slack_watch::Found, LinearError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.linear_watch.get_mut(&orch) else {
            return;
        };
        w.checking = false;
        let by_hand = std::mem::take(&mut w.by_hand);
        if let Ok(f) = &found
            && !f.mine.is_empty()
        {
            crate::action_log::write(format!(
                "slack: {}: passed over {} (you opened them)",
                pipeline.file,
                f.mine.join(", ")
            ));
            self.slack_mine.extend(f.mine.iter().cloned());
        }
        let Some(w) = self.linear_watch.get_mut(&orch) else {
            return;
        };
        let found = found.map(|f| f.queued);
        match found {
            Ok(prs) if !prs.is_empty() => {
                w.failures.clear();
                let names: Vec<&str> = prs.iter().map(|p| p.ticket.as_str()).collect();
                crate::action_log::write(format!(
                    "slack: {}: took {} from {}",
                    pipeline.file,
                    names.join(", "),
                    settings.label
                ));
                if by_hand {
                    window.push_notification(
                        Notification::info(format!(
                            "Took {} from {}. The orchestrator is starting the first.",
                            names.join(", "),
                            settings.label
                        ))
                        .title(pipeline.name.clone()),
                        cx,
                    );
                }
                self.slack_queue(&pipeline.file).extend(prs);
                self.save_slack_queue(&pipeline.file);
                if let Some(slot) = self.slot_for_work(orch, window, cx) {
                    self.start_next_pr(orch, slot, pipeline, settings, window, cx);
                }
            }
            Ok(_) => {
                w.failures.clear();
                w.status = Some("Nothing new".to_string());
                crate::action_log::write(format!("slack: {}: nothing new", pipeline.file));
                if by_hand {
                    window.push_notification(
                        Notification::info(format!(
                            "No new pull request in {}: every message with one has a :{}: \
                             already.",
                            settings.label, settings.reaction
                        ))
                        .title(pipeline.name.clone()),
                        cx,
                    );
                }
            }
            Err(e) if !w.on || by_hand => {
                crate::action_log::write(format!("slack: {e}"));
                window.push_notification(
                    Notification::error(format!("Couldn't look in {}: {e}", settings.label))
                        .title(pipeline.name.clone()),
                    cx,
                );
            }
            Err(e) => {
                crate::action_log::write(format!("slack: {e}"));
                match w.failures.record(&e) {
                    Some(reason) => {
                        w.on = false;
                        w.status = None;
                        w.stopped = Some(reason.clone());
                        window.push_notification(
                            Notification::error(format!(
                                "Stopped watching {}: {reason}",
                                settings.label
                            ))
                            .title(pipeline.name.clone()),
                            cx,
                        );
                    }
                    None => w.status = Some("Last look failed".to_string()),
                }
            }
        }
        cx.notify();
    }

    /// **Connect Slack** for environment `env`: the browser signs in to Slack through the
    /// team's Canopy, which hands the token back here; it's stored as `<env>/slack`, and a
    /// Slack watcher in `env` that had stopped goes back to Automatic.
    pub(super) fn connect_slack(
        &mut self,
        env: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = "Connect Slack";
        let (ui, _) = crate::config::load(&self.hydra.paths);
        let Some(canopy) = ui.slack_connect_url else {
            window.push_notification(
                Notification::warning(
                    "Set slack_connect_url in hydraui's config.toml to your team's Canopy (e.g. \
                     https://canopy.dev.iov42.net): it finishes the Slack sign-in.",
                )
                .title(title),
                cx,
            );
            return;
        };
        let started = (|| {
            let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = server
                .server_addr()
                .to_ip()
                .map(|a| a.port())
                .ok_or("no local port")?;
            let state = crate::slack_connect::nonce()?;
            Ok::<_, String>((server, port, state))
        })();
        let (server, port, state) = match started {
            Ok(s) => s,
            Err(e) => {
                window.push_notification(Notification::error(e).title(title), cx);
                return;
            }
        };
        crate::action_log::write(format!("slack: connecting {env} through {canopy}"));
        cx.open_url(&crate::slack_connect::start_url(&canopy, port, &state));
        window.push_notification(
            Notification::info(
                "Finish in your browser: sign in to Canopy if asked, then Allow on Slack's page.",
            )
            .title(title),
            cx,
        );
        let hydra = self.hydra.clone();
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    let c =
                        crate::slack_connect::receive(&server, &state, crate::slack_connect::WAIT)?;
                    hydra.set_secret(&format!("{env}/slack"), &c.token)?;
                    Ok::<_, String>((env, c))
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                match done {
                    Ok((env, c)) => {
                        crate::action_log::write(format!(
                            "slack: {env} connected as {} in {}",
                            c.user, c.team
                        ));
                        // Watchers that stopped for want of a token go again.
                        let stopped: Vec<PaneId> = ws
                            .linear_watch
                            .iter()
                            .filter(|(_, w)| w.stopped.is_some())
                            .map(|(id, _)| *id)
                            .filter(|id| ws.slack_of(*id, cx).is_some_and(|(_, _, e)| e == env))
                            .collect();
                        for id in &stopped {
                            if let Some(w) = ws.linear_watch.get_mut(id) {
                                w.on = true;
                                w.stopped = None;
                                w.last = None;
                                w.failures.clear();
                            }
                        }
                        window.push_notification(
                            Notification::success(format!(
                                "Slack is connected for {env}{}.{}",
                                if c.team.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", c.team)
                                },
                                if stopped.is_empty() {
                                    ""
                                } else {
                                    " Pipelines watching Slack are back on Automatic."
                                }
                            ))
                            .title(title),
                            cx,
                        );
                        cx.notify();
                    }
                    Err(e) => {
                        crate::action_log::write(format!("slack: connect failed: {e}"));
                        window.push_notification(Notification::error(e).title(title), cx);
                    }
                }
            });
        })
        .detach();
    }
}
