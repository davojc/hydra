//! Parking (core spec §54): an orchestrator whose open runs have all sat with nothing changing
//! for the hours config sets (`park_idle_hours`) closes, its runs set aside to resume later
//! from History or the Pipelines tab, rather than polling on for hours.

use crate::runs::{Run, RunState};

/// Hours with nothing changing before an orchestrator is parked, when config doesn't say.
pub const DEFAULT_HOURS: u32 = 4;

/// The choices History offers; 0 is never.
pub const CHOICES: [u32; 6] = [0, 1, 2, 4, 8, 12];

/// The log time of the last line of run log `log`.
pub fn last_at(log: &str) -> Option<String> {
    log.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|e| e.get("at")?.as_str().map(str::to_string))
}

/// Whether run `r`, whose log last changed at `last`, has sat `hours` with nothing changing at
/// log time `now`. Only a run at work counts: one waiting on a person, on an answer to its
/// question, or paused by a person is waiting on purpose, and parking it would drop what it
/// waits for.
pub fn idle(r: &Run, last: &str, now: &str, hours: u32) -> bool {
    hours > 0
        && r.state == RunState::Running
        && !r.paused
        && r.asking.is_none()
        && crate::run_feed::before(now, u64::from(hours) * 3_600)
            .is_some_and(|cut| last <= cut.as_str())
}

/// `4 hours`, `1 hour`.
pub fn hours_text(hours: u32) -> String {
    format!("{hours} hour{}", if hours == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::{Ask, Whom};

    /// A run at work, as a restart reads it back from its log.
    fn run() -> Run {
        let home = tempfile::tempdir().unwrap();
        let dir = crate::pipeline::folder(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("one.toml"),
            "name = \"One\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n",
        )
        .unwrap();
        let file = crate::run_log::file_for(home.path(), "PRE-1-1");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let line = |event: &str, rest: &str| {
            format!(r#"{{"at":"2026-11-10 09:00:00.000","event":"{event}"{rest}}}"#)
        };
        let log = [
            line(
                "run started",
                r#","ticket":"PRE-1","summary":"S","pipeline":"one""#,
            ),
            line("orchestrator", r#","env":"e","folder":"/w","pid":1"#),
            line("claude session", r#","who":"orchestrator","id":"c""#),
        ];
        std::fs::write(file, log.join("\n") + "\n").unwrap();
        crate::run_resume::interrupted(home.path(), |_| false)
            .remove(0)
            .run
    }

    const NOW: &str = "2026-11-10 13:00:00.000";

    #[test]
    fn last_at_is_the_last_line_with_a_time() {
        let log = "{\"at\":\"2026-11-10 09:00:00.000\",\"event\":\"run started\"}\n\
                   {\"at\":\"2026-11-10 10:30:00.000\",\"event\":\"stage passed\"}\n\
                   not json\n";
        assert_eq!(last_at(log).as_deref(), Some("2026-11-10 10:30:00.000"));
        assert_eq!(last_at(""), None);
    }

    #[test]
    fn a_running_run_parks_once_its_hours_pass() {
        let r = run();
        assert!(idle(&r, "2026-11-10 09:00:00.000", NOW, 4));
        assert!(!idle(&r, "2026-11-10 09:00:00.001", NOW, 4));
        assert!(!idle(&r, "2026-11-10 12:00:00.000", NOW, 4));
    }

    #[test]
    fn never_parks_when_hours_are_zero() {
        assert!(!idle(&run(), "2026-11-01 00:00:00.000", NOW, 0));
    }

    #[test]
    fn a_run_waiting_on_purpose_never_parks() {
        let old = "2026-11-01 00:00:00.000";
        let mut r = run();
        r.state = RunState::NeedsYou("scored 70".into());
        assert!(!idle(&r, old, NOW, 4));
        let mut r = run();
        r.paused = true;
        assert!(!idle(&r, old, NOW, 4));
        let mut r = run();
        r.asking = Some(Ask {
            stage: 0,
            whom: Whom::default(),
            question: "Which?".into(),
            link: None,
            since: std::time::SystemTime::now(),
        });
        assert!(!idle(&r, old, NOW, 4));
        let mut r = run();
        r.state = RunState::Finished;
        assert!(!idle(&r, old, NOW, 4));
    }

    #[test]
    fn hours_read_as_words() {
        assert_eq!(hours_text(1), "1 hour");
        assert_eq!(hours_text(4), "4 hours");
    }
}
