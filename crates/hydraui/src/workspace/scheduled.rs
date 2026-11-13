//! Scheduled pipelines starting their runs (core spec §58): looked at every 30 seconds.

use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use gpui_component::v_flex;

use super::Workspace;
use crate::envs::PaneId;
use crate::inbox::{Note, PushNote};
use crate::orchestrator_theme::{self as theme, color};

/// What a scheduled pipeline's row under its card says: when it runs, and how long until the
/// next run (or that it's starting now).
pub(super) fn schedule_line(when: &crate::schedule::When, wait: crate::schedule::Wait) -> String {
    match wait.left {
        0 => format!("{} · starting its run now", when.text()),
        left => format!(
            "{} · next run in {}",
            when.text(),
            crate::schedule::countdown(left)
        ),
    }
}

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
        // `[mail]` pipelines look on their own timetables (email triage spec §3).
        self.mail_tick(false, window, cx);
        // Pipelines imported from a swarm look for a newer version there (core spec §70).
        self.pipeline_updates_tick(window, cx);
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
            if let Err(e) = crate::schedule::mark_run(&root, &file, &now) {
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

    /// The row under a scheduled pipeline's card (core spec §79): when it runs, the time to the
    /// next run, and a bar filling from the run before to the next, like a trigger's wait for
    /// its next look. Only on the card holding the pipeline's trigger; `None` without a
    /// `[schedule]`.
    pub(super) fn render_schedule_row(
        &self,
        orch: PaneId,
        accent: u32,
        cx: &gpui::App,
    ) -> Option<AnyElement> {
        if self.lead_of(orch, cx) != orch {
            return None;
        }
        let file = self.orchestrators.get(&orch)?;
        let root = self.hydra.paths.root();
        let pipeline = crate::pipeline::load(root, file).ok()?;
        let when = pipeline.schedule.as_ref()?.when()?;
        let last = crate::schedule::last_runs(root);
        let wait = when.wait(
            &crate::action_log::now(),
            last.get(file).map(String::as_str),
        )?;
        let started = SystemTime::now().checked_sub(Duration::from_secs(wait.since));
        Some(
            v_flex()
                .ml(px(26.))
                .mr_1()
                .my_1()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(schedule_line(&when, wait)),
                )
                .child(super::linear::look_bar(
                    SharedString::from(format!("schedule-bar-{orch}")),
                    wait.progress(),
                    started,
                    Duration::from_secs(wait.since + wait.left),
                    false,
                    accent,
                ))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_says_when_it_runs_and_how_long_until_the_next() {
        let when = crate::schedule::parse("thu 19:26").unwrap();
        let wait = |since, left| crate::schedule::Wait { since, left };
        assert_eq!(
            schedule_line(&when, wait(60, 8 * 3600)),
            "Thursdays at 19:26 · next run in 8 hours"
        );
        assert_eq!(
            schedule_line(&when, wait(60, 30)),
            "Thursdays at 19:26 · next run in 30 seconds"
        );
        assert_eq!(
            schedule_line(&when, wait(10, 0)),
            "Thursdays at 19:26 · starting its run now"
        );
    }
}
