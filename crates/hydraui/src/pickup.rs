//! Auto-pickup's choice of a Claude session for a card moved into Pickup (spec:
//! docs/superpowers/specs/2026-10-20-hydraui-auto-pickup-design.md §5-§6): which terminals are
//! candidates, which of them are clearly free, and the prompt typed into the one picked. Pure, so
//! the rules are tested directly.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::envs::PaneId;
use crate::mcp::SessionState;
use crate::tasks::{Board, Column};

/// A terminal a card was just sent to is left alone this long, unless its hooks report first.
pub const RESERVED_FOR: Duration = Duration::from_secs(30);

/// The card's text in the prompt is cut to this many characters.
const CARD_CHARS: usize = 120;

/// What hydraui knows about one terminal, for choosing where a card goes.
#[derive(Debug, Clone)]
pub struct Session {
    pub pane: PaneId,
    /// `git_root(cwd)`: a linked worktree's root is its own folder.
    pub project_root: Option<PathBuf>,
    pub cwd: PathBuf,
    /// The Claude session's last hook state; `None` when none has reported.
    pub claude: Option<SessionState>,
    /// The window is active and this is the terminal in front.
    pub focused: bool,
    /// A card was sent to it moments ago.
    pub reserved: bool,
    /// Its input may hold something the user typed.
    pub draft: bool,
    /// Its last turn clearly finished: a fresh session, or one that just verified a card.
    pub finished: bool,
}

/// Where a card can go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// These are free for sure; empty when none is.
    Certain(Vec<PaneId>),
    /// These may be free: Jev decides.
    AskJev(Vec<PaneId>),
}

/// The session is in `project`'s main work tree (§5.1, 2).
fn in_project(s: &Session, project: &Path) -> bool {
    match &s.project_root {
        Some(root) => root == project,
        None => s.cwd == project,
    }
}

/// Facts 1-3 (§5.1): a Claude session in `project`'s main work tree whose last state is idle.
fn idle(s: &Session, project: &Path) -> bool {
    s.claude == Some(SessionState::Idle) && in_project(s, project)
}

/// The project's idle Claude sessions, for the card menu's Send to (§8).
pub fn idle_in_project(sessions: &[Session], project: &Path) -> Vec<PaneId> {
    sessions
        .iter()
        .filter(|s| idle(s, project))
        .map(|s| s.pane)
        .collect()
}

/// The sessions that pass every fact of §5.1: idle in the project, not in front of the user,
/// not reserved and with no draft.
pub fn candidates(sessions: &[Session], project: &Path) -> Vec<PaneId> {
    sessions
        .iter()
        .filter(|s| idle(s, project) && !s.focused && !s.reserved && !s.draft)
        .map(|s| s.pane)
        .collect()
}

/// Where a card for `project` can go (§5.4): the clearly finished candidates when there are
/// any; else the others for Jev to judge when hydraui has a key (`jev`); else nowhere.
pub fn pick(sessions: &[Session], project: &Path, jev: bool) -> Pick {
    let ok = candidates(sessions, project);
    let (finished, unsure): (Vec<&Session>, Vec<&Session>) = sessions
        .iter()
        .filter(|s| ok.contains(&s.pane))
        .partition(|s| s.finished);
    if !finished.is_empty() {
        Pick::Certain(finished.iter().map(|s| s.pane).collect())
    } else if jev {
        Pick::AskJev(unsure.iter().map(|s| s.pane).collect())
    } else {
        Pick::Certain(Vec::new())
    }
}

/// Why `s` can or can't take a card for `project`, for the log: the first fact of §5.1 it fails,
/// else whether it is clearly finished or left to Jev.
pub fn describe(s: &Session, project: &Path) -> String {
    match s.claude {
        None => return "skipped: no Claude hook report from it".into(),
        Some(SessionState::Idle) => {}
        Some(state) => return format!("skipped: Claude is {state:?}, not Idle"),
    }
    if !in_project(s, project) {
        let at = s.project_root.as_ref().unwrap_or(&s.cwd);
        return format!("skipped: in {}, not this project", at.display());
    }
    if s.focused {
        "skipped: it is the terminal in front of you".into()
    } else if s.reserved {
        "skipped: a card was sent to it moments ago".into()
    } else if s.draft {
        "skipped: its input may hold a draft".into()
    } else if s.finished {
        "free: clearly finished (fresh, or just verified a card)".into()
    } else {
        "uncertain: idle after other work, Jev decides".into()
    }
}

/// Whether a card sent at `at` still holds its terminal at `now`.
pub fn is_reserved(at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(at) < RESERVED_FOR
}

/// Whether `text` is still a card in Pickup: the Jev calls take a moment, and a card that has
/// left meanwhile isn't sent (§6).
pub fn still_in_pickup(board: &Board, text: &str) -> bool {
    board.cards(Column::Pickup).iter().any(|c| c.text == text)
}

/// The one-line prompt typed into the session for `card` (§6). Line breaks and other control
/// characters become spaces; a long card is cut, with "…", and the prompt says where its full
/// text is, which `tasks_take` needs.
pub fn prompt(card: &str) -> String {
    let flat: String = card
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() > CARD_CHARS {
        let cut: String = flat.chars().take(CARD_CHARS).collect();
        format!(
            "A card was moved to Pickup: \"{}…\". Take it with tasks_take and do it. (shortened: tasks_read shows it in full)",
            cut.trim_end()
        )
    } else {
        format!("A card was moved to Pickup: \"{flat}\". Take it with tasks_take and do it.")
    }
}

/// Why Jev found no session free, when every ask failed (`Err` holds the failure, never the
/// key): the first failure. `None` when Jev answered at least once, or wasn't asked.
pub fn jev_failure(answers: &[Result<bool, String>]) -> Option<String> {
    if answers.iter().all(Result::is_err) {
        answers.first().and_then(|a| a.clone().err())
    } else {
        None
    }
}

/// The warning when no session can take a card (§7), with Jev's failure when it had one.
pub fn no_free_text(jev_failure: Option<&str>) -> String {
    match jev_failure {
        Some(why) => format!("No free Claude session for this card (Jev: {why})"),
        None => "No free Claude session for this card".to_string(),
    }
}

/// One of `panes` at random.
pub fn choose(panes: &[PaneId]) -> Option<PaneId> {
    if panes.is_empty() {
        return None;
    }
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the OS has a random source");
    let i = (u64::from_le_bytes(bytes) % panes.len() as u64) as usize;
    Some(panes[i])
}

/// The terminal the user may be typing in: the shown one of the selected environment, while
/// the window is active and that terminal's Tasks tab isn't the one on screen (§5.1, 4).
pub fn focused_pane(
    window_active: bool,
    shown: Option<PaneId>,
    board_shown: impl Fn(PaneId) -> bool,
) -> Option<PaneId> {
    (window_active && shown.is_some_and(|id| !board_shown(id)))
        .then_some(shown)
        .flatten()
}

/// Cards on their way to a session, by project and text, so one card never goes to two
/// sessions (§2 anti-goals): an offer is running for it, or it was sent moments ago.
#[derive(Debug, Default)]
pub struct InFlight {
    /// `None` while an offer for it runs, else when it was sent.
    cards: HashMap<(PathBuf, String), Option<Instant>>,
}

impl InFlight {
    /// Starts an offer for the card; `false` when one is running or it was just sent.
    pub fn try_start(&mut self, project: &Path, text: &str, now: Instant) -> bool {
        let key = (project.to_path_buf(), text.to_string());
        match self.cards.get(&key) {
            None => {}
            Some(Some(at)) if !is_reserved(*at, now) => {}
            Some(_) => return false,
        }
        self.cards.insert(key, None);
        true
    }

    /// The card was sent at `now`: it stays here until it leaves Pickup or 30 s pass.
    pub fn sent(&mut self, project: &Path, text: &str, now: Instant) {
        self.cards
            .insert((project.to_path_buf(), text.to_string()), Some(now));
    }

    /// An offer for the card is still running: not ended, nor overtaken by a send by hand.
    pub fn running(&self, project: &Path, text: &str) -> bool {
        matches!(
            self.cards.get(&(project.to_path_buf(), text.to_string())),
            Some(None)
        )
    }

    /// The offer ended without sending.
    pub fn finish(&mut self, project: &Path, text: &str) {
        self.cards
            .remove(&(project.to_path_buf(), text.to_string()));
    }

    /// The card arrived in Pickup again, so it had left: a past send no longer holds it.
    pub fn arrived(&mut self, project: &Path, text: &str) {
        let key = (project.to_path_buf(), text.to_string());
        if matches!(self.cards.get(&key), Some(Some(_))) {
            self.cards.remove(&key);
        }
    }

    /// Forgets cards sent 30 s ago or more, or no longer in Pickup (`in_pickup`).
    pub fn expire(&mut self, now: Instant, in_pickup: impl Fn(&Path, &str) -> bool) {
        self.cards.retain(|(project, text), sent| match sent {
            None => true,
            Some(at) => is_reserved(*at, now) && in_pickup(project, text),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Card;

    fn project() -> PathBuf {
        PathBuf::from("/repo")
    }

    /// An idle, finished Claude session in the project's main work tree: a candidate.
    fn free(pane: PaneId) -> Session {
        Session {
            pane,
            project_root: Some(project()),
            cwd: project().join("src"),
            claude: Some(SessionState::Idle),
            focused: false,
            reserved: false,
            draft: false,
            finished: true,
        }
    }

    #[test]
    fn describe_names_the_first_fact_a_session_fails() {
        let p = project();
        let said = |s: Session| describe(&s, &p);
        assert!(
            said(Session {
                claude: None,
                ..free(1)
            })
            .contains("no Claude hook report")
        );
        assert!(
            said(Session {
                claude: Some(SessionState::Working),
                focused: true,
                ..free(1)
            })
            .contains("Working")
        );
        let elsewhere = Session {
            project_root: Some(PathBuf::from("/other")),
            ..free(1)
        };
        assert!(said(elsewhere).contains("other"), "names where it is");
        assert!(
            said(Session {
                focused: true,
                draft: true,
                ..free(1)
            })
            .contains("in front")
        );
        assert!(
            said(Session {
                reserved: true,
                ..free(1)
            })
            .contains("sent to it")
        );
        assert!(
            said(Session {
                draft: true,
                ..free(1)
            })
            .contains("draft")
        );
        assert!(said(free(1)).starts_with("free"));
        assert!(
            said(Session {
                finished: false,
                ..free(1)
            })
            .contains("Jev decides")
        );
    }

    #[test]
    fn finished_sessions_are_picked_without_jev() {
        let unsure = Session {
            finished: false,
            ..free(2)
        };
        let s = [free(1), unsure, free(3)];
        assert_eq!(pick(&s, &project(), false), Pick::Certain(vec![1, 3]));
        assert_eq!(pick(&s, &project(), true), Pick::Certain(vec![1, 3]));
    }

    #[test]
    fn uncertain_sessions_go_to_jev_only_when_there_is_a_key() {
        let unsure = |pane| Session {
            finished: false,
            ..free(pane)
        };
        let s = [unsure(1), unsure(2)];
        assert_eq!(pick(&s, &project(), false), Pick::Certain(vec![]));
        assert_eq!(pick(&s, &project(), true), Pick::AskJev(vec![1, 2]));
    }

    #[test]
    fn focused_terminal_is_never_a_candidate() {
        let s = [Session {
            focused: true,
            ..free(1)
        }];
        assert_eq!(pick(&s, &project(), true), Pick::AskJev(vec![]));
        assert!(candidates(&s, &project()).is_empty());
    }

    #[test]
    fn a_reserved_terminal_is_not_a_candidate() {
        let s = [
            Session {
                reserved: true,
                ..free(1)
            },
            free(2),
        ];
        assert_eq!(pick(&s, &project(), false), Pick::Certain(vec![2]));
        let sent = Instant::now();
        assert!(is_reserved(sent, sent + Duration::from_secs(29)));
        assert!(!is_reserved(sent, sent + RESERVED_FOR));
    }

    #[test]
    fn drafts_working_and_waiting_sessions_are_skipped() {
        let s = [
            Session {
                draft: true,
                ..free(1)
            },
            Session {
                claude: Some(SessionState::Working),
                ..free(2)
            },
            Session {
                claude: Some(SessionState::Waiting),
                ..free(3)
            },
            Session {
                claude: None,
                ..free(4)
            },
            Session {
                claude: Some(SessionState::Ended),
                ..free(5)
            },
        ];
        assert!(candidates(&s, &project()).is_empty());
        assert_eq!(pick(&s, &project(), false), Pick::Certain(vec![]));
        // Send to lists the idle ones, draft or not.
        assert_eq!(idle_in_project(&s, &project()), vec![1]);
    }

    #[test]
    fn linked_worktrees_and_other_projects_are_skipped() {
        let worktree = PathBuf::from("/repo-worktrees/card");
        let s = [
            Session {
                project_root: Some(worktree.clone()),
                cwd: worktree,
                ..free(1)
            },
            Session {
                project_root: Some(PathBuf::from("/other")),
                cwd: PathBuf::from("/other"),
                ..free(2)
            },
            // Outside git: the folder itself must be the project.
            Session {
                project_root: None,
                cwd: project(),
                ..free(3)
            },
            Session {
                project_root: None,
                cwd: project().join("sub"),
                ..free(4)
            },
        ];
        assert_eq!(candidates(&s, &project()), vec![3]);
        assert_eq!(idle_in_project(&s, &project()), vec![3]);
    }

    #[test]
    fn prompt_is_one_line_and_quotes_the_card() {
        let p = prompt("a\nb \"c\"");
        assert!(p.contains("a b \"c\""), "{p}");
        assert!(!p.contains('\n'));
        assert_eq!(
            p,
            "A card was moved to Pickup: \"a b \"c\"\". Take it with tasks_take and do it."
        );
        let long = prompt(&"x".repeat(200));
        assert!(
            long.contains(&format!("\"{}…\"", "x".repeat(120))),
            "{long}"
        );
        assert!(!long.contains(&"x".repeat(121)));
        assert!(
            long.ends_with("(shortened: tasks_read shows it in full)"),
            "{long}"
        );
        assert!(!p.contains("shortened"));
    }

    #[test]
    fn jev_is_blamed_only_when_every_ask_failed() {
        let ok: Result<bool, String> = Ok(false);
        let err: Result<bool, String> = Err("HTTP 401".into());
        assert_eq!(jev_failure(&[]), None);
        assert_eq!(jev_failure(&[ok, err.clone()]), None);
        assert_eq!(
            jev_failure(&[err.clone(), err]),
            Some("HTTP 401".to_string())
        );
        assert_eq!(no_free_text(None), "No free Claude session for this card");
        assert_eq!(
            no_free_text(Some("HTTP 401")),
            "No free Claude session for this card (Jev: HTTP 401)"
        );
    }

    #[test]
    fn send_needs_the_card_still_in_pickup() {
        let mut board = Board::default();
        board.columns[Column::Pickup as usize].push(Card {
            text: "fix it".into(),
            taker: None,
            commit: None,
        });
        board.columns[Column::InProgress as usize].push(Card {
            text: "taken".into(),
            taker: None,
            commit: None,
        });
        assert!(still_in_pickup(&board, "fix it"));
        assert!(!still_in_pickup(&board, "taken"));
        assert!(!still_in_pickup(&board, "gone"));
    }

    #[test]
    fn a_card_cannot_end_the_paste_early() {
        let p = prompt("x\x1b[201~\rrm -rf /");
        assert!(!p.contains('\x1b'), "{p:?}");
        assert!(!p.contains('\r'));
    }

    #[test]
    fn the_terminal_under_its_shown_tasks_tab_is_not_focused() {
        assert_eq!(focused_pane(true, Some(1), |_| false), Some(1));
        assert_eq!(focused_pane(true, Some(1), |p| p == 1), None);
        assert_eq!(focused_pane(false, Some(1), |_| false), None);
        assert_eq!(focused_pane(true, None, |_| false), None);
    }

    #[test]
    fn an_offer_overtaken_by_a_send_by_hand_stops() {
        let (repo, now) = (project(), Instant::now());
        let mut f = InFlight::default();
        assert!(f.try_start(&repo, "card", now));
        assert!(f.running(&repo, "card"));
        // The user sent it with Send to while Jev was thinking.
        f.sent(&repo, "card", now);
        assert!(!f.running(&repo, "card"));
        assert!(!InFlight::default().running(&repo, "card"));
    }

    #[test]
    fn a_card_on_its_way_is_not_offered_twice() {
        let (repo, now) = (project(), Instant::now());
        let mut f = InFlight::default();
        assert!(f.try_start(&repo, "card", now));
        // An offer is running (Jev): another for the same card does nothing.
        assert!(!f.try_start(&repo, "card", now));
        // Other cards and other projects are their own.
        assert!(f.try_start(&repo, "other", now));
        assert!(f.try_start(Path::new("/elsewhere"), "card", now));
        // Ended without a send: it can be offered again.
        f.finish(&repo, "card");
        assert!(f.try_start(&repo, "card", now));
    }

    #[test]
    fn a_sent_card_is_held_until_it_leaves_pickup_or_time_passes() {
        let (repo, now) = (project(), Instant::now());
        let mut f = InFlight::default();
        assert!(f.try_start(&repo, "card", now));
        f.sent(&repo, "card", now);
        let still = |_: &Path, _: &str| true;
        f.expire(now + Duration::from_secs(10), still);
        assert!(!f.try_start(&repo, "card", now + Duration::from_secs(10)));
        // 30 s on, it can go again.
        f.expire(now + RESERVED_FOR, still);
        assert!(f.try_start(&repo, "card", now + RESERVED_FOR));

        // Taken out of Pickup: it can go again.
        let mut f = InFlight::default();
        f.sent(&repo, "card", now);
        f.expire(now, |_, _| false);
        assert!(f.try_start(&repo, "card", now));

        // Moved out and back into Pickup after the send: it can go again.
        let mut f = InFlight::default();
        f.sent(&repo, "card", now);
        f.arrived(&repo, "card");
        assert!(f.try_start(&repo, "card", now));

        // ...but not while an offer for it is still running.
        let mut f = InFlight::default();
        assert!(f.try_start(&repo, "card", now));
        f.arrived(&repo, "card");
        f.expire(now, |_, _| false);
        assert!(!f.try_start(&repo, "card", now));
    }

    #[test]
    fn a_random_choice_is_one_of_the_panes() {
        assert_eq!(choose(&[]), None);
        assert_eq!(choose(&[7]), Some(7));
        assert!([1, 2, 3].contains(&choose(&[1, 2, 3]).unwrap()));
    }
}
