//! A stage's question for product (a decision that isn't the stage's to make): when hydraui
//! has the waiting session look for the answer in its Slack thread, and what it types to it.

use std::time::{Duration, SystemTime};

/// How often a waiting session is asked to look in its Slack thread.
pub const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);

/// Whether the session asked at `since`, last told to look at `last`, should look again.
pub fn check_due(since: SystemTime, last: Option<SystemTime>, now: SystemTime) -> bool {
    now.duration_since(last.unwrap_or(since))
        .is_ok_and(|waited| waited >= CHECK_EVERY)
}

/// Whole minutes until the next look (rounded up), for the card: `0` when one is due.
pub fn next_check_in(since: SystemTime, last: Option<SystemTime>, now: SystemTime) -> u64 {
    let waited = now
        .duration_since(last.unwrap_or(since))
        .unwrap_or_default();
    CHECK_EVERY.saturating_sub(waited).as_secs().div_ceil(60)
}

/// Typed to the waiting session to have it look for the answer.
pub fn check_prompt(question: &str, link: &str) -> String {
    format!(
        "Look in the Slack thread {link} for an answer to your question: {question} If it's \
         answered, call stage_answer with the answer and who gave it, then carry on. If someone \
         replied with a question of their own or wants something explained, and nobody has \
         answered them yet, reply to them in that thread (plainly, the way the question was \
         written), then stop: the decision is still theirs. Otherwise say in one line that \
         there's no answer yet, and stop: you'll be asked again."
    )
}

/// Typed to the waiting session when the user answers in hydraui.
pub fn answer_prompt(answer: &str) -> String {
    format!(
        "The answer to your question, from the user: {answer} It's recorded; carry on with your \
         stage as it says (don't call stage_answer)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_session_looks_every_five_minutes() {
        let asked = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        let min = |m: u64| Duration::from_secs(m * 60);
        assert!(!check_due(asked, None, asked + min(4)));
        assert!(check_due(asked, None, asked + min(5)));
        assert!(!check_due(asked, Some(asked + min(5)), asked + min(9)));
        assert!(check_due(asked, Some(asked + min(5)), asked + min(10)));
        assert_eq!(
            next_check_in(asked, None, asked + Duration::from_secs(61)),
            4
        );
        assert_eq!(
            next_check_in(asked, Some(asked + min(5)), asked + min(9)),
            1
        );
        assert_eq!(next_check_in(asked, None, asked + min(7)), 0);
    }

    #[test]
    fn the_prompts_say_what_to_do_with_the_answer() {
        let check = check_prompt("Remove auto-translate?", "https://s/t/1");
        assert!(
            check.contains("https://s/t/1") && check.contains("stage_answer"),
            "{check}"
        );
        assert!(check.contains("reply to them in that thread"), "{check}");
        let answer = answer_prompt("Keep it");
        assert!(answer.contains("Keep it") && answer.contains("don't call stage_answer"));
    }
}
