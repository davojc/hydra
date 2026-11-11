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

impl Workspace {
    /// Each `[mail]` pipeline: a look when its time has come (every one when `now`), then the
    /// next waiting thread to its orchestrator once the last one handed over has started.
    pub(super) fn mail_tick(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.hydra.paths.root().to_path_buf();
        let at = crate::action_log::now();
        let Some(date) = at.split(' ').next().map(str::to_string) else {
            return;
        };
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
            let due = now
                || m.when()
                    .is_some_and(|w| w.due(&at, last.get(&look_key(&file)).map(String::as_str)));
            if due && !self.mail_looking.contains(&file) {
                if let Err(e) = crate::schedule::mark_run(&root, &look_key(&file), &date) {
                    crate::action_log::write(format!("mail {file}: can't note the look: {e}"));
                    continue;
                }
                self.start_mail_look(&p, &m, now, window, cx);
            }
            self.start_next_mail(&p, &m, window, cx);
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
    fn mail_env(&self, p: &Pipeline, m: &MailSettings, cx: &gpui::App) -> Option<String> {
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
                    let me = gmail::profile(&ep, &access)?;
                    let threads =
                        gmail::list_threads(&ep, &access, &search, mail_watch::MAX_THREADS)?;
                    let mut out = Vec::new();
                    for t in mail_watch::fresh(&threads, &me, &seen) {
                        let messages = gmail::thread_messages(&ep, &access, &t.id)?;
                        let ticket = mail_watch::ticket(&t.id);
                        let dir = mail_watch::thread_folder(&root, &ticket);
                        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                        std::fs::write(dir.join("email.md"), mail_watch::email_md(&messages))
                            .map_err(|e| e.to_string())?;
                        out.push(Mailed {
                            thread: t.id.clone(),
                            ticket,
                            subject: t.subject.clone(),
                            from: t.from.clone(),
                        });
                    }
                    Ok::<_, String>((me, out))
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
        found: Result<(String, Vec<Mailed>), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mail_looking.remove(&p.file);
        let days = m.look_back_days;
        let root = self.hydra.paths.root().to_path_buf();
        let (me, new) = match found {
            Ok(f) => f,
            Err(e) => {
                crate::action_log::write(format!("mail {}: look failed: {e}", p.file));
                window.push_note(
                    Note::error(format!("Couldn't look in the mailbox: {e}")).title(p.name.clone()),
                    cx,
                );
                return;
            }
        };
        let mut state = mail_watch::load(&root, &p.file);
        let new: Vec<Mailed> = new
            .into_iter()
            .filter(|m| state.seen.insert(m.thread.clone()))
            .collect();
        state.queue.extend(new.iter().cloned());
        if let Err(e) = mail_watch::save(&root, &p.file, &state) {
            crate::action_log::write(format!("mail {}: can't keep what it took: {e}", p.file));
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
        if !new.is_empty() {
            window.push_note(
                Note::info(format!(
                    "{} new email{} in {me} from the last {days} days: {}. The orchestrator \
                     starts them one at a time.",
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
                     claimed, done or answered."
                ))
                .title(p.name.clone()),
                cx,
            );
        }
        self.start_next_mail(p, m, window, cx);
        cx.notify();
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
