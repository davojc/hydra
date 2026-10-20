//! A project's task board in memory (spec: docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
//! §5-§6): the cards as last read or written, a watch on `.hydra/tasks.md` that reloads them
//! when the file changes on disk, and every change written through at once.
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use gpui::{Context, EventEmitter, Task};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::tasks::{Board, CardRef, Change, Column, normalise};
use crate::tasks_project::{self, CommitError, Privacy};

/// The toast when a change names a card the file no longer has (§6).
pub const MISSING: &str = "That card changed in the file; your change wasn't applied.";
/// The toast when an edit's card is gone and its text was added to Backlog instead (§6).
pub const RESCUED: &str = "That card changed in the file; your text was added to Backlog.";

/// The toast when the privacy choice can't be written to `.gitignore` (§3).
pub fn privacy_problem(project: &std::path::Path, e: &std::io::Error) -> String {
    format!(
        "Couldn't update {}: {e}",
        project.join(".gitignore").display()
    )
}

pub enum TaskBoardEvent {
    /// The first card was just written to a new file in a git repository: ask commit/private.
    AskGit,
    /// A change couldn't apply (card gone) or the write failed: toast this text.
    Problem(String),
    /// The user moved the card with this text to Done (worktrees: offer to merge its branch).
    MovedToDone(String),
    /// The user moved the card with this text into Pickup from another column (a reload never
    /// says this): auto-pickup types it into a free Claude session.
    ArrivedInPickup(String),
}

/// What became of a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// It's in the file (or there was nothing to write).
    Written,
    /// Its card isn't in the file any more: dropped, or an edit's new text added to Backlog
    /// (§6).
    CardGone,
    /// The file couldn't be read or written.
    WriteFailed,
}

impl Applied {
    /// Whether an edit that ended this way stays open with its text: only when the write
    /// failed, so a typed idea isn't lost (§1 anti-goal). A gone card's edit closes (§6).
    pub fn keeps_edit(self) -> bool {
        self == Applied::WriteFailed
    }
}

pub struct TaskBoard {
    project: PathBuf,
    board: Board,
    /// The file's text as last read or written, to tell our own writes from others' (§6).
    last_text: String,
    _watcher: Option<RecommendedWatcher>,
    _reload: Task<()>,
}

impl EventEmitter<TaskBoardEvent> for TaskBoard {}

/// Whether the file now reads `now` calls for a reload after it last read or wrote `last`:
/// not when they're equal, so hydraui's own writes don't echo (§6).
pub fn needs_reload(last: &str, now: &str) -> bool {
    last != now
}

/// Whether a change to `paths` may have changed the board file: one of them is the file, or
/// the project folder itself (how a full change buffer, changes lost, arrives).
pub fn concerns_board(project: &Path, paths: &[PathBuf]) -> bool {
    let file = tasks_project::tasks_path(project);
    paths.iter().any(|p| *p == file || p.as_path() == project)
}

/// What an edit whose card the file no longer has becomes (§6): its text as a new card at the
/// top of Backlog, so a typed idea isn't lost (§1). `None` for any other change, or an edit
/// that leaves the text as it was or empties it.
pub fn rescue(change: &Change) -> Option<Change> {
    let Change::Edit { card, text } = change else {
        return None;
    };
    let text = normalise(text);
    (!text.is_empty() && text != card.text).then_some(Change::Add { text })
}

/// The text of the card `change` moves into Pickup from another column, else `None`.
pub fn arrived(change: &Change) -> Option<String> {
    match change {
        Change::Move {
            card,
            to: Column::Pickup,
            ..
        } if card.column != Column::Pickup => Some(card.text.clone()),
        _ => None,
    }
}

/// The move a drop makes: `card` dropped before the card at `before` in `to`, or at the end
/// when `None`. `None` when the card isn't there or would land where it already is.
pub fn drop_move(
    board: &Board,
    card: &CardRef,
    to: Column,
    before: Option<usize>,
) -> Option<Change> {
    let from = board.find(card)?;
    let len = board.cards(to).len();
    let mut index = before.unwrap_or(len).min(len);
    if card.column == to {
        // `Change::Move`'s index counts with the card already taken out.
        if from < index {
            index -= 1;
        }
        if index == from {
            return None;
        }
    }
    Some(Change::Move {
        card: card.clone(),
        to,
        index,
    })
}

/// A recursive watch on `project` that sends `()` when the board file may have changed.
fn watch(project: &Path, tx: UnboundedSender<()>) -> Option<RecommendedWatcher> {
    let base = project.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // An error means changes may have been missed: look again.
        let look = match &res {
            Ok(event) => concerns_board(&base, &event.paths),
            Err(_) => true,
        };
        if look {
            let _ = tx.unbounded_send(());
        }
    })
    .ok()?;
    watcher.watch(project, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

impl TaskBoard {
    /// The board as the file has it now (empty when there's no file or it can't be read), watched
    /// from now until the entity is dropped.
    pub fn new(project: PathBuf, cx: &mut Context<Self>) -> Self {
        let (board, last_text) = tasks_project::load(&project).unwrap_or_default();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let watcher = watch(&project, tx);
        let reload = cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while rx.next().await.is_some() {
                // A write is often several events (temp file, rename): let them settle first.
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                while rx.try_recv().is_ok() {}
                if this.update(cx, |b, cx| b.reload(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            project,
            board,
            last_text,
            _watcher: watcher,
            _reload: reload,
        }
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Reads the file again; a read that fails, or finds what we last read or wrote, changes
    /// nothing.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Ok((board, text)) = tasks_project::load(&self.project) else {
            return;
        };
        if needs_reload(&self.last_text, &text) {
            self.board = board;
            self.last_text = text;
            cx.notify();
        }
    }

    /// Commits `change` (applied to the file as it is now, §6) and shows the result. Emits
    /// `AskGit` when this write created the file in a git repository that would commit it (§3),
    /// `Problem` when the card is gone (the board then reloads; an edit's text is added to
    /// Backlog, see `rescue`) or the write failed.
    pub fn change(&mut self, change: Change, cx: &mut Context<Self>) -> Applied {
        match tasks_project::commit(&self.project, &change) {
            Ok(done) => {
                self.board = done.board;
                self.last_text = done.text;
                if let Change::Move {
                    card,
                    to: Column::Done,
                    ..
                } = &change
                    && card.column != Column::Done
                {
                    cx.emit(TaskBoardEvent::MovedToDone(card.text.clone()));
                }
                if let Some(text) = arrived(&change) {
                    cx.emit(TaskBoardEvent::ArrivedInPickup(text));
                }
                if done.created
                    && tasks_project::is_repo(&self.project)
                    && tasks_project::privacy(&self.project) == Privacy::Committed
                {
                    cx.emit(TaskBoardEvent::AskGit);
                }
                cx.notify();
                Applied::Written
            }
            Err(CommitError::Missing) => {
                if let Some(add) = rescue(&change) {
                    // An add can't miss: it's written, or it failed and said why.
                    let applied = self.change(add, cx);
                    if applied == Applied::Written {
                        cx.emit(TaskBoardEvent::Problem(RESCUED.to_string()));
                        return Applied::CardGone;
                    }
                    return applied;
                }
                cx.emit(TaskBoardEvent::Problem(MISSING.to_string()));
                self.reload(cx);
                Applied::CardGone
            }
            Err(CommitError::Io(e)) => {
                cx.emit(TaskBoardEvent::Problem(format!(
                    "Couldn't write {}: {e}",
                    tasks_project::tasks_path(&self.project).display()
                )));
                Applied::WriteFailed
            }
        }
    }

    /// The board menu's privacy choice (§3): `true` adds our `.gitignore` line, `false` removes
    /// it. A failure is a `Problem`.
    pub fn set_private(&mut self, private: bool, cx: &mut Context<Self>) {
        let done = if private {
            tasks_project::keep_private(&self.project)
        } else {
            tasks_project::commit_tasks(&self.project)
        };
        if let Err(e) = done {
            cx.emit(TaskBoardEvent::Problem(privacy_problem(&self.project, &e)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::parse;

    #[test]
    fn task_board_ignores_its_own_echo() {
        assert!(!needs_reload("a", "a"));
        assert!(needs_reload("a", "b"));
        assert!(needs_reload("", "# Tasks\n"));
    }

    #[test]
    fn task_board_keeps_an_edit_only_when_the_write_failed() {
        assert!(Applied::WriteFailed.keeps_edit());
        assert!(!Applied::Written.keeps_edit());
        assert!(!Applied::CardGone.keeps_edit());
    }

    #[test]
    fn task_board_watches_only_its_file() {
        let project = Path::new(r"C:\work\repo");
        let file = tasks_project::tasks_path(project);
        assert!(concerns_board(project, std::slice::from_ref(&file)));
        assert!(concerns_board(project, &[project.join("x.rs"), file]));
        assert!(
            concerns_board(project, &[project.to_path_buf()]),
            "overflow"
        );
        assert!(!concerns_board(
            project,
            &[project.join("src").join("main.rs")]
        ));
        assert!(!concerns_board(project, &[project.join(".hydra")]));
    }

    #[test]
    fn task_board_adds_a_gone_cards_edited_text_to_backlog() {
        let card = CardRef {
            column: Column::Pickup,
            text: "old idea".into(),
            nth: 0,
        };
        let edit = |text: &str| Change::Edit {
            card: card.clone(),
            text: text.into(),
        };
        assert_eq!(
            rescue(&edit("  new\n idea ")),
            Some(Change::Add {
                text: "new idea".into()
            })
        );
        // Unchanged or emptied: nothing to keep.
        assert_eq!(rescue(&edit("old  idea")), None);
        assert_eq!(rescue(&edit("  ")), None);
        // Only edits are rescued.
        assert_eq!(rescue(&Change::Delete { card: card.clone() }), None);
        assert_eq!(
            rescue(&Change::Move {
                card,
                to: Column::Done,
                index: 0
            }),
            None
        );
    }

    #[test]
    fn moving_a_card_into_pickup_says_it_arrived() {
        let card = CardRef {
            column: Column::Backlog,
            text: "idea".into(),
            nth: 0,
        };
        let mv = Change::Move {
            card,
            to: Column::Pickup,
            index: 0,
        };
        assert_eq!(arrived(&mv), Some("idea".to_string()));
    }

    #[test]
    fn moves_within_pickup_or_elsewhere_say_nothing() {
        let card = |column| CardRef {
            column,
            text: "idea".into(),
            nth: 0,
        };
        let mv = |from, to| Change::Move {
            card: card(from),
            to,
            index: 0,
        };
        assert_eq!(arrived(&mv(Column::Pickup, Column::Pickup)), None);
        assert_eq!(arrived(&mv(Column::Backlog, Column::Done)), None);
        assert_eq!(arrived(&mv(Column::Pickup, Column::Backlog)), None);
        assert_eq!(
            arrived(&Change::Add {
                text: "idea".into()
            }),
            None
        );
    }

    fn board() -> Board {
        parse("## Backlog\n- a\n- b\n- a\n\n## Pickup\n- p\n")
    }

    fn card(b: &Board, column: Column, index: usize) -> CardRef {
        b.card_ref(column, index).unwrap()
    }

    #[test]
    fn task_board_drop_onto_its_own_place_is_no_change() {
        let b = board();
        let a0 = card(&b, Column::Backlog, 0);
        // Before itself, before the card after it.
        assert_eq!(drop_move(&b, &a0, Column::Backlog, Some(0)), None);
        assert_eq!(drop_move(&b, &a0, Column::Backlog, Some(1)), None);
        // The last card appended to its own column.
        let a1 = card(&b, Column::Backlog, 2);
        assert_eq!(drop_move(&b, &a1, Column::Backlog, None), None);
    }

    #[test]
    fn task_board_drop_within_a_column_counts_without_the_card() {
        let b = board();
        let a0 = card(&b, Column::Backlog, 0);
        // Before the third card: it lands second once taken out.
        assert_eq!(
            drop_move(&b, &a0, Column::Backlog, Some(2)),
            Some(Change::Move {
                card: a0.clone(),
                to: Column::Backlog,
                index: 1
            })
        );
        assert_eq!(
            drop_move(&b, &a0, Column::Backlog, None),
            Some(Change::Move {
                card: a0.clone(),
                to: Column::Backlog,
                index: 2
            })
        );
        // Upward: the index is unaffected.
        let a1 = card(&b, Column::Backlog, 2);
        assert_eq!(a1.nth, 1);
        assert_eq!(
            drop_move(&b, &a1, Column::Backlog, Some(0)),
            Some(Change::Move {
                card: a1,
                to: Column::Backlog,
                index: 0
            })
        );
    }

    #[test]
    fn task_board_drop_into_another_column() {
        let b = board();
        let bb = card(&b, Column::Backlog, 1);
        assert_eq!(
            drop_move(&b, &bb, Column::Pickup, Some(0)),
            Some(Change::Move {
                card: bb.clone(),
                to: Column::Pickup,
                index: 0
            })
        );
        assert_eq!(
            drop_move(&b, &bb, Column::Done, None),
            Some(Change::Move {
                card: bb.clone(),
                to: Column::Done,
                index: 0
            })
        );
        // A card the board no longer has.
        let gone = CardRef {
            column: Column::Backlog,
            text: "zz".into(),
            nth: 0,
        };
        assert_eq!(drop_move(&b, &gone, Column::Pickup, None), None);
    }
}
