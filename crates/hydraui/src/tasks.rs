//! A project's task board file (spec: docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
//! §2): `<project>/.hydra/tasks.md`, five headed lists of one-line cards, parsed forgivingly and
//! written back in one canonical shape. Anything that isn't a card is kept verbatim in `extra`.

/// Written lines wrap so none is longer than this many columns.
const WRAP_WIDTH: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    Backlog,
    Pickup,
    InProgress,
    /// Work committed and waiting for the user to check it.
    Verify,
    Done,
}

impl Column {
    pub const ALL: [Column; 5] = [
        Column::Backlog,
        Column::Pickup,
        Column::InProgress,
        Column::Verify,
        Column::Done,
    ];

    /// Whether this column's cards record who took them (MCP server spec §4).
    pub fn has_takers(self) -> bool {
        matches!(self, Column::InProgress | Column::Verify | Column::Done)
    }

    pub fn heading(self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Pickup => "Pickup",
            Column::InProgress => "InProgress",
            Column::Verify => "Verify",
            Column::Done => "Done",
        }
    }
}

/// One card: its text and, in InProgress, Verify and Done, the terminal that took it (MCP server spec §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub text: String,
    pub taker: Option<String>,
    /// The commit holding the card's work, given when it moved to Verify; written after the taker
    /// as `[taker @commit]`.
    pub commit: Option<String>,
}

impl Card {
    pub fn new(text: impl Into<String>) -> Card {
        Card {
            text: text.into(),
            taker: None,
            commit: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    /// Cards per column, indexed by `Column as usize`, top first.
    pub columns: [Vec<Card>; 5],
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
    pub fn cards(&self, column: Column) -> &[Card] {
        &self.columns[column as usize]
    }

    pub fn card_ref(&self, column: Column, index: usize) -> Option<CardRef> {
        let text = &self.cards(column).get(index)?.text;
        let nth = self.cards(column)[..index]
            .iter()
            .filter(|c| c.text == *text)
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
            .filter(|(_, c)| c.text == card.text)
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
    /// The first Pickup card with this text goes to the top of InProgress, taken by `taker`.
    Take {
        text: String,
        taker: String,
    },
    /// The first InProgress card with this text that `taker` took goes to the top of Verify,
    /// recording `commit`.
    Verify {
        text: String,
        taker: String,
        commit: Option<String>,
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

/// A leading `[name] ` and the text after it. `[x]`, `[X]` and `[ ]` are checkboxes, not names,
/// and a name needs text after it; otherwise the whole text is the card.
fn split_taker(text: &str) -> (Option<String>, &str) {
    let Some(rest) = text.strip_prefix('[') else {
        return (None, text);
    };
    match rest.split_once("] ") {
        Some((name, card))
            if !name.is_empty()
                && !name.contains(']')
                && !["x", "X", " "].contains(&name)
                && !card.trim().is_empty() =>
        {
            (Some(name.to_string()), card.trim())
        }
        _ => (None, text),
    }
}

/// A taker's `name @commit`: the name and a commit of 4 to 40 hex digits after ` @`, else the
/// whole taker as the name.
fn split_commit(taker: String) -> (String, Option<String>) {
    match taker.rsplit_once(" @") {
        Some((name, hash))
            if !name.is_empty()
                && (4..=40).contains(&hash.len())
                && hash.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            (name.to_string(), Some(hash.to_string()))
        }
        _ => (taker, None),
    }
}

/// In InProgress, Verify and Done, a leading `\` before `[` (after any further `\`) marks the text as
/// literal, not a taker (MCP server spec §4); the text without that one `\`.
fn unescape(text: &str) -> Option<&str> {
    text.strip_prefix('\\')
        .filter(|rest| rest.trim_start_matches('\\').starts_with('['))
}

/// Whether a card without a taker in InProgress, Verify or Done must be written escaped so it reads
/// back as the same text: it would read as a taker, a checkbox, or an escape.
fn needs_escape(text: &str) -> bool {
    split_taker(text).0.is_some() || strip_checkbox(text) != text || unescape(text).is_some()
}

/// Ends the card being read, if any, filing it under `column`.
fn flush(board: &mut Board, column: Option<Column>, card: &mut Option<String>) {
    if let (Some(col), Some(text)) = (column, card.take()) {
        let text = normalise(&text);
        if text.is_empty() {
            return;
        }
        let (taker, text) = if col.has_takers() {
            match unescape(&text) {
                Some(literal) => (None, literal),
                None => split_taker(&text),
            }
        } else {
            (None, text.as_str())
        };
        let (taker, commit) = match taker.map(split_commit) {
            Some((name, commit)) => (Some(name), commit),
            None => (None, None),
        };
        board.columns[col as usize].push(Card {
            text: text.to_string(),
            taker,
            commit,
        });
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
        let takes = column.has_takers();
        for card in board.cards(column) {
            match (&card.taker, &card.commit) {
                (Some(taker), Some(commit)) => {
                    write_card(&mut out, &format!("[{taker} @{commit}] {}", card.text))
                }
                (Some(taker), None) => write_card(&mut out, &format!("[{taker}] {}", card.text)),
                (None, _) if takes && needs_escape(&card.text) => {
                    write_card(&mut out, &format!("\\{}", card.text))
                }
                (None, _) => write_card(&mut out, &card.text),
            }
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
                board.columns[Column::Backlog as usize].insert(0, Card::new(text));
            }
        }
        Change::Edit { card, text } => {
            let at = board.find(card).ok_or(Missing)?;
            let text = normalise(text);
            let cards = &mut board.columns[card.column as usize];
            if text.is_empty() {
                cards.remove(at);
            } else {
                cards[at].text = text;
            }
        }
        Change::Move { card, to, index } => {
            let at = board.find(card).ok_or(Missing)?;
            let mut moved = board.columns[card.column as usize].remove(at);
            if matches!(to, Column::Backlog | Column::Pickup) {
                moved.taker = None;
                moved.commit = None;
            }
            let target = &mut board.columns[*to as usize];
            target.insert((*index).min(target.len()), moved);
        }
        Change::Take { text, taker } => {
            let text = normalise(text);
            let pickup = &mut board.columns[Column::Pickup as usize];
            let at = pickup.iter().position(|c| c.text == text).ok_or(Missing)?;
            let mut card = pickup.remove(at);
            card.taker = Some(taker.clone());
            board.columns[Column::InProgress as usize].insert(0, card);
        }
        Change::Verify {
            text,
            taker,
            commit,
        } => {
            let text = normalise(text);
            let doing = &mut board.columns[Column::InProgress as usize];
            let at = doing
                .iter()
                .position(|c| c.text == text && c.taker.as_deref() == Some(taker.as_str()))
                .ok_or(Missing)?;
            let mut card = doing.remove(at);
            card.commit = commit.clone();
            board.columns[Column::Verify as usize].insert(0, card);
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

    const CANONICAL: &str = "# Tasks\n\n## Backlog\n- Let the terminal list show unread output.\n\n## Pickup\n- Ctrl+K opens the launcher.\n\n## InProgress\n\n## Verify\n\n## Done\n- Collapse button.\n";

    #[test]
    fn canonical_file_parses() {
        let b = parse(CANONICAL);
        assert_eq!(
            b.cards(Column::Backlog),
            [Card::new("Let the terminal list show unread output.")]
        );
        assert_eq!(
            b.cards(Column::Pickup),
            [Card::new("Ctrl+K opens the launcher.")]
        );
        assert!(b.cards(Column::InProgress).is_empty());
        assert_eq!(b.cards(Column::Done), [Card::new("Collapse button.")]);
        assert_eq!(b.extra, "");
    }

    #[test]
    fn canonical_file_round_trips() {
        assert_eq!(write(&parse(CANONICAL)), CANONICAL);
    }

    #[test]
    fn empty_board_writes_all_five_columns() {
        assert_eq!(
            write(&Board::default()),
            "# Tasks\n\n## Backlog\n\n## Pickup\n\n## InProgress\n\n## Verify\n\n## Done\n"
        );
    }

    #[test]
    fn a_board_from_before_verify_gains_the_column() {
        let old = "# Tasks\n\n## Backlog\n\n## Pickup\n\n## InProgress\n- [c] a\n\n## Done\n- b\n";
        let b = parse(old);
        assert!(b.cards(Column::Verify).is_empty());
        assert_eq!(b.cards(Column::Done), [Card::new("b")]);
        assert!(write(&b).contains("- [c] a\n\n## Verify\n\n## Done\n- b\n"));
    }

    #[test]
    fn verify_moves_only_the_takers_own_card_and_keeps_its_name() {
        let mut b = parse("## InProgress\n- [bob] same\n- [c] same\n- [c] other\n");
        let verify = |text: &str, taker: &str| Change::Verify {
            text: text.into(),
            taker: taker.into(),
            commit: None,
        };
        apply(&mut b, &verify("same", "c")).unwrap();
        assert_eq!(
            b.cards(Column::InProgress),
            [
                Card {
                    text: "same".into(),
                    taker: Some("bob".into()),
                    commit: None
                },
                Card {
                    text: "other".into(),
                    taker: Some("c".into()),
                    commit: None
                }
            ]
        );
        assert_eq!(b.cards(Column::Verify)[0].taker.as_deref(), Some("c"));
        assert_eq!(apply(&mut b, &verify("same", "c")), Err(Missing), "bob's");
        assert_eq!(apply(&mut b, &verify("gone", "c")), Err(Missing));
        let out = write(&b);
        assert!(out.contains("## Verify\n- [c] same\n"), "{out}");
        assert_eq!(parse(&out), b);
    }

    #[test]
    fn verify_records_the_commit_beside_the_taker() {
        let mut b = parse(
            "## InProgress
- [my term] same
",
        );
        let verify = Change::Verify {
            text: "same".into(),
            taker: "my term".into(),
            commit: Some("5e9a827".into()),
        };
        apply(&mut b, &verify).unwrap();
        assert_eq!(
            b.cards(Column::Verify),
            [Card {
                text: "same".into(),
                taker: Some("my term".into()),
                commit: Some("5e9a827".into()),
            }]
        );
        let out = write(&b);
        assert!(
            out.contains(
                "## Verify
- [my term @5e9a827] same
"
            ),
            "{out}"
        );
        assert_eq!(parse(&out), b);
        // Back to Backlog or Pickup the taker goes, and its commit with it.
        let card = b.card_ref(Column::Verify, 0).unwrap();
        apply(
            &mut b,
            &Change::Move {
                card,
                to: Column::Pickup,
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Pickup), [Card::new("same")]);
    }

    #[test]
    fn only_a_hex_word_after_at_is_a_commit() {
        let b = parse(
            "## Verify
- [a @beef12] x
- [b @main] y
- [c@beef12] z
",
        );
        let got: Vec<_> = b
            .cards(Column::Verify)
            .iter()
            .map(|c| (c.taker.as_deref(), c.commit.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (Some("a"), Some("beef12")),
                (Some("b @main"), None),
                (Some("c@beef12"), None)
            ]
        );
    }

    #[test]
    fn long_cards_wrap_at_100_columns_with_two_space_continuation() {
        let mut b = Board::default();
        let text = "word ".repeat(40).trim().to_string();
        b.columns[0].push(Card::new(text.clone()));
        let out = write(&b);
        assert!(out.lines().all(|l| l.chars().count() <= 100));
        assert!(out.lines().any(|l| l.starts_with("  word")));
        assert_eq!(parse(&out).cards(Column::Backlog), [Card::new(text)]);
    }

    #[test]
    fn forgiving_variants_keep_every_card() {
        let text = "## backlog\n- wrapped\n  over two lines\n* star idea\n+ plus idea\n- [ ] box idea\n- [x] done box\n## In Progress\n- parent\n  - nested child\n## Done\n";
        let b = parse(text);
        assert_eq!(
            b.cards(Column::Backlog),
            [
                Card::new("wrapped over two lines"),
                Card::new("star idea"),
                Card::new("plus idea"),
                Card::new("box idea"),
                Card::new("done box")
            ]
        );
        assert_eq!(
            b.cards(Column::InProgress),
            [Card::new("parent nested child")]
        );
        assert!(b.cards(Column::Pickup).is_empty());
    }

    #[test]
    fn extra_text_survives_a_round_trip() {
        let text = "Intro line\n\n## Backlog\n- a\nloose prose\n## Notes\n- not a card\n";
        let b = parse(text);
        assert_eq!(b.cards(Column::Backlog), [Card::new("a")]);
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
            assert_eq!(b2.cards(Column::Backlog).first().unwrap().text, "a");
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
        assert_eq!(
            b.cards(Column::Backlog),
            [Card::new("same"), Card::new("other")]
        );
        assert_eq!(b.cards(Column::Done), [Card::new("same")]);
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
        assert_eq!(b.cards(Column::Backlog)[0].text, "new idea");
        let r = b.card_ref(Column::Pickup, 0).unwrap();
        apply(
            &mut b,
            &Change::Edit {
                card: r,
                text: "edited".into(),
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Pickup), [Card::new("edited")]);
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
        assert_eq!(b.cards(Column::Backlog).last().unwrap().text, "new idea");
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
        assert_eq!(fresh.cards(Column::Pickup), [Card::new("from claude")]);
        assert_eq!(
            fresh.cards(Column::InProgress),
            [Card::new("Ctrl+K opens the launcher.")]
        );
    }

    #[test]
    fn a_change_to_a_vanished_card_is_missing() {
        let seen = parse(CANONICAL);
        let card = seen.card_ref(Column::Done, 0).unwrap();
        let mut fresh = parse("## Done\n");
        assert_eq!(apply(&mut fresh, &Change::Delete { card }), Err(Missing));
    }

    #[test]
    fn takers_are_read_in_inprogress_and_done_only() {
        let b = parse(
            "## Backlog
- [WIP] still text
## Pickup
- [x] boxed
## InProgress
- [claude] doing it
- [ ] boxed too
## Done
- [api-2] did it
",
        );
        assert_eq!(b.cards(Column::Backlog), [Card::new("[WIP] still text")]);
        assert_eq!(b.cards(Column::Pickup), [Card::new("boxed")]);
        assert_eq!(
            b.cards(Column::InProgress)[0],
            Card {
                text: "doing it".into(),
                taker: Some("claude".into()),
                commit: None
            }
        );
        assert_eq!(b.cards(Column::InProgress)[1], Card::new("boxed too"));
        assert_eq!(b.cards(Column::Done)[0].taker.as_deref(), Some("api-2"));
    }

    #[test]
    fn takers_round_trip() {
        let text = "# Tasks

## Backlog

## Pickup

## InProgress
- [my term] doing it

## Verify

## Done
";
        assert_eq!(write(&parse(text)), text);
    }

    #[test]
    fn take_moves_the_first_match_to_the_top_of_inprogress() {
        let mut b = parse(
            "## Pickup
- a
- the  card
  wrapped
- the card wrapped
## InProgress
- [x1] old
",
        );
        apply(
            &mut b,
            &Change::Take {
                text: "the card
wrapped "
                    .into(),
                taker: "claude".into(),
            },
        )
        .unwrap();
        assert_eq!(
            b.cards(Column::Pickup),
            [Card::new("a"), Card::new("the card wrapped")]
        );
        assert_eq!(
            b.cards(Column::InProgress)[0],
            Card {
                text: "the card wrapped".into(),
                taker: Some("claude".into()),
                commit: None
            }
        );
        assert_eq!(
            apply(
                &mut b,
                &Change::Take {
                    text: "gone".into(),
                    taker: "c".into()
                }
            ),
            Err(Missing)
        );
    }

    #[test]
    fn moving_back_drops_the_taker_and_done_keeps_it() {
        let mut b = parse(
            "## InProgress
- [c] one
- [c] two
",
        );
        let one = b.card_ref(Column::InProgress, 0).unwrap();
        apply(
            &mut b,
            &Change::Move {
                card: one,
                to: Column::Pickup,
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Pickup), [Card::new("one")]);
        let two = b.card_ref(Column::InProgress, 0).unwrap();
        assert_eq!(two.text, "two");
        apply(
            &mut b,
            &Change::Move {
                card: two,
                to: Column::Done,
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(b.cards(Column::Done)[0].taker.as_deref(), Some("c"));
    }

    #[test]
    fn text_that_looks_like_a_taker_is_escaped() {
        let mut b = parse("## Backlog\n- [WIP] fix login\n");
        let card = b.card_ref(Column::Backlog, 0).unwrap();
        let to_inprogress = Change::Move {
            card,
            to: Column::InProgress,
            index: 0,
        };
        apply(&mut b, &to_inprogress).unwrap();
        let out = write(&b);
        assert!(out.contains("\n- \\[WIP] fix login\n"), "{out}");
        let mut b = parse(&out);
        assert_eq!(b.cards(Column::InProgress), [Card::new("[WIP] fix login")]);
        let card = b.card_ref(Column::InProgress, 0).unwrap();
        let back = Change::Move {
            card,
            to: Column::Backlog,
            index: 0,
        };
        apply(&mut b, &back).unwrap();
        let out = write(&b);
        assert!(out.contains("\n- [WIP] fix login\n"), "{out}");
        assert_eq!(
            parse(&out).cards(Column::Backlog),
            [Card::new("[WIP] fix login")]
        );
    }

    #[test]
    fn escaped_text_reads_literally_in_inprogress_and_done() {
        let b = parse(
            r"## InProgress
- [bob] [WIP] real
- \[c] d
## Done
- \[x] y
- \\[e] f
",
        );
        assert_eq!(
            b.cards(Column::InProgress),
            [
                Card {
                    text: "[WIP] real".into(),
                    taker: Some("bob".into()),
                    commit: None
                },
                Card::new("[c] d")
            ]
        );
        assert_eq!(
            b.cards(Column::Done),
            [Card::new("[x] y"), Card::new(r"\[e] f")]
        );
        assert_eq!(parse(&write(&b)), b);
    }
}
