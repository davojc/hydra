//! Whether a terminal's Claude session is ready for a new prompt, from what hydraui saw: a
//! draft typed into its input (or a prompt hydraui typed that hasn't started), and whether its
//! last turn clearly finished (a fresh session, or one that just moved its card to Verify). Pure, so the rules are tested directly.
use std::time::{Duration, Instant};

use crate::mcp::SessionState;

/// Two Escapes this close together clear Claude's input.
const DOUBLE_ESCAPE: Duration = Duration::from_millis(500);

/// What is in the session's input box, as far as the keys typed say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Draft {
    /// Nothing typed since the last prompt was sent.
    #[default]
    Clear,
    /// Something was typed and not sent.
    Typed,
    /// Something was typed, then a key that may have cleared it (Esc Esc, Ctrl+C): the screen
    /// decides.
    MaybeCleared,
}

/// One terminal's readiness.
#[derive(Debug, Clone, Default)]
pub struct Readiness {
    pub draft: Draft,
    /// The session's last turn clearly finished: it is new, or it just verified a card.
    pub finished: bool,
    /// The session resumes a conversation (layout restore), so its next start is not fresh.
    resumed: bool,
    last_escape: Option<Instant>,
}

impl Readiness {
    /// For a terminal whose Claude session is a resumed conversation (layout restore), which
    /// may end in a question: its start doesn't count as fresh.
    pub fn resumed() -> Readiness {
        Readiness {
            resumed: true,
            ..Readiness::default()
        }
    }

    /// A printable or editing key, IME text or a paste.
    pub fn typed(&mut self) {
        self.draft = Draft::Typed;
    }

    /// An unmodified Enter: the draft was sent.
    pub fn enter(&mut self) {
        self.draft = Draft::Clear;
    }

    /// Escape at `now`; a second one within 500 ms may have cleared a draft.
    pub fn escape(&mut self, now: Instant) {
        let double = self
            .last_escape
            .is_some_and(|first| now.saturating_duration_since(first) <= DOUBLE_ESCAPE);
        if double {
            self.last_escape = None;
            if self.draft == Draft::Typed {
                self.draft = Draft::MaybeCleared;
            }
        } else {
            self.last_escape = Some(now);
        }
    }

    /// Ctrl+C, which clears Claude's input when it has some.
    pub fn ctrl_c(&mut self) {
        if self.draft == Draft::Typed {
            self.draft = Draft::MaybeCleared;
        }
    }

    /// The session's hooks moved it from `before` to `after`.
    pub fn state(&mut self, before: Option<SessionState>, after: SessionState) {
        match (before, after) {
            (_, SessionState::Ended) => *self = Readiness::default(),
            (None, SessionState::Idle) => {
                // A new session: fresh, unless it resumed a conversation.
                self.finished = !self.resumed;
                self.resumed = false;
                self.draft = Draft::Clear;
            }
            (Some(SessionState::Idle), SessionState::Working) => {
                self.draft = Draft::Clear;
                self.finished = false;
            }
            _ => {}
        }
    }

    /// The session's hooks said it started by resuming a conversation (`claude -c`, `claude -r`,
    /// `/resume`): it is idle, but its last turn may have ended in a question, so it is not
    /// fresh.
    pub fn resumed_start(&mut self, before: Option<SessionState>) {
        self.resumed = true;
        self.state(before, SessionState::Idle);
        self.resumed = false;
        self.finished = false;
    }

    /// hydraui typed a prompt into the session (auto-pickup). Until the session starts working
    /// on it (or ends), it counts as a draft: a prompt that didn't submit still sits in the
    /// input, so the session isn't free.
    pub fn prompt_typed(&mut self) {
        self.draft = Draft::Typed;
    }

    /// The session moved a card to Verify: its turn is clearly finished.
    pub fn verified(&mut self) {
        self.finished = true;
    }

    /// What the screen says about the input box (`claude_input_empty`).
    pub fn screen_says_empty(&mut self, empty: Option<bool>) {
        if self.draft == Draft::MaybeCleared && empty == Some(true) {
            self.draft = Draft::Clear;
        }
    }

    /// The input may hold something the user typed.
    pub fn has_draft(&self) -> bool {
        matches!(self.draft, Draft::Typed | Draft::MaybeCleared)
    }
}

/// A row that is a rule: only `─` after leading and trailing spaces.
fn is_rule(row: &[(char, bool)]) -> bool {
    let mut chars = row.iter().map(|&(c, _)| c).filter(|&c| c != ' ');
    chars.next() == Some('─') && chars.all(|c| c == '─')
}

/// The prompt character's position when the row is an input line (`>` or `❯` first).
fn prompt_at(row: &[(char, bool)]) -> Option<usize> {
    let at = row.iter().position(|&(c, _)| c != ' ')?;
    matches!(row[at].0, '>' | '❯').then_some(at)
}

/// Whether Claude's input line on screen is empty. `rows` are the screen's rows as
/// `(char, dim)` cells. The input line is the last row starting with `>` or `❯` right below a
/// rule of `─`, with another rule further down; the rows up to that rule are the rest of a
/// draft. Dim text (a placeholder) doesn't count. `None` when there is no such line.
pub fn claude_input_empty(rows: &[Vec<(char, bool)>]) -> Option<bool> {
    let blank = |cells: &[(char, bool)]| cells.iter().all(|&(c, dim)| dim || c == ' ');
    (1..rows.len()).rev().find_map(|i| {
        let at = prompt_at(&rows[i])?;
        if !is_rule(&rows[i - 1]) {
            return None;
        }
        let end = i + 1 + rows[i + 1..].iter().position(|r| is_rule(r))?;
        Some(blank(&rows[i][at + 1..]) && rows[i + 1..end].iter().all(|r| blank(r)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionState::{Idle, Working};

    fn typed() -> Readiness {
        let mut r = Readiness::default();
        r.typed();
        r
    }

    #[test]
    fn typing_sets_a_draft_and_enter_clears_it() {
        let mut r = Readiness::default();
        assert!(!r.has_draft());
        r.typed();
        assert_eq!(r.draft, Draft::Typed);
        assert!(r.has_draft());
        r.enter();
        assert_eq!(r.draft, Draft::Clear);
        assert!(!r.has_draft());
    }

    #[test]
    fn one_escape_keeps_the_draft_two_quick_ones_may_clear_it() {
        let t0 = Instant::now();
        let mut r = typed();
        r.escape(t0);
        assert_eq!(r.draft, Draft::Typed);
        r.escape(t0 + Duration::from_millis(300));
        assert_eq!(r.draft, Draft::MaybeCleared);
        assert!(r.has_draft());

        let mut slow = typed();
        slow.escape(t0);
        slow.escape(t0 + Duration::from_millis(900));
        assert_eq!(slow.draft, Draft::Typed);

        let mut none = Readiness::default();
        none.escape(t0);
        none.escape(t0 + Duration::from_millis(100));
        assert_eq!(none.draft, Draft::Clear, "nothing to clear");

        let mut c = typed();
        c.ctrl_c();
        assert_eq!(c.draft, Draft::MaybeCleared);
    }

    #[test]
    fn maybe_cleared_needs_the_screen_to_say_empty() {
        let mut r = typed();
        r.ctrl_c();
        r.screen_says_empty(None);
        assert_eq!(r.draft, Draft::MaybeCleared);
        r.screen_says_empty(Some(false));
        assert_eq!(r.draft, Draft::MaybeCleared);
        r.screen_says_empty(Some(true));
        assert_eq!(r.draft, Draft::Clear);

        let mut t = typed();
        t.screen_says_empty(Some(true));
        assert_eq!(
            t.draft,
            Draft::Typed,
            "only a maybe is settled by the screen"
        );
    }

    #[test]
    fn a_prompt_starting_clears_the_draft_and_finished() {
        let mut r = typed();
        r.finished = true;
        r.state(Some(Idle), Working);
        assert_eq!(r.draft, Draft::Clear);
        assert!(!r.finished);
    }

    #[test]
    fn stop_and_tool_use_keep_type_ahead() {
        let mut r = typed();
        r.state(Some(Working), Working);
        assert_eq!(r.draft, Draft::Typed);
        r.state(Some(Working), Idle);
        assert_eq!(r.draft, Draft::Typed);
    }

    #[test]
    fn a_new_session_is_fresh() {
        let mut r = typed();
        r.state(None, Idle);
        assert!(r.finished);
        assert_eq!(r.draft, Draft::Clear);
        r.state(Some(Idle), SessionState::Ended);
        assert!(!r.finished);
        assert_eq!(r.draft, Draft::Clear);
    }

    #[test]
    fn a_resumed_session_is_not_fresh() {
        let mut r = Readiness::resumed();
        r.state(None, Idle);
        assert!(!r.finished);
        r.state(Some(Idle), SessionState::Ended);
        r.state(None, Idle);
        assert!(r.finished, "a later new session in the terminal is fresh");
    }

    #[test]
    fn verify_marks_finished_until_the_next_prompt() {
        let mut r = Readiness::default();
        r.state(None, Idle);
        r.state(Some(Idle), Working);
        assert!(!r.finished);
        r.verified();
        assert!(r.finished);
        r.state(Some(Working), Working);
        r.state(Some(Working), Idle);
        assert!(r.finished);
        r.state(Some(Idle), Working);
        assert!(!r.finished);
    }

    #[test]
    fn a_prompt_hydraui_typed_is_a_draft_until_it_starts() {
        let mut r = Readiness::default();
        r.state(None, Idle);
        r.prompt_typed();
        assert!(
            r.has_draft(),
            "an unsubmitted prompt keeps the session busy"
        );
        r.state(Some(Idle), Working);
        assert!(!r.has_draft());

        let mut ended = Readiness::default();
        ended.state(None, Idle);
        ended.prompt_typed();
        ended.state(Some(Idle), SessionState::Ended);
        assert!(!ended.has_draft());
    }

    #[test]
    fn a_session_resumed_by_hand_is_not_fresh() {
        let mut r = Readiness::default();
        r.typed();
        r.resumed_start(None);
        assert!(!r.finished);
        assert_eq!(r.draft, Draft::Clear);

        // `/resume` inside a fresh session.
        let mut inside = Readiness::default();
        inside.state(None, Idle);
        assert!(inside.finished);
        inside.resumed_start(Some(Idle));
        assert!(!inside.finished);

        inside.state(Some(Idle), SessionState::Ended);
        inside.state(None, Idle);
        assert!(inside.finished, "a later new session is fresh");
    }

    fn screen(rows: &[&str]) -> Vec<Vec<(char, bool)>> {
        rows.iter()
            .map(|r| r.chars().map(|c| (c, false)).collect())
            .collect()
    }

    #[test]
    fn the_input_line_is_read_from_the_screen() {
        assert_eq!(
            claude_input_empty(&screen(&["────", "> ", "────"])),
            Some(true)
        );
        assert_eq!(
            claude_input_empty(&screen(&["────", "> hello", "────"])),
            Some(false)
        );
        assert_eq!(
            claude_input_empty(&screen(&["  ────", "❯ ", "  ────", "  ? for shortcuts"])),
            Some(true)
        );
        let mut placeholder = screen(&["────", "> ", "────"]);
        placeholder[1].extend("Try \"x\"".chars().map(|c| (c, true)));
        assert_eq!(claude_input_empty(&placeholder), Some(true));
        assert_eq!(
            claude_input_empty(&screen(&["> old", "output", "────", "> ", "────"])),
            Some(true),
            "the last input line counts"
        );
        assert_eq!(
            claude_input_empty(&screen(&["────", "> first line", "  ", "────"])),
            Some(false)
        );
        assert_eq!(
            claude_input_empty(&screen(&["────", "> ", "  second line", "────"])),
            Some(false),
            "a draft's later lines count too"
        );
        assert_eq!(claude_input_empty(&screen(&["$ ls", "a b"])), None);
        assert_eq!(claude_input_empty(&screen(&["> no rules"])), None);
    }
}
