//! Claims: one machine in a swarm takes a job so no other machine does it too. A context is a
//! named kind of job, and an owner sets its rules in the roster.
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The longest a context's name may be.
const MAX_CONTEXT_NAME: usize = 64;

/// Whether `name` can name a context: 1 to 64 of a-z, 0-9, `-` and `_`.
pub fn valid_context_name(name: &str) -> bool {
    (1..=MAX_CONTEXT_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// How claims in one context behave. An owner sets them in the roster.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ContextSettings {
    /// How often a holder renews a claim, in seconds.
    pub renew_secs: u32,
    /// How many renewals a holder may miss before the claim lapses.
    pub lapse_after: u32,
    /// How many claims one machine may hold at once: -1 is no limit, and 0 pauses the context
    /// (claims already held run on).
    pub max_per_machine: i32,
    /// How long a finished claim is kept, in seconds.
    pub keep_done_secs: u64,
}

impl Default for ContextSettings {
    fn default() -> Self {
        ContextSettings {
            renew_secs: 60,
            lapse_after: 3,
            max_per_machine: 1,
            keep_done_secs: 30 * 24 * 60 * 60,
        }
    }
}

impl ContextSettings {
    /// How long, in microseconds, a claim may go without a renewal before it lapses: the missed
    /// renewals allowed, plus `slack` for clocks and sync that run late.
    pub fn lapse_micros(&self, slack: Duration) -> u64 {
        let secs = u64::from(self.renew_secs) * u64::from(self.lapse_after);
        secs.saturating_mul(1_000_000)
            .saturating_add(slack.as_micros().min(u128::from(u64::MAX)) as u64)
    }

    /// The settings with anything out of range brought to the nearest value that works, so a
    /// bad change can't stop claims from ever lapsing or being kept.
    pub fn normalised(self) -> Self {
        ContextSettings {
            renew_secs: self.renew_secs.max(1),
            lapse_after: self.lapse_after.max(1),
            max_per_machine: self.max_per_machine.max(-1),
            keep_done_secs: self.keep_done_secs.max(60),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_specs() {
        let d = ContextSettings::default();
        assert_eq!(
            (
                d.renew_secs,
                d.lapse_after,
                d.max_per_machine,
                d.keep_done_secs
            ),
            (60, 3, 1, 2_592_000)
        );
        assert_eq!(d.lapse_micros(Duration::from_secs(30)), 210_000_000);
    }

    #[test]
    fn settings_out_of_range_are_raised_to_the_nearest_that_works() {
        let n = ContextSettings {
            renew_secs: 0,
            lapse_after: 0,
            max_per_machine: -5,
            keep_done_secs: 1,
        }
        .normalised();
        assert_eq!(
            (
                n.renew_secs,
                n.lapse_after,
                n.max_per_machine,
                n.keep_done_secs
            ),
            (1, 1, -1, 60)
        );
        let paused = ContextSettings {
            max_per_machine: 0,
            ..Default::default()
        };
        assert_eq!(paused.normalised(), paused);
    }

    #[test]
    fn context_names_are_short_and_plain() {
        assert!(valid_context_name("inbox"));
        assert!(valid_context_name("a-b_9"));
        assert!(valid_context_name(&"a".repeat(64)));
        assert!(!valid_context_name(""));
        assert!(!valid_context_name(&"a".repeat(65)));
        assert!(!valid_context_name("Has Space"));
        assert!(!valid_context_name("Upper"));
        assert!(!valid_context_name("dot.ted"));
    }
}
