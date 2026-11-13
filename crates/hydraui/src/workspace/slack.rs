//! A `[slack]` pipeline's watcher in the app: the looks at its channel (marking each message it
//! takes), the pull requests waiting their turn, and telling the orchestrator to start the next
//! one when no run is open. It shares the card's Automatic switch and `linear::Watch` with the
//! Linear trigger: a pipeline has one or the other. The decisions are `slack_watch`'s.

use crate::inbox::{Note, PushNote};
use std::collections::VecDeque;
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{Context, Window};

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

    /// Each second, for a `[slack]` orchestrator: the next waiting pull request starts when an
    /// orchestrator has room (`false`: the look waits a tick); otherwise a look is due (`true`)
    /// by the usual timer, full or not.
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
            // Handed over once without starting: it goes back to the front for another try.
            if let Some(mut pr) = self.slack_inflight.remove(&slot)
                && pr.tries == 0
            {
                pr.tries = 1;
                crate::action_log::write(format!(
                    "slack: {}: the orchestrator didn't start {ticket}; trying again",
                    pipeline.file
                ));
                self.slack_queue(&pipeline.file).push_front(pr);
                self.save_slack_queue(&pipeline.file);
                continue;
            }
            crate::action_log::write(format!(
                "slack: {}: the orchestrator didn't start {ticket}; skipped",
                pipeline.file
            ));
            window.push_note(
                Note::warning(format!(
                    "The orchestrator didn't start {ticket} within 5 minutes, so it was skipped. \
                     Give it to the orchestrator yourself if it still needs a review."
                ))
                .title(pipeline.name.clone()),
                cx,
            );
        }
        if self.slack_queue(&pipeline.file).is_empty() {
            return true;
        }
        let Some(slot) = self.slot_for_work(orch, window, cx) else {
            return true;
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
        self.slack_handed.insert(pr.ticket.to_lowercase());
        self.slack_inflight.insert(slot, pr.clone());
        self.linear_watch.entry(orch).or_default().status = Some(format!("Started {}", pr.ticket));
        crate::action_log::write(format!(
            "slack: {}: starting {} ({})",
            pipeline.file, pr.ticket, pr.url
        ));
        let prompt = slack_watch::start_prompt(pipeline, settings, &pr);
        self.queue_wake(slot, pr.ticket.clone(), prompt);
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
        // Handed to an orchestrator and not started yet counts as waiting too.
        let mut queued: Vec<String> = self
            .slack_queue(&pipeline.file)
            .iter()
            .map(|q| q.ticket.clone())
            .collect();
        queued.extend(self.slack_inflight.values().map(|q| q.ticket.clone()));
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
        // Taken through the swarm first when the pipeline names one (§87).
        let shared = pipeline.claims_in();
        let claims_api = super::jobs::claims_api();
        let label = pipeline.name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let token = hydra.slack_token(&env)?;
                    // Only a run still open holds its pull request: one posted again after its
                    // review ended is reviewed again (core spec §40).
                    let history = crate::run_history::read(hydra.paths.root(), &[]);
                    let mut ran = slack_watch::still_open(&history);
                    let mut reviewed = slack_watch::reviewed(&history);
                    // Ones the person watching opened are never reviewed; they don't hold a tick.
                    reviewed.extend(passed_over.iter().cloned());
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
                    let take = |key: &str| match &shared {
                        Some((swarm, context)) => {
                            super::jobs::take(claims_api.as_ref(), swarm, context, key, &label)
                        }
                        None => crate::jobs::Taken::Unsettled(String::new()),
                    };
                    let give_back = |holder: u64| {
                        if let Some(api) = &claims_api {
                            let _ = api.release(holder);
                        }
                    };
                    slack_watch::find_and_mark_taking(
                        &crate::slack::Http { token },
                        &s,
                        SystemTime::now(),
                        &ran,
                        &queued,
                        &reviewed,
                        &mine,
                        &slack_watch::Claims {
                            take: &take,
                            give_back: &give_back,
                        },
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
        if let Ok(f) = &found {
            if !f.ticked.is_empty() {
                crate::action_log::write(format!(
                    "slack: {}: marked {} message(s) done with :{}:",
                    pipeline.file,
                    f.ticked.len(),
                    settings.done_reaction
                ));
            }
            if let Some(e) = &f.tick_failed {
                crate::action_log::write(format!(
                    "slack: {}: can't mark a message done: {e}",
                    pipeline.file
                ));
            }
            // Taken through the swarm: kept while their runs are open (§87).
            self.hold_claims(f.claims.clone());
            for (ticket, who) in &f.held {
                crate::action_log::write(format!(
                    "slack: {}: {ticket}: {who} has it in the swarm; left to them",
                    pipeline.file
                ));
            }
            if let Some(why) = &f.unsettled {
                crate::action_log::write(format!("slack: {}: claims: {why}", pipeline.file));
            }
            // Someone marked the same message in the same moment and was first (§87).
            for (ticket, who) in &f.lost {
                crate::action_log::write(format!(
                    "slack: {}: {ticket}: Slack user {who} marked it first; left to them",
                    pipeline.file
                ));
            }
        }
        let Some(w) = self.linear_watch.get_mut(&orch) else {
            return;
        };
        let lost: Option<String> = found.as_ref().ok().filter(|f| !f.lost.is_empty()).map(|f| {
            let names: Vec<&str> = f.lost.iter().map(|(t, _)| t.as_str()).collect();
            format!("{} taken by someone else first", names.join(", "))
        });
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
                    window.push_note(
                        Note::info(format!(
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
                w.status = Some(lost.unwrap_or_else(|| "Nothing new".to_string()));
                crate::action_log::write(format!("slack: {}: nothing new", pipeline.file));
                if by_hand {
                    window.push_note(
                        Note::info(format!(
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
                window.push_note(
                    Note::error(format!("Couldn't look in {}: {e}", settings.label))
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
                        let s = crate::linear_watch::stopped("Slack", &reason);
                        window.push_note(
                            Note::error(format!("{}. {}", s.title, s.hint))
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

    /// What Connect Slack is doing for an environment, under the button.
    pub(in crate::workspace) fn slack_connecting_of(&self, env: &str) -> Option<&SlackConnecting> {
        self.slack_connecting.get(env)
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
        // Without the team's Canopy there's no sign-in to start: ask for it here, where the
        // button was pressed, then carry on (a notice in the bell looked like nothing happened).
        let Some(canopy) = ui.slack_connect_url else {
            self.text_dialog_about(
                "Connect Slack: your team's Canopy".into(),
                Some(
                    "Slack sign-in goes through your team's Canopy, which hydraui needs to know \
                     once. Ask whoever set up hydraui for your team for its address.",
                ),
                String::new(),
                "https://canopy.example.com",
                move |ws, value, window, cx| {
                    let url = crate::config::slack_connect_url(&value)?;
                    crate::config::set(
                        &ws.hydra.paths,
                        "slack_connect_url",
                        Some(toml_edit::Value::from(url)),
                    )?;
                    crate::action_log::write("slack: set slack_connect_url");
                    // Saved: start the sign-in now, as the button meant.
                    ws.connect_slack(env.clone(), window, cx);
                    Ok(())
                },
                window,
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
                self.slack_connecting
                    .insert(env.clone(), SlackConnecting::Failed(e.clone()));
                window.push_note(Note::error(e).title(title), cx);
                cx.notify();
                return;
            }
        };
        crate::action_log::write(format!("slack: connecting {env} through {canopy}"));
        let url = crate::slack_connect::start_url(&canopy, port, &state);
        cx.open_url(&url);
        // Said under the button, with the page to open again if the browser didn't.
        self.slack_connecting
            .insert(env.clone(), SlackConnecting::Waiting(url));
        cx.notify();
        window.push_note(
            Note::info(
                "Finish in your browser: sign in to Canopy if asked, then Allow on Slack's page.",
            )
            .title(title),
            cx,
        );
        let hydra = self.hydra.clone();
        let waited = env.clone();
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
                        ws.slack_connecting.remove(&env);
                        super::services::note(&env, crate::pipeline::Service::Slack, true);
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
                        window.push_note(
                            Note::success(format!(
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
                        ws.slack_connecting
                            .insert(waited.clone(), SlackConnecting::Failed(e.clone()));
                        window.push_note(Note::error(e).title(title), cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

/// Where Connect Slack is for an environment (core spec §73).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::workspace) enum SlackConnecting {
    /// The sign-in page is open in the browser (this address, to open again).
    Waiting(String),
    /// It couldn't connect, and why.
    Failed(String),
}

impl Workspace {
    /// A run of a Slack pipeline started by hand, not taken by its trigger: its pull request's
    /// message gets the trigger's reaction, so nobody else picks it up (core spec §85). Off the
    /// UI thread; a message someone already marked, or one not in the channel, is said.
    pub(super) fn mark_hand_started(
        &mut self,
        run: &str,
        env: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((ticket, link, settings, name)) = crate::mcp::server::get().and_then(|sv| {
            let reg = crate::mcp::server::lock(sv.registry());
            let r = reg.runs.run(run)?;
            Some((
                r.ticket.clone(),
                r.link.clone().unwrap_or_default(),
                r.pipeline.slack.clone()?,
                r.pipeline.name.clone(),
            ))
        }) else {
            return;
        };
        // Taken by the trigger: marked when it was taken.
        if self.slack_handed.contains(&ticket.to_lowercase()) {
            return;
        }
        let (hydra, env, run) = (self.hydra.clone(), env.to_string(), run.to_string());
        let label = settings.label.clone();
        cx.spawn_in(window, async move |_, cx| {
            let marked = cx
                .background_spawn(async move {
                    let token = hydra.slack_token(&env).map_err(|e| e.to_string())?;
                    crate::slack_watch::mark_started(
                        &crate::slack::Http { token },
                        &settings,
                        SystemTime::now(),
                        &ticket,
                        &link,
                    )
                    .map_err(|e| e.to_string())
                })
                .await;
            let _ = cx.update(|window, cx| {
                use crate::slack_watch::Marked;
                let said = match &marked {
                    Ok(Marked::Marked) => None,
                    Ok(Marked::TakenAlready) => Some(format!(
                        "{run} was started by hand, but its message in {label} already has a \
                         reaction: someone may be reviewing it too."
                    )),
                    Ok(Marked::NotFound) => Some(format!(
                        "{run} was started by hand, and its message isn't in {label} (or is \
                         older than the look-back), so it isn't marked there: tell the team \
                         you have it."
                    )),
                    Err(e) => Some(format!("{run}: couldn't mark its message in {label}: {e}")),
                };
                crate::action_log::write(format!(
                    "slack: {run} started by hand: {}",
                    said.as_deref().unwrap_or("marked its message")
                ));
                if let Some(said) = said {
                    window.push_note(Note::warning(said).title(name.clone()), cx);
                }
            });
        })
        .detach();
    }
}
