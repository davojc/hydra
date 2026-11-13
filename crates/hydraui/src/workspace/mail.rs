//! `[mail]` pipelines in the app (email triage spec §3): a look at the mailbox on the
//! pipeline's timetable, or at once with **Check mail now**, and the threads it found handed to
//! the pipeline's orchestrator one at a time, each as its own run. Looked at every 30 seconds
//! with the schedules.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Context, Window};

use super::Workspace;
use crate::gmail::{self, Endpoints};
use crate::inbox::{Note, PushNote};
use crate::mail_watch::{self, Mailed};
use crate::pipeline::{MailSettings, Pipeline};

/// How long a thread handed to an orchestrator has to become a run before it's given up on.
const START_WITHIN: Duration = Duration::from_secs(5 * 60);

/// What `schedule.json` keeps a pipeline's last look under, beside its scheduled runs.
fn look_key(file: &str) -> String {
    format!("mail:{file}")
}

/// What `schedule.json` keeps a pipeline's last look's finding under (core spec §89).
pub(super) fn found_key(file: &str) -> String {
    format!("found:mail:{file}")
}

impl Workspace {
    /// The line under a mail pipeline's card (core spec §89): when it looks, what its last
    /// look found, and when it looks next. Only on the card holding the pipeline's trigger.
    pub(super) fn render_mail_row(
        &self,
        orch: crate::envs::PaneId,
        cx: &gpui::App,
    ) -> Option<gpui::AnyElement> {
        use crate::orchestrator_theme::{self as theme, color};
        use gpui::prelude::*;
        if self.lead_of(orch, cx) != orch {
            return None;
        }
        let file = self.orchestrators.get(&orch)?;
        let root = self.hydra.paths.root();
        let pipeline = crate::pipeline::load(root, file).ok()?;
        let when = pipeline.mail.as_ref()?.when()?;
        let last = crate::schedule::last_runs(root);
        let line = crate::schedule::look_line(
            &when,
            &crate::action_log::now(),
            last.get(&look_key(file)).map(String::as_str),
            last.get(&found_key(file)).map(String::as_str),
        );
        Some(
            gpui::div()
                .ml(gpui::px(26.))
                .mr_1()
                .my_1()
                .text_xs()
                .text_color(color(theme::TEXT_SECONDARY))
                .child(format!("Mail · {line}"))
                .into_any_element(),
        )
    }
}

impl Workspace {
    /// Each `[mail]` pipeline: a look when its time has come (every one when `now`), then the
    /// next waiting thread to its orchestrator once the last one handed over has started.
    pub(super) fn mail_tick(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.hydra.paths.root().to_path_buf();
        let at = crate::action_log::now();
        let last = crate::schedule::last_runs(&root);
        let mut any = false;
        for file in crate::pipeline::names(&root) {
            let Ok(p) = crate::pipeline::load(&root, &file) else {
                continue;
            };
            let Some(m) = p.mail.clone() else {
                continue;
            };
            any = true;
            // Below the Claude week's floor no email is taken or started (core spec §83).
            if let Some(left) = self
                .mail_env(&p, &m, cx)
                .and_then(|env| self.week_held(&env))
            {
                if now {
                    let floor = crate::mcp::server::get()
                        .map_or(crate::runs::MIN_WEEK_LEFT, |sv| {
                            crate::mcp::server::lock(sv.registry()).min_week_left
                        });
                    window.push_note(
                        Note::info(crate::runs::week_hold_line(left, floor)).title("Check mail"),
                        cx,
                    );
                }
                self.release_mail(&p, &m, window, cx);
                continue;
            }
            let due = now
                || m.when()
                    .is_some_and(|w| w.due(&at, last.get(&look_key(&file)).map(String::as_str)));
            if due && !self.mail_looking.contains(&file) {
                if let Err(e) = crate::schedule::mark_run(&root, &look_key(&file), &at) {
                    crate::action_log::write(format!("mail {file}: can't note the look: {e}"));
                    continue;
                }
                self.start_mail_look(&p, &m, now, window, cx);
            }
            self.start_next_mail(&p, &m, window, cx);
            self.release_mail(&p, &m, window, cx);
        }
        if now && !any {
            window.push_note(
                Note::info("No pipeline takes its work from mail: add a [mail] table to one.")
                    .title("Check mail"),
                cx,
            );
        }
    }

    /// The environment a `[mail]` pipeline runs in, as a scheduled one's (§58).
    pub(super) fn mail_env(
        &self,
        p: &Pipeline,
        m: &MailSettings,
        cx: &gpui::App,
    ) -> Option<String> {
        let running = self.orchestrators.iter().find_map(|(id, f)| {
            (*f == p.file).then(|| self.panes.get(id).map(|v| v.read(cx).env().to_string()))?
        });
        crate::schedule::pick_env(
            m.env.as_deref(),
            p.autostart.as_deref(),
            running.as_deref(),
            self.ui.default_env.as_deref(),
            &self.hydra.env_names(),
        )
    }

    /// A look at the mailbox, off the UI thread: each new thread's `email.md` written, then
    /// queued here.
    fn start_mail_look(
        &mut self,
        p: &Pipeline,
        m: &MailSettings,
        by_hand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = p.name.clone();
        let Some(client) = self.ui.gmail_client() else {
            window.push_note(
                Note::warning(
                    "Set gmail_client_id and gmail_client_secret in hydraui's config.toml, \
                     then Connect Gmail, so this pipeline can read its mailbox.",
                )
                .title(title),
                cx,
            );
            return;
        };
        let Some(env) = self.mail_env(p, m, cx) else {
            window.push_note(
                Note::error("There's no environment to read the mailbox in.").title(title),
                cx,
            );
            return;
        };
        // Claims carry who made them (spec §5): the name the swarms know you by.
        let Some(me) = self
            .ui
            .peer_name
            .as_deref()
            .map(crate::mail_claim::slug)
            .filter(|s| !s.is_empty())
        else {
            window.push_note(
                Note::warning(
                    "Set peer_name in hydraui's config.toml to your name: claims on emails \
                     say who has them.",
                )
                .title(title),
                cx,
            );
            return;
        };
        let swarm = match &m.swarm {
            None => None,
            Some(name) => match self.mail_swarm_folder(name) {
                Some(f) => Some(f),
                None => {
                    window.push_note(
                        Note::error(format!(
                            "[mail] swarm = \"{name}\", but this machine isn't in a swarm \
                             by that name."
                        ))
                        .title(title),
                        cx,
                    );
                    return;
                }
            },
        };
        let root = self.hydra.paths.root().to_path_buf();
        let file = p.file.clone();
        let seen = mail_watch::load(&root, &file).seen;
        let search = mail_watch::search(m);
        let hydra = self.hydra.clone();
        self.mail_looking.insert(file.clone());
        crate::action_log::write(format!("mail {file}: looking ({search})"));
        let (p, m) = (p.clone(), m.clone());
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let ep = Endpoints::google();
                    let access = gmail::refresh(&ep, &client, &hydra.gmail_sign_in(&env)?)?;
                    let address = gmail::profile(&ep, &access)?;
                    let threads =
                        gmail::list_threads(&ep, &access, &search, mail_watch::MAX_THREADS)?;
                    let fresh: Vec<Mailed> = mail_watch::fresh(&threads, &address, &seen)
                        .into_iter()
                        .map(|t| Mailed {
                            thread: t.id.clone(),
                            ticket: mail_watch::ticket(&t.id),
                            subject: t.subject.clone(),
                            from: t.from.clone(),
                            // The same in every copy of a group's email (spec §5.1).
                            key: mail_watch::claim_key(t),
                        })
                        .collect();
                    // One person per email (spec §5): only threads this person keeps run.
                    let claimed = crate::mail_claim::claim(
                        &ep,
                        &access,
                        &me,
                        &address,
                        swarm.as_deref(),
                        fresh,
                        &crate::action_log::now,
                        crate::mail_claim::SETTLE,
                    )?;
                    for m in &claimed.kept {
                        let messages = gmail::thread_messages(&ep, &access, &m.thread)?;
                        let dir = mail_watch::thread_folder(&root, &m.ticket);
                        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                        std::fs::write(dir.join("email.md"), mail_watch::email_md(&messages))
                            .map_err(|e| e.to_string())?;
                    }
                    Ok::<_, String>((address, claimed))
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.mail_looked(&p, &m, by_hand, found, window, cx)
            });
        })
        .detach();
    }

    fn mail_looked(
        &mut self,
        p: &Pipeline,
        m: &MailSettings,
        by_hand: bool,
        found: Result<(String, crate::mail_claim::Claimed), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mail_looking.remove(&p.file);
        let days = m.look_back_days;
        let root = self.hydra.paths.root().to_path_buf();
        let (me, claimed) = match found {
            Ok(f) => f,
            Err(e) => {
                crate::action_log::write(format!("mail {}: look failed: {e}", p.file));
                // Shown under Check in Pipeline settings (§89).
                let _ = crate::schedule::mark_run(
                    &root,
                    &found_key(&p.file),
                    &format!("couldn't look: {e}"),
                );
                window.push_note(
                    Note::error(format!("Couldn't look in the mailbox: {e}")).title(p.name.clone()),
                    cx,
                );
                return;
            }
        };
        let mut state = mail_watch::load(&root, &p.file);
        // Someone else's are never this person's to take.
        for (m, who) in claimed.theirs.iter().chain(&claimed.lost) {
            state.seen.insert(m.thread.clone());
            crate::action_log::write(format!("mail {}: {} is {who}'s", p.file, m.ticket));
        }
        let others = claimed.theirs.len() + claimed.lost.len();
        let new: Vec<Mailed> = claimed
            .kept
            .into_iter()
            .filter(|m| state.seen.insert(m.thread.clone()))
            .collect();
        for m in &new {
            state.held.insert(m.thread.clone(), m.ticket.clone());
            state.keys.insert(m.thread.clone(), m.key().to_string());
        }
        state.queue.extend(new.iter().cloned());
        if let Err(e) = mail_watch::save(&root, &p.file, &state) {
            crate::action_log::write(format!("mail {}: can't keep what it took: {e}", p.file));
        }
        // What it found, under Check in Pipeline settings (§89).
        let subjects: Vec<&str> = new.iter().map(|m| m.subject.as_str()).collect();
        if let Err(e) = crate::schedule::mark_run(
            &root,
            &found_key(&p.file),
            &mail_watch::found_line(&subjects, others),
        ) {
            crate::action_log::write(format!("mail {}: can't note what it found: {e}", p.file));
        }
        crate::action_log::write(format!(
            "mail {}: {me}: {} new: {}",
            p.file,
            new.len(),
            new.iter()
                .map(|m| m.ticket.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let others = if others == 0 {
            String::new()
        } else {
            format!(
                " {others} more {} someone else's.",
                if others == 1 { "is" } else { "are" }
            )
        };
        if !new.is_empty() {
            window.push_note(
                Note::info(format!(
                    "Claimed {} new email{} in {me} from the last {days} days: {}. The \
                     orchestrator starts them one at a time.{others}",
                    new.len(),
                    if new.len() == 1 { "" } else { "s" },
                    new.iter()
                        .map(|m| format!("\"{}\"", m.subject))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .title(p.name.clone()),
                cx,
            );
        } else if by_hand {
            window.push_note(
                Note::info(format!(
                    "Nothing new in {me} from the last {days} days: every thread is taken, \
                     claimed, done or answered.{others}"
                ))
                .title(p.name.clone()),
                cx,
            );
        }
        self.start_next_mail(p, m, window, cx);
        cx.notify();
    }

    /// The folder of the swarm called `name` on this machine.
    fn mail_swarm_folder(&self, name: &str) -> Option<std::path::PathBuf> {
        self.swarms
            .iter()
            .find(|s| !s.removed && s.name.eq_ignore_ascii_case(name))
            .map(|s| s.folder.clone())
    }

    /// Claims whose runs have ended are let go (spec §5): a finished one marked done, a
    /// stopped one freed for someone else. Off the UI thread; one release at a time per
    /// pipeline.
    fn release_mail(
        &mut self,
        p: &Pipeline,
        m: &MailSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mail_releasing.contains(&p.file) {
            return;
        }
        let root = self.hydra.paths.root().to_path_buf();
        let state = mail_watch::load(&root, &p.file);
        let (held, keys) = (state.held, state.keys);
        if held.is_empty() {
            return;
        }
        let history = crate::run_history::read(&root, &[]);
        let ended: Vec<(String, String, bool)> = held
            .iter()
            .filter_map(|(thread, ticket)| {
                let e = history
                    .iter()
                    .rfind(|e| e.ticket.eq_ignore_ascii_case(ticket) && e.ended)?;
                // Still running under a newer attempt: not yet.
                let open = history
                    .iter()
                    .any(|o| o.ticket.eq_ignore_ascii_case(ticket) && !o.ended);
                (!open).then(|| {
                    (
                        thread.clone(),
                        keys.get(thread).cloned().unwrap_or_else(|| thread.clone()),
                        e.outcome == crate::run_history::Outcome::Done,
                    )
                })
            })
            .collect();
        if ended.is_empty() {
            return;
        }
        let (Some(client), Some(env), Some(me)) = (
            self.ui.gmail_client(),
            self.mail_env(p, m, cx),
            self.ui.peer_name.as_deref().map(crate::mail_claim::slug),
        ) else {
            return;
        };
        let swarm = m.swarm.as_deref().and_then(|n| self.mail_swarm_folder(n));
        let hydra = self.hydra.clone();
        let file = p.file.clone();
        self.mail_releasing.insert(file.clone());
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    let ep = Endpoints::google();
                    let access = gmail::refresh(&ep, &client, &hydra.gmail_sign_in(&env)?)?;
                    let mut released = Vec::new();
                    for (thread, key, finished) in ended {
                        crate::mail_claim::release(
                            &ep,
                            &access,
                            &me,
                            swarm.as_deref(),
                            &thread,
                            &key,
                            finished,
                        )?;
                        released.push((thread, finished));
                    }
                    Ok::<_, String>(released)
                })
                .await;
            let _ = this.update_in(cx, |ws, _, _| {
                ws.mail_releasing.remove(&file);
                let root = ws.hydra.paths.root().to_path_buf();
                match done {
                    Ok(released) => {
                        let mut state = mail_watch::load(&root, &file);
                        for (thread, finished) in &released {
                            state.held.remove(thread);
                            state.keys.remove(thread);
                            crate::action_log::write(format!(
                                "mail {file}: let go of {thread} ({})",
                                if *finished { "done" } else { "freed" }
                            ));
                        }
                        if let Err(e) = mail_watch::save(&root, &file, &state) {
                            crate::action_log::write(format!("mail {file}: {e}"));
                        }
                    }
                    Err(e) => crate::action_log::write(format!("mail {file}: can't let go: {e}")),
                }
            });
        })
        .detach();
    }

    /// The next waiting thread to an orchestrator with room, once the one handed over before
    /// has become a run (or 5 minutes passed, when it's given up on and said so).
    fn start_next_mail(
        &mut self,
        p: &Pipeline,
        m: &MailSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((ticket, since)) = self.mail_inflight.get(&p.file).cloned() {
            let started = self
                .orchestrators
                .iter()
                .filter(|(_, f)| **f == p.file)
                .any(|(id, _)| {
                    self.open_runs_of(*id, cx)
                        .iter()
                        .any(|t| t.eq_ignore_ascii_case(&ticket))
                });
            if !started && since.elapsed() < START_WITHIN {
                return;
            }
            self.mail_inflight.remove(&p.file);
            if !started {
                crate::action_log::write(format!(
                    "mail {}: the orchestrator didn't start {ticket}",
                    p.file
                ));
                window.push_note(
                    Note::warning(format!(
                        "The orchestrator didn't start {ticket} within 5 minutes, so it was \
                         skipped. Its email is in hydraui/mail/{ticket}/email.md."
                    ))
                    .title(p.name.clone()),
                    cx,
                );
            }
        }
        let root = self.hydra.paths.root().to_path_buf();
        let mut state = mail_watch::load(&root, &p.file);
        if state.queue.is_empty() {
            return;
        }
        let Some(env) = self.mail_env(p, m, cx) else {
            return;
        };
        let open = self
            .orchestrators
            .iter()
            .filter(|(id, f)| {
                **f == p.file && self.panes.get(id).is_some_and(|v| v.read(cx).env() == env)
            })
            .map(|(id, _)| *id)
            .min();
        let slot = match open {
            Some(o) => match self.slot_for_work(o, window, cx) {
                Some(slot) => slot,
                // Every run is busy: the next starts when one ends.
                None => return,
            },
            None => self.open_orchestrator_slot(&env, &p.file, window, cx),
        };
        let Some(next) = state.queue.pop_front() else {
            return;
        };
        if let Err(e) = mail_watch::save(&root, &p.file, &state) {
            crate::action_log::write(format!("mail {}: can't keep its queue: {e}", p.file));
        }
        let email = mail_watch::thread_folder(&root, &next.ticket).join("email.md");
        let prompt = mail_watch::start_prompt(p, &next, &email);
        self.mail_inflight
            .insert(p.file.clone(), (next.ticket.clone(), Instant::now()));
        self.pending_wakes
            .insert(slot, (next.ticket.clone(), prompt));
        self.send_wakes(window, cx);
        crate::action_log::write(format!("mail {}: handed {} to {env}", p.file, next.ticket));
    }
}
