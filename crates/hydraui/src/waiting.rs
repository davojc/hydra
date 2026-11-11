//! While a stage waits (core spec §60): on a person, on a pull request's author, or on product,
//! its session is woken on the pipeline's `[waiting]` timetable to look again. One rule for every
//! pipeline. Pure: the app keeps the times.

use std::time::{Duration, SystemTime};

use crate::pipeline::WaitingSettings;

/// What a waiting stage does at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Not time yet.
    NotYet,
    /// Look again now.
    Now,
    /// It has looked for as long as the pipeline says: no more looks.
    Over,
}

/// Whether a stage waiting since `since`, last told to look at `last`, looks again at `now`.
pub fn look(
    s: &WaitingSettings,
    since: SystemTime,
    last: Option<SystemTime>,
    now: SystemTime,
) -> Look {
    if s.look_every == 0 {
        return Look::NotYet;
    }
    if s.look_for > 0
        && now
            .duration_since(since)
            .is_ok_and(|w| w >= Duration::from_secs(u64::from(s.look_for) * 3600))
    {
        return Look::Over;
    }
    let every = Duration::from_secs(u64::from(s.look_every) * 60);
    if now
        .duration_since(last.unwrap_or(since))
        .is_ok_and(|waited| waited >= every)
    {
        Look::Now
    } else {
        Look::NotYet
    }
}

/// Whole minutes until the next look (rounded up), for the card: `0` when one is due.
pub fn next_in(
    s: &WaitingSettings,
    since: SystemTime,
    last: Option<SystemTime>,
    now: SystemTime,
) -> u64 {
    let every = Duration::from_secs(u64::from(s.look_every) * 60);
    let waited = now
        .duration_since(last.unwrap_or(since))
        .unwrap_or_default();
    every.saturating_sub(waited).as_secs().div_ceil(60)
}

/// What a session waiting on a person is told each time it looks. The same for every pipeline;
/// the session's own prompt says what its stage depends on.
pub fn needs_you_prompt(waiting_for: &str) -> String {
    format!(
        "You're waiting on a person: {waiting_for}. Look again at what your stage depends on, as \
         your prompt describes it (a pull request, a ticket, an environment, a thread). If \
         something changed that settles it, do what your stage needs again and call stage_report: \
         that report counts. If nothing changed, say so in one line and end your turn without \
         reporting: you'll be asked to look again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::WaitThen;

    fn min(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    fn every(look_every: u32, look_for: u32) -> WaitingSettings {
        WaitingSettings {
            look_every,
            look_for,
            needs_you: true,
            then: WaitThen::Stop,
        }
    }

    #[test]
    fn it_looks_on_its_timetable_until_its_hours_run_out() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let s = every(5, 0);
        assert_eq!(look(&s, t0, None, t0 + min(4)), Look::NotYet);
        assert_eq!(look(&s, t0, None, t0 + min(5)), Look::Now);
        assert_eq!(look(&s, t0, Some(t0 + min(5)), t0 + min(9)), Look::NotYet);
        assert_eq!(look(&s, t0, Some(t0 + min(5)), t0 + min(10)), Look::Now);
        // Until someone acts: still looking a day later.
        assert_eq!(
            look(&s, t0, Some(t0 + min(1435)), t0 + min(1440)),
            Look::Now
        );
        // Up to 2 hours: then it's over, whatever the last look.
        let s = every(5, 2);
        assert_eq!(
            look(&s, t0, Some(t0 + min(115)), t0 + min(119)),
            Look::NotYet
        );
        assert_eq!(look(&s, t0, Some(t0 + min(115)), t0 + min(120)), Look::Over);
        // Never looks.
        assert_eq!(look(&every(0, 0), t0, None, t0 + min(600)), Look::NotYet);
        assert_eq!(next_in(&s, t0, Some(t0 + min(5)), t0 + min(7)), 3);
    }

    #[test]
    fn the_look_says_what_to_do_and_not_to_report_for_nothing() {
        let p = needs_you_prompt("Approve PR #5 on GitHub as a code owner");
        assert!(p.contains("Approve PR #5 on GitHub as a code owner"), "{p}");
        assert!(p.contains("stage_report"), "{p}");
        assert!(p.contains("without reporting"), "{p}");
    }
}
