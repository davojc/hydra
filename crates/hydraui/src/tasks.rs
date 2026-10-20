//! A project's task board file (spec: docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
//! §2): `<project>/.hydra/tasks.md`, four headed lists of one-line cards, parsed forgivingly and
//! written back in one canonical shape. Anything that isn't a card is kept verbatim in `extra`.

/// Written lines wrap so none is longer than this many columns.
const WRAP_WIDTH: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    Backlog,
    Pickup,
    InProgress,
    Done,
}

impl Column {
    pub const ALL: [Column; 4] = [
        Column::Backlog,
        Column::Pickup,
        Column::InProgress,
        Column::Done,
    ];

    pub fn heading(self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Pickup => "Pickup",
            Column::InProgress => "InProgress",
            Column::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    /// Cards per column, indexed by `Column as usize`, top first.
    pub columns: [Vec<String>; 4],
    /// Everything that isn't a card under a known column, verbatim, in original order.
    pub extra: String,
}

/// A card by its column, text and which occurrence of that text it is in the column (0-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRef {
    pub column: Column,
    pub text: String,
    pub nth: usize,
}

impl Board {
    pub fn cards(&self, column: Column) -> &[String] {
        &self.columns[column as usize]
    }

    pub fn card_ref(&self, column: Column, index: usize) -> Option<CardRef> {
        let text = self.cards(column).get(index)?;
        let nth = self.cards(column)[..index]
            .iter()
            .filter(|t| *t == text)
            .count();
        Some(CardRef {
            column,
            text: text.clone(),
            nth,
        })
    }

    /// Where `card` is now (its index in its column), if it still is: the `nth` card in its
    /// column with its text (§6).
    pub fn find(&self, card: &CardRef) -> Option<usize> {
        self.cards(card.column)
            .iter()
            .enumerate()
            .filter(|(_, t)| **t == card.text)
            .nth(card.nth)
            .map(|(i, _)| i)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// A new card at the top of Backlog.
    Add {
        text: String,
    },
    Edit {
        card: CardRef,
        text: String,
    },
    /// To `index` in `to` (clamped to the column's length).
    Move {
        card: CardRef,
        to: Column,
        index: usize,
    },
    Delete {
        card: CardRef,
    },
    ClearDone,
}

/// The card a change names isn't in the board any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing;

/// The column a `## ` heading names, ignoring case and spaces ("In Progress" is InProgress).
fn column_for(heading: &str) -> Option<Column> {
    let key: String = heading
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    Column::ALL
        .into_iter()
        .find(|c| c.heading().to_lowercase() == key)
}

/// A heading line: its level and text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    let rest = line[level..].strip_prefix(' ')?;
    (level > 0).then(|| (level, rest.trim()))
}

fn strip_marker(line: &str) -> Option<&str> {
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|m| line.strip_prefix(m))
        .map(str::trim)
}

fn strip_checkbox(text: &str) -> &str {
    ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find_map(|b| text.strip_prefix(b))
        .unwrap_or(text)
}

/// Trimmed, with whitespace runs (newlines included) collapsed to one space.
pub fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Ends the card being read, if any, filing it under `column`.
fn flush(board: &mut Board, column: Option<Column>, card: &mut Option<String>) {
    if let (Some(col), Some(text)) = (column, card.take()) {
        let text = normalise(&text);
        if !text.is_empty() {
            board.columns[col as usize].push(text);
        }
    }
}

pub fn parse(text: &str) -> Board {
    let mut board = Board::default();
    let mut extra: Vec<&str> = Vec::new();
    let mut column: Option<Column> = None;
    let mut card: Option<String> = None;
    // Blank lines inside a column are only extra when more extra follows them.
    let mut blanks = 0;

    for line in text.lines() {
        if let Some((level, name)) = heading(line) {
            flush(&mut board, column, &mut card);
            blanks = 0;
            column = if level == 2 { column_for(name) } else { None };
            if column.is_none() && !(level == 1 && name == "Tasks") {
                extra.push(line);
            }
        } else if column.is_none() {
            extra.push(line);
        } else if line.trim().is_empty() {
            flush(&mut board, column, &mut card);
            blanks += 1;
        } else if let Some(first) = strip_marker(line) {
            flush(&mut board, column, &mut card);
            blanks = 0;
            card = Some(strip_checkbox(first).to_string());
        } else if let (true, Some(card)) = (line.starts_with(char::is_whitespace), card.as_mut()) {
            let more = strip_marker(line.trim_start()).unwrap_or_else(|| line.trim());
            card.push(' ');
            card.push_str(more);
        } else {
            flush(&mut board, column, &mut card);
            extra.extend(std::iter::repeat_n("", blanks));
            blanks = 0;
            extra.push(line);
        }
    }
    flush(&mut board, column, &mut card);

    // Leading and trailing blank lines aren't kept, so `extra` is stable across a round trip.
    let blank = |l: &&str| l.trim().is_empty();
    let start = extra.iter().position(|l| !blank(l)).unwrap_or(extra.len());
    let end = extra
        .iter()
        .rposition(|l| !blank(l))
        .map_or(start, |i| i + 1);
    board.extra = extra[start..end].join("\n");
    board
}

/// One card as a `- ` line wrapped at `WRAP_WIDTH`, continuation lines indented two spaces.
fn write_card(out: &mut String, text: &str) {
    let mut line = String::from("-");
    let mut width = 1;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if width > 1 && width + 1 + len > WRAP_WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from(" ");
            width = 1;
        }
        line.push(' ');
        line.push_str(word);
        width += 1 + len;
    }
    out.push_str(&line);
    out.push('\n');
}

pub fn write(board: &Board) -> String {
    let mut out = String::from("# Tasks\n");
    for column in Column::ALL {
        out.push_str("\n## ");
        out.push_str(column.heading());
        out.push('\n');
        for card in board.cards(column) {
            write_card(&mut out, card);
        }
    }
    if !board.extra.is_empty() {
        out.push('\n');
        // Under a bare `## Done` list a stray bullet would read back as a card, so unheaded
        // extra gets a heading of its own (which parses as extra again).
        if !board
            .extra
            .lines()
            .next()
            .is_some_and(|l| heading(l).is_some())
        {
            out.push_str("## Notes\n");
        }
        out.push_str(&board.extra);
        out.push('\n');
    }
    out
}

/// Applies `change`; an `Edit` to empty text deletes the card; text is normalised (trimmed,
/// whitespace runs incl. newlines collapsed to one space); an `Add` of empty text is a no-op.
pub fn apply(board: &mut Board, change: &Change) -> Result<(), Missing> {
    match change {
        Change::Add { text } => {
            let text = normalise(text);
            if !text.is_empty() {
                board.columns[Column::Backlog as usize].insert(0, text);
            }
        }
        Change::Edit { card, text } => {
            let at = board.find(card).ok_or(Missing)?;
            let text = normalise(text);
            let cards = &mut board.columns[card.column as usize];
            if text.is_empty() {
                cards.remove(at);
            } else {
                cards[at] = text;
            }
        }
        Change::Move { card, to, index } => {
            let at = board.find(card).ok_or(Missing)?;
            let text = board.columns[card.column as usize].remove(at);
            let target = &mut board.columns[*to as usize];
            target.insert((*index).min(target.len()), text);
        }
        Change::Delete { card } => {
            let at = board.find(card).ok_or(Missing)?;
            board.columns[card.column as usize].remove(at);
        }
        Change::ClearDone => board.columns[Column::Done as usize].clear(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "# Tasks\n\n## Backlog\n- Let the terminal list show unread output.\n\n## Pickup\n- Ctrl+K opens the launcher.\n\n## InProgress\n\n## Done\n- Collapse button.\n";

    #[test]
    fn canonical_file_parses() {
        let b = parse(CANONICAL);
        assert_eq!(
            b.cards(Column::Backlog),
            ["Let the terminal list show unread output."]
        );
        assert_eq!(b.cards(Column::Pickup), ["Ctrl+K opens the launcher."]);
        assert!(b.cards(Column::InProgress).is_empty());
        assert_eq!(b.cards(Column::Done), ["Collapse button."]);
        assert_eq!(b.extra, "");
    }

    #[test]
    fn canonical_file_round_trips() {
        assert_eq!(write(&parse(CANONICAL)), CANONICAL);
    }

    #[test]
    fn empty_board_writes_all_four_columns() {
        assert_eq!(
            write(&Board::default()),
            "# Tasks\n\n## Backlog\n\n## Pickup\n\n## InProgress\n\n## Done\n"
        );
    }

    #[test]
    fn long_cards_wrap_at_100_columns_with_two_space_continuation() {
        let mut b = Board::default();
        let text = "word ".repeat(40).trim().to_string();
        b.columns[0].push(text.clone());
        let out = write(&b);
        assert!(out.lines().all(|l| l.chars().count() <= 100));
        assert!(out.lines().any(|l| l.starts_with("  word")));
        assert_eq!(parse(&out).cards(Column::Backlog), [text]);
    }

    #[test]
    fn forgiving_variants_keep_every_card() {
        let text = "## backlog\n- wrapped\n  over two lines\n* star idea\n+ plus idea\n- [ ] box idea\n- [x] done box\n## In Progress\n- parent\n  - nested child\n## Done\n";
        let b = parse(text);
        assert_eq!(
            b.cards(Column::Backlog),
            [
                "wrapped over two lines",
                "star idea",
                "plus idea",
                "box idea",
                "done box"
            ]
        );
        assert_eq!(b.cards(Column::InProgress), ["parent nested child"]);
        assert!(b.cards(Column::Pickup).is_empty());
    }

    #[test]
    fn extra_text_survives_a_round_trip() {
        let text = "Intro line\n\n## Backlog\n- a\nloose prose\n## Notes\n- not a card\n";
        let b = parse(text);
        assert_eq!(b.cards(Column::Backlog), ["a"]);
        assert!(b.extra.contains("Intro line") && b.extra.contains("loose prose"));
        assert!(b.extra.contains("## Notes\n- not a card"));
        let again = parse(&write(&b));
        assert_eq!(again.columns, b.columns);
        assert!(again.extra.starts_with("## Notes\n"));
        assert!(again.extra.contains("Intro line") && again.extra.contains("loose prose"));
        assert_eq!(parse(&write(&again)), again);
    }

    #[test]
    fn extra_bullets_never_become_done_cards() {
        for text in [
            "Notes:\n- x\n\n## Backlog\n- a\n",
            "## Backlog\n- a\n\nsome prose\n- b\n",
        ] {
            let b = parse(text);
            assert!(!b.extra.is_empty());
            let b2 = parse(&write(&b));
            assert!(b2.cards(Column::Done).is_empty(), "{text:?}");
            assert_eq!(b2.cards(Column::Backlog).first().unwrap(), "a");
            assert_eq!(parse(&write(&b2)), b2);
        }
    }

    #[test]
    fn identical_cards_are_told_apart_by_occurrence() {
        let mut b = parse("## Backlog\n- same\n- other\n- same\n");
        let second = b.card_ref(Column::Backlog, 2).unwrap();
        assert_eq!(second.nth, 1);
        apply(
            &mut b,
            &Change::Move {
                card: second,
                to: Column::Done,
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Backlog), ["same", "other"]);
        assert_eq!(b.cards(Column::Done), ["same"]);
    }

    #[test]
    fn changes_do_what_they_say() {
        let mut b = parse(CANONICAL);
        apply(
            &mut b,
            &Change::Add {
                text: "  new\n idea ".into(),
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Backlog)[0], "new idea");
        let r = b.card_ref(Column::Pickup, 0).unwrap();
        apply(
            &mut b,
            &Change::Edit {
                card: r,
                text: "edited".into(),
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Pickup), ["edited"]);
        let r = b.card_ref(Column::Pickup, 0).unwrap();
        apply(
            &mut b,
            &Change::Edit {
                card: r,
                text: "   ".into(),
            },
        )
        .unwrap();
        assert!(b.cards(Column::Pickup).is_empty(), "empty edit deletes");
        let r = b.card_ref(Column::Backlog, 0).unwrap();
        apply(
            &mut b,
            &Change::Move {
                card: r,
                to: Column::Backlog,
                index: 99,
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Backlog).last().unwrap(), "new idea");
        apply(&mut b, &Change::ClearDone).unwrap();
        assert!(b.cards(Column::Done).is_empty());
        apply(&mut b, &Change::Add { text: "".into() }).unwrap();
        assert_eq!(b.cards(Column::Backlog).len(), 2, "empty add is a no-op");
    }

    #[test]
    fn a_change_applies_to_a_file_changed_underneath() {
        // The board read CANONICAL; meanwhile Claude added a card to Pickup.
        let seen = parse(CANONICAL);
        let card = seen.card_ref(Column::Pickup, 0).unwrap();
        let mut fresh = parse(&CANONICAL.replace("## Pickup\n", "## Pickup\n- from claude\n"));
        apply(
            &mut fresh,
            &Change::Move {
                card,
                to: Column::InProgress,
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(fresh.cards(Column::Pickup), ["from claude"]);
        assert_eq!(
            fresh.cards(Column::InProgress),
            ["Ctrl+K opens the launcher."]
        );
    }

    #[test]
    fn a_change_to_a_vanished_card_is_missing() {
        let seen = parse(CANONICAL);
        let card = seen.card_ref(Column::Done, 0).unwrap();
        let mut fresh = parse("## Done\n");
        assert_eq!(apply(&mut fresh, &Change::Delete { card }), Err(Missing));
    }
}
