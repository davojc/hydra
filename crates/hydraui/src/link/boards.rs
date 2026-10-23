//! Board changes the phone asks for, checked and committed through the same file lock the
//! desktop board and MCP server use.

use std::path::{Path, PathBuf};

use hydra_link::{BoardInfo, Outcome};

use super::wire;
use crate::tasks::{self, Change};
use crate::tasks_project::{self, CommitError};

/// A change the phone may make to a board.
#[derive(Debug, Clone, PartialEq)]
pub enum PhoneChange {
    /// A new card, landing at the top of `to`.
    Add {
        text: String,
        to: hydra_link::Column,
    },
    /// A card to the top of `to`.
    Move {
        card: hydra_link::CardRef,
        to: hydra_link::Column,
    },
}

fn refused(reason: impl Into<String>) -> Outcome {
    Outcome::Refused {
        reason: reason.into(),
    }
}

fn failed(e: CommitError) -> Outcome {
    match e {
        CommitError::Missing => Outcome::Missing,
        CommitError::Io(e) => refused(format!("Couldn't save the board: {e}")),
    }
}

/// Applies `change` to the board of `project`, which must be (once normalised with
/// `project_root`) one of `known`; nothing is created for any other. Returns the board as
/// written.
pub fn apply(known: &[PathBuf], project: &str, change: PhoneChange) -> Result<BoardInfo, Outcome> {
    // `project_root` walks ancestors lexically, so `<known>/../x` would resolve to `<known>`
    // though it names a folder beside it: a path with `..` never names a known project.
    let path = Path::new(project);
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(refused("hydraui doesn't know that project"));
    }
    let root = tasks_project::project_root(path);
    if !known.iter().any(|k| tasks_project::project_root(k) == root) {
        return Err(refused("hydraui doesn't know that project"));
    }
    let committed = match change {
        PhoneChange::Add { text, to } => {
            if text.trim().is_empty() {
                return Err(refused("A card needs some text"));
            }
            let mut done = tasks_project::commit(&root, &Change::Add { text: text.clone() })
                .map_err(failed)?;
            let to = wire::from_column(to);
            if to != tasks::Column::Backlog {
                let card = tasks::CardRef {
                    column: tasks::Column::Backlog,
                    text: tasks::normalise(&text),
                    nth: 0,
                };
                done = tasks_project::commit(&root, &Change::Move { card, to, index: 0 }).map_err(
                    |e| match e {
                        CommitError::Missing => {
                            refused("The new card went missing before it could be moved")
                        }
                        other => failed(other),
                    },
                )?;
            }
            done
        }
        PhoneChange::Move { card, to } => {
            let card = tasks::CardRef {
                column: wire::from_column(card.column),
                text: card.text,
                nth: card.nth,
            };
            let change = Change::Move {
                card,
                to: wire::from_column(to),
                index: 0,
            };
            tasks_project::commit(&root, &change).map_err(failed)?
        }
    };
    Ok(wire::board_info(&root, &committed.board))
}

/// The card's text when `change` put a card into Pickup.
pub fn arrived_in_pickup(change: &PhoneChange) -> Option<String> {
    match change {
        PhoneChange::Add { text, to } if *to == hydra_link::Column::Pickup => Some(text.clone()),
        PhoneChange::Move { card, to } if *to == hydra_link::Column::Pickup => {
            Some(card.text.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydra_link::Column as W;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        d
    }

    fn proj(d: &tempfile::TempDir) -> String {
        d.path().to_string_lossy().into_owned()
    }

    fn add(text: &str, to: W) -> PhoneChange {
        PhoneChange::Add {
            text: text.into(),
            to,
        }
    }

    fn texts(info: &BoardInfo, col: usize) -> Vec<&str> {
        info.columns[col].iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn a_card_added_to_pickup_lands_on_top_of_pickup() {
        let d = repo();
        let known = vec![d.path().to_path_buf()];
        apply(&known, &proj(&d), add("old", W::Backlog)).unwrap();
        apply(&known, &proj(&d), add("older", W::Pickup)).unwrap();
        let info = apply(&known, &proj(&d), add("fix paste", W::Pickup)).unwrap();
        assert_eq!(texts(&info, 1), ["fix paste", "older"]);
        assert_eq!(texts(&info, 0), ["old"]);
        let (board, _) = tasks_project::load(d.path()).unwrap();
        assert_eq!(board.cards(tasks::Column::Pickup)[0].text, "fix paste");
    }

    #[test]
    fn an_unknown_project_is_refused_and_nothing_written() {
        let known_dir = repo();
        let other = repo();
        let known = vec![known_dir.path().to_path_buf()];
        let sneaky = format!("{}/../x", known_dir.path().display());
        for p in [proj(&other), sneaky] {
            match apply(&known, &p, add("x", W::Backlog)) {
                Err(Outcome::Refused { reason }) => {
                    assert_eq!(reason, "hydraui doesn't know that project")
                }
                other => panic!("expected Refused, got {other:?}"),
            }
        }
        assert!(!other.path().join(".hydra").exists());
        assert!(!known_dir.path().join(".hydra").exists());
        assert!(!known_dir.path().join("../x").exists());
    }

    #[test]
    fn a_stale_card_is_missing() {
        let d = repo();
        let known = vec![d.path().to_path_buf()];
        apply(&known, &proj(&d), add("real", W::Backlog)).unwrap();
        let path = tasks_project::tasks_path(d.path());
        let before = std::fs::read_to_string(&path).unwrap();
        let stale = PhoneChange::Move {
            card: hydra_link::CardRef {
                column: W::Backlog,
                text: "gone".into(),
                nth: 0,
            },
            to: W::Pickup,
        };
        assert_eq!(apply(&known, &proj(&d), stale), Err(Outcome::Missing));
        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn a_card_moves_to_the_top_of_its_new_column() {
        let d = repo();
        let known = vec![d.path().to_path_buf()];
        apply(&known, &proj(&d), add("a", W::Backlog)).unwrap();
        let mv = PhoneChange::Move {
            card: hydra_link::CardRef {
                column: W::Backlog,
                text: "a".into(),
                nth: 0,
            },
            to: W::Verify,
        };
        let info = apply(&known, &proj(&d), mv).unwrap();
        assert_eq!(texts(&info, 3), ["a"]);
        assert!(info.columns[0].is_empty());
    }

    #[test]
    fn blank_text_is_refused() {
        let d = repo();
        let known = vec![d.path().to_path_buf()];
        for t in ["", "   \n\t"] {
            assert_eq!(
                apply(&known, &proj(&d), add(t, W::Pickup)),
                Err(Outcome::Refused {
                    reason: "A card needs some text".into()
                })
            );
        }
        assert!(!d.path().join(".hydra").exists());
    }

    #[test]
    fn only_pickup_arrivals_are_reported() {
        let card = hydra_link::CardRef {
            column: W::Backlog,
            text: "m".into(),
            nth: 0,
        };
        let mv = |to| PhoneChange::Move {
            card: card.clone(),
            to,
        };
        assert_eq!(arrived_in_pickup(&add("a", W::Pickup)), Some("a".into()));
        assert_eq!(arrived_in_pickup(&mv(W::Pickup)), Some("m".into()));
        assert_eq!(arrived_in_pickup(&add("a", W::Backlog)), None);
        assert_eq!(arrived_in_pickup(&mv(W::Verify)), None);
    }
}
