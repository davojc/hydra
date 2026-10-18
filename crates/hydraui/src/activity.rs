//! What a terminal is doing, for the tab strip: working (its output changed lately), waiting
//! (a command running but silent), or idle. Works for any program. No UI.
use std::time::{Duration, Instant};

use hydraui_term::PromptState;

/// How long a running program can be silent and still count as working.
pub const QUIET: Duration = Duration::from_secs(3);

/// Ordered: a tab shows its busiest pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Activity {
    /// At the prompt, or the program has exited.
    #[default]
    Idle,
    /// Running, but silent for QUIET or longer: possibly waiting for you.
    Waiting,
    /// Running, with output in the last QUIET.
    Working,
}

/// A terminal's activity: `running` is false once its shell has exited; `prompt` is the last
/// shell-integration mark (`None` without integration); `last_output` when output last came.
/// Any recent output is work, whatever is running; a command that has gone quiet is waiting.
pub fn activity(
    running: bool,
    prompt: Option<PromptState>,
    last_output: Option<Instant>,
    now: Instant,
) -> Activity {
    let recent = last_output.is_some_and(|t| now.saturating_duration_since(t) < QUIET);
    match (running, prompt) {
        (false, _) => Activity::Idle,
        (true, _) if recent => Activity::Working,
        (true, Some(PromptState::CommandRunning)) => Activity::Waiting,
        (true, _) => Activity::Idle,
    }
}

/// The busiest of a tab's panes.
pub fn busiest(panes: impl IntoIterator<Item = Activity>) -> Activity {
    panes.into_iter().max().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> (Instant, Instant) {
        let start = Instant::now();
        (start, start + Duration::from_secs(secs))
    }

    #[test]
    fn a_running_command_works_then_waits() {
        let (out, now) = at(1);
        let run = Some(PromptState::CommandRunning);
        assert_eq!(activity(true, run, Some(out), now), Activity::Working);
        let (out, now) = at(3);
        assert_eq!(
            activity(true, run, Some(out), now),
            Activity::Waiting,
            "3 s of silence"
        );
        assert_eq!(
            activity(true, run, None, Instant::now()),
            Activity::Waiting,
            "no output yet"
        );
    }

    #[test]
    fn any_recent_output_is_work_whatever_is_running() {
        let (out, now) = at(1);
        for prompt in [
            Some(PromptState::AtPrompt),
            Some(PromptState::CommandDone(Some(0))),
            None,
        ] {
            assert_eq!(
                activity(true, prompt, Some(out), now),
                Activity::Working,
                "{prompt:?}"
            );
        }
    }

    #[test]
    fn a_quiet_prompt_and_an_exited_program_are_idle() {
        let (out, now) = at(5);
        assert_eq!(
            activity(true, Some(PromptState::AtPrompt), Some(out), now),
            Activity::Idle
        );
        assert_eq!(
            activity(
                true,
                Some(PromptState::CommandDone(Some(0))),
                Some(out),
                now
            ),
            Activity::Idle
        );
        assert_eq!(
            activity(true, None, Some(out), now),
            Activity::Idle,
            "no integration"
        );
        let (out, now) = at(0);
        assert_eq!(
            activity(false, Some(PromptState::CommandRunning), Some(out), now),
            Activity::Idle,
            "exited"
        );
    }

    #[test]
    fn a_tab_shows_its_busiest_pane() {
        use Activity::*;
        assert_eq!(busiest([Idle, Waiting, Idle]), Waiting);
        assert_eq!(busiest([Waiting, Working]), Working);
        assert_eq!(busiest([]), Idle);
    }
}
