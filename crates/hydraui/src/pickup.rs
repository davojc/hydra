//! Auto-pickup's choice of a Claude session for a card moved into Pickup (spec:
//! docs/superpowers/specs/2026-10-20-hydraui-auto-pickup-design.md §5-§6): which terminals are
//! candidates, which of them are clearly free, and the prompt typed into the one picked. Pure, so
//! the rules are tested directly.
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
/// characters become spaces; a long card is cut, with "…".
pub fn prompt(card: &str) -> String {
    let flat: String = card
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = if flat.chars().count() > CARD_CHARS {
        let cut: String = flat.chars().take(CARD_CHARS).collect();
        format!("{}…", cut.trim_end())
    } else {
        flat
    };
    format!("A card was moved to Pickup: \"{text}\". Take it with tasks_take and do it.")
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
    fn a_random_choice_is_one_of_the_panes() {
        assert_eq!(choose(&[]), None);
        assert_eq!(choose(&[7]), Some(7));
        assert!([1, 2, 3].contains(&choose(&[1, 2, 3]).unwrap()));
    }
}
