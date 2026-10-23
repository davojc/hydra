//! hydraui's types as the phone sees them.

use std::path::Path;

use hydra_link::{BoardInfo, Kind, Status, WireCard};

use crate::activity::Activity;
use crate::mcp::SessionState;
use crate::tasks::{self, Board};

/// A hydraui column as the wire names it.
pub fn column(c: tasks::Column) -> hydra_link::Column {
    match c {
        tasks::Column::Backlog => hydra_link::Column::Backlog,
        tasks::Column::Pickup => hydra_link::Column::Pickup,
        tasks::Column::InProgress => hydra_link::Column::InProgress,
        tasks::Column::Verify => hydra_link::Column::Verify,
        tasks::Column::Done => hydra_link::Column::Done,
    }
}

/// A wire column as hydraui names it.
pub fn from_column(c: hydra_link::Column) -> tasks::Column {
    match c {
        hydra_link::Column::Backlog => tasks::Column::Backlog,
        hydra_link::Column::Pickup => tasks::Column::Pickup,
        hydra_link::Column::InProgress => tasks::Column::InProgress,
        hydra_link::Column::Verify => tasks::Column::Verify,
        hydra_link::Column::Done => tasks::Column::Done,
    }
}

/// What a terminal is and what it is doing. `claude` is the Claude session's state when the
/// terminal runs one; `finished` says an idle Claude has a turn the user hasn't seen.
pub fn status(claude: Option<SessionState>, activity: Activity, finished: bool) -> (Kind, Status) {
    match claude {
        Some(SessionState::Waiting) => (Kind::Claude, Status::NeedsYou),
        Some(SessionState::Working) => (Kind::Claude, Status::Working),
        Some(SessionState::Idle) if finished => (Kind::Claude, Status::Finished),
        Some(SessionState::Idle) => (Kind::Claude, Status::Idle),
        Some(SessionState::Ended) => (Kind::Claude, Status::Ended),
        None if activity == Activity::Working => (Kind::Shell, Status::Working),
        None => (Kind::Shell, Status::Idle),
    }
}

/// `board` as the phone shows it; `name` is the project folder's name.
pub fn board_info(project: &Path, board: &Board) -> BoardInfo {
    BoardInfo {
        project: project.to_string_lossy().into_owned(),
        name: project
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        columns: tasks::Column::ALL
            .iter()
            .map(|&c| {
                board
                    .cards(c)
                    .iter()
                    .map(|card| WireCard {
                        text: card.text.clone(),
                        taker: card.taker.clone(),
                        commit: card.commit.clone(),
                    })
                    .collect()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_states_map_to_what_the_phone_shows() {
        let a = Activity::Idle;
        let k = Kind::Claude;
        assert_eq!(
            status(Some(SessionState::Waiting), a, false),
            (k, Status::NeedsYou)
        );
        assert_eq!(
            status(Some(SessionState::Working), a, false),
            (k, Status::Working)
        );
        assert_eq!(
            status(Some(SessionState::Idle), a, true),
            (k, Status::Finished)
        );
        assert_eq!(
            status(Some(SessionState::Idle), a, false),
            (k, Status::Idle)
        );
        assert_eq!(
            status(Some(SessionState::Ended), a, true),
            (k, Status::Ended)
        );
        assert_eq!(
            status(None, Activity::Working, false),
            (Kind::Shell, Status::Working)
        );
        assert_eq!(
            status(None, Activity::Waiting, false),
            (Kind::Shell, Status::Idle)
        );
        assert_eq!(
            status(None, Activity::Idle, false),
            (Kind::Shell, Status::Idle)
        );
    }

    #[test]
    fn columns_round_trip() {
        for c in tasks::Column::ALL {
            assert_eq!(from_column(column(c)), c);
        }
    }

    #[test]
    fn a_board_is_described_in_column_order() {
        let mut board = Board::default();
        board.columns[tasks::Column::Verify as usize].push(tasks::Card {
            text: "x".into(),
            taker: Some("t".into()),
            commit: Some("abc".into()),
        });
        let info = board_info(Path::new("/w/proj"), &board);
        assert_eq!(info.name, "proj");
        assert_eq!(info.columns.len(), 5);
        assert_eq!(info.columns[3][0].commit.as_deref(), Some("abc"));
        assert!(info.columns[0].is_empty());
    }
}
