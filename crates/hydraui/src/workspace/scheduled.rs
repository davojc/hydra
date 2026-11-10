//! Scheduled pipelines starting their runs (core spec §58): looked at every 30 seconds.

use gpui::{Context, Window};

use super::Workspace;
use crate::inbox::{Note, PushNote};

impl Workspace {
    /// Starts each scheduled pipeline's run that is due: marked first, so one that can't start
    /// isn't tried again every 30 seconds, then handed to its orchestrator as a Tickets pick
    /// is (a free one of the pipeline's in that environment, else a new one).
    pub(super) fn schedule_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .schedule_checked
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(30))
        {
            return;
        }
        self.schedule_checked = Some(std::time::Instant::now());
        let root = self.hydra.paths.root().to_path_buf();
        let now = crate::action_log::now();
        let Some(date) = now.split(' ').next().map(str::to_string) else {
            return;
        };
        let last = crate::schedule::last_runs(&root);
        let envs = self.hydra.env_names();
        for file in crate::pipeline::names(&root) {
            let Ok(p) = crate::pipeline::load(&root, &file) else {
                continue;
            };
            let Some((sc, when)) = p.schedule.as_ref().and_then(|s| Some((s, s.when()?))) else {
                continue;
            };
            if !when.due(&now, last.get(&file).map(String::as_str)) {
                continue;
            }
            if let Err(e) = crate::schedule::mark_run(&root, &file, &date) {
                crate::action_log::write(format!("schedule {file}: can't note the run: {e}"));
                continue;
            }
            let running = self.orchestrators.iter().find_map(|(id, f)| {
                (*f == file).then(|| self.panes.get(id).map(|v| v.read(cx).env().to_string()))?
            });
            let Some(env) = crate::schedule::pick_env(
                sc.env.as_deref(),
                p.autostart.as_deref(),
                running.as_deref(),
                self.ui.default_env.as_deref(),
                &envs,
            ) else {
                window.push_note(
                    Note::error(format!(
                        "{} is due ({}), but there's no environment to run it in.",
                        p.name,
                        when.text()
                    ))
                    .title("Scheduled run"),
                    cx,
                );
                continue;
            };
            let open = self
                .orchestrators
                .iter()
                .filter(|(id, f)| {
                    **f == file && self.panes.get(id).is_some_and(|v| v.read(cx).env() == env)
                })
                .map(|(id, _)| *id)
                .min();
            let slot = open
                .and_then(|o| self.slot_for_work(o, window, cx))
                .unwrap_or_else(|| self.open_orchestrator_slot(&env, &file, window, cx));
            let ticket = crate::schedule::ticket(&file, &date);
            let prompt = crate::schedule::start_prompt(&p.name, &file, &when, &ticket, &date);
            self.pending_wakes.insert(slot, (ticket.clone(), prompt));
            self.send_wakes(window, cx);
            crate::action_log::write(format!("schedule: {ticket} handed to {file} in {env}"));
            window.push_note(
                Note::info(format!(
                    "{} started its scheduled run ({}) as {ticket} in {env}.",
                    p.name,
                    when.text()
                ))
                .title("Scheduled run"),
                cx,
            );
        }
    }
}
