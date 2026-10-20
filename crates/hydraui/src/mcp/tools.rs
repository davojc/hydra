//! The task-board tools (spec §5): pure logic over the calling terminal's board file.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::registry::Terminal;
use crate::tasks::{Change, Column, normalise};
use crate::tasks_project::{CommitError, commit, load, project_root};

/// What the server tells Claude at `initialize` (spec §5, "What the instructions and
/// descriptions tell Claude").
pub const INSTRUCTIONS: &str = "\
The task board is the user's to-do list for this project (the Tasks tab in hydraui). It has four \
columns: Backlog (ideas), Pickup (work the user has approved for you to do), InProgress and Done.

- Check Pickup with tasks_read when the user asks you to, and after you finish a piece of work.
- Take one card at a time from Pickup with tasks_take, then do the work it describes.
- When the work is done, tell the user and leave the card in InProgress: the user reviews it and \
moves it to Done. Never claim on the board that a card is done; you cannot move it.
- Ideas or follow-ups worth keeping go to Backlog with tasks_add, never into Pickup.
- Use tasks_mine to recall which cards this session is working on.
- You cannot move cards into Pickup or Done, edit, reorder or delete them: those are the user's.";

/// The `tools` array of `tools/list`.
pub fn list() -> Value {
    let none = json!({ "type": "object", "properties": {} });
    let text = |what: &str| {
        json!({
            "type": "object",
            "properties": { "text": { "type": "string", "description": what } },
            "required": ["text"],
        })
    };
    json!([
        {
            "name": "tasks_read",
            "description": "Read this project's task board: every column's cards in order, with \
                who took each InProgress or Done card, plus your own name. The board is the \
                user's to-do list; Pickup holds work they approved. Check it when the user asks \
                and after you finish a piece of work.",
            "inputSchema": none,
        },
        {
            "name": "tasks_add",
            "description": "Add an idea or follow-up worth keeping to the top of Backlog. It never \
                goes into Pickup: the user decides what gets picked up.",
            "inputSchema": text("The card's text (one paragraph)."),
        },
        {
            "name": "tasks_take",
            "description": "Take one approved card from Pickup before you start it: it moves to \
                InProgress with your name on it. Take one card at a time. When the work is done, \
                tell the user and leave it in InProgress; the user moves it to Done.",
            "inputSchema": text("The exact text of the Pickup card, as tasks_read shows it."),
        },
        {
            "name": "tasks_mine",
            "description": "List the InProgress cards you took (your own name on them), to recall \
                what this session is working on.",
            "inputSchema": none,
        },
    ])
}

pub struct Outcome {
    pub text: String,
    pub is_error: bool,
    /// The project whose board file this call created.
    pub created: Option<PathBuf>,
}

impl Outcome {
    fn ok(text: String) -> Self {
        Outcome {
            text,
            is_error: false,
            created: None,
        }
    }

    fn err(text: impl Into<String>) -> Self {
        Outcome {
            text: text.into(),
            is_error: true,
            created: None,
        }
    }
}

/// Runs a tool for `terminal`; `None` for a name that isn't one of ours.
pub fn call(terminal: &Terminal, name: &str, args: &Value) -> Option<Outcome> {
    let project = project_root(&terminal.cwd);
    Some(match name {
        "tasks_read" => match load(&project) {
            Err(e) => Outcome::err(format!("can't read the board: {e}")),
            Ok((board, _)) => {
                let mut columns = serde_json::Map::new();
                for column in Column::ALL {
                    let cards: Vec<Value> = board
                        .cards(column)
                        .iter()
                        .map(|c| match &c.taker {
                            Some(t) => json!({ "text": c.text, "taker": t }),
                            None => json!({ "text": c.text }),
                        })
                        .collect();
                    columns.insert(column.heading().into(), Value::Array(cards));
                }
                let doc = json!({
                    "you": terminal.name,
                    "project": project.display().to_string(),
                    "columns": columns,
                });
                Outcome::ok(serde_json::to_string_pretty(&doc).expect("json serialises"))
            }
        },
        "tasks_mine" => match load(&project) {
            Err(e) => Outcome::err(format!("can't read the board: {e}")),
            Ok((board, _)) => {
                let mine: Vec<&str> = board
                    .cards(Column::InProgress)
                    .iter()
                    .filter(|c| c.taker.as_deref() == Some(terminal.name.as_str()))
                    .map(|c| c.text.as_str())
                    .collect();
                Outcome::ok(json!(mine).to_string())
            }
        },
        "tasks_add" | "tasks_take" => {
            let Some(text) = args.get("text").and_then(Value::as_str) else {
                return Some(Outcome::err(
                    "the argument `text` is required and must be a string",
                ));
            };
            let text = normalise(text);
            if text.is_empty() {
                return Some(Outcome::err("text is empty"));
            }
            let adding = name == "tasks_add";
            let change = if adding {
                Change::Add { text }
            } else {
                Change::Take {
                    text,
                    taker: terminal.name.clone(),
                }
            };
            match commit(&project, &change) {
                Ok(done) => Outcome {
                    text: if adding {
                        "added to Backlog".into()
                    } else {
                        "taken: the card is now in InProgress".into()
                    },
                    is_error: false,
                    created: done.created.then_some(project),
                },
                Err(CommitError::Missing) => Outcome::err(
                    "no Pickup card has that text: it may be taken, moved or edited already. \
                     Call tasks_read to see the board.",
                ),
                Err(CommitError::Io(e)) => Outcome::err(format!("can't write the board: {e}")),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder that is a git repo root, so `project_root` stops at it.
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        d
    }

    #[test]
    fn a_session_adds_takes_and_finds_its_cards() {
        let d = repo();
        let t = Terminal {
            name: "claude".into(),
            cwd: d.path().join("src"),
        };
        std::fs::create_dir(&t.cwd).unwrap();
        std::fs::create_dir(d.path().join(".hydra")).unwrap();
        std::fs::write(
            d.path().join(".hydra/tasks.md"),
            "## Pickup\n- do the thing\n",
        )
        .unwrap();
        let take = call(&t, "tasks_take", &json!({"text": "do the thing"})).unwrap();
        assert!(!take.is_error);
        assert!(
            call(&t, "tasks_take", &json!({"text": "do the thing"}))
                .unwrap()
                .is_error
        );
        assert!(
            !call(&t, "tasks_add", &json!({"text": "follow-up"}))
                .unwrap()
                .is_error
        );
        let mine: Value =
            serde_json::from_str(&call(&t, "tasks_mine", &json!({})).unwrap().text).unwrap();
        assert_eq!(mine, json!(["do the thing"]));
        let file = std::fs::read_to_string(d.path().join(".hydra/tasks.md")).unwrap();
        assert!(file.contains("## Backlog\n- follow-up\n"));
        assert!(file.contains("## InProgress\n- [claude] do the thing\n"));
    }

    #[test]
    fn reading_a_missing_board_creates_nothing() {
        let d = repo();
        let t = Terminal {
            name: "c".into(),
            cwd: d.path().into(),
        };
        let read = call(&t, "tasks_read", &json!({})).unwrap();
        assert!(!read.is_error && read.text.contains("\"you\": \"c\""));
        assert!(
            call(&t, "tasks_take", &json!({"text": "x"}))
                .unwrap()
                .is_error
        );
        assert!(!call(&t, "tasks_mine", &json!({})).unwrap().is_error);
        assert!(!d.path().join(".hydra").exists());
    }

    #[test]
    fn bad_calls_are_errors_not_panics() {
        let d = repo();
        let t = Terminal {
            name: "c".into(),
            cwd: d.path().into(),
        };
        assert!(call(&t, "tasks_move", &json!({})).is_none());
        assert!(call(&t, "tasks_add", &json!({"text": 5})).unwrap().is_error);
        assert!(call(&t, "tasks_add", &json!({})).unwrap().is_error);
        assert!(
            call(&t, "tasks_add", &json!({"text": "  "}))
                .unwrap()
                .is_error
        );
    }

    #[test]
    fn the_list_offers_exactly_the_four_tools() {
        let names: Vec<_> = list()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["tasks_read", "tasks_add", "tasks_take", "tasks_mine"]
        );
    }

    #[test]
    fn creating_the_board_is_reported() {
        let d = repo();
        let t = Terminal {
            name: "c".into(),
            cwd: d.path().into(),
        };
        assert_eq!(
            call(&t, "tasks_add", &json!({"text": "x"}))
                .unwrap()
                .created
                .as_deref(),
            Some(d.path())
        );
        assert_eq!(
            call(&t, "tasks_add", &json!({"text": "y"}))
                .unwrap()
                .created,
            None
        );
    }

    #[test]
    fn read_shows_takers_and_columns() {
        let d = repo();
        let t = Terminal {
            name: "c".into(),
            cwd: d.path().into(),
        };
        std::fs::create_dir(d.path().join(".hydra")).unwrap();
        std::fs::write(
            d.path().join(".hydra/tasks.md"),
            "## Pickup\n- a\n\n## InProgress\n- [bob] b\n",
        )
        .unwrap();
        let read: Value =
            serde_json::from_str(&call(&t, "tasks_read", &json!({})).unwrap().text).unwrap();
        assert_eq!(read["columns"]["Pickup"], json!([{"text": "a"}]));
        assert_eq!(
            read["columns"]["InProgress"],
            json!([{"text": "b", "taker": "bob"}])
        );
        assert_eq!(read["columns"]["Done"], json!([]));
    }
}
