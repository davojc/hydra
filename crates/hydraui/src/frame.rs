//! A snapshot of what the terminal shows, built while holding the grid's lock and painted
//! after it is released (spec §7.2). Plain data, so it is tested without a window.
use hydraui_term::alacritty_terminal::Term;
use hydraui_term::alacritty_terminal::event::EventListener;
use hydraui_term::alacritty_terminal::grid::Dimensions;
use hydraui_term::alacritty_terminal::index::{Boundary, Column, Direction, Point};
use hydraui_term::alacritty_terminal::term::cell::Flags;
use hydraui_term::alacritty_terminal::term::search::{Match, RegexSearch};
use hydraui_term::alacritty_terminal::term::viewport_to_point;
use hydraui_term::alacritty_terminal::vte::ansi::CursorShape;

use crate::palette::{Palette, dim};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
}

/// A stretch of a row's text in one style; `len` is in UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub len: usize,
    pub style: Style,
}

/// A character drawn on its own at its column: a wide one (two cells) or one with combining
/// marks. Its cells hold spaces in the row text, so the rest of the row stays aligned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyph {
    pub col: usize,
    pub text: String,
    pub style: Style,
    pub wide: bool,
}

/// Columns `start..end` of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

/// One character per cell, so the text can be drawn with a fixed advance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub runs: Vec<Run>,
    pub glyphs: Vec<Glyph>,
    /// Cells whose background isn't the palette's.
    pub backgrounds: Vec<(Span, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Block,
    HollowBlock,
    Underline,
    Beam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub kind: CursorKind,
    /// On a wide character: two cells wide.
    pub wide: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub rows: Vec<Row>,
    pub cursor: Option<Cursor>,
    pub selection: Vec<Span>,
}

impl Row {
    fn push(&mut self, ch: char, style: Style) {
        let start = self.text.len();
        self.text.push(ch);
        let len = self.text.len() - start;
        match self.runs.last_mut() {
            Some(run) if run.style == style => run.len += len,
            _ => self.runs.push(Run { len, style }),
        }
    }
}

/// The visible screen (the history when the user has scrolled). An unfocused pane shows a
/// hollow cursor.
pub fn build_frame<T: EventListener>(term: &Term<T>, palette: &Palette, focused: bool) -> Frame {
    let content = term.renderable_content();
    let rows = term.screen_lines();
    let offset = content.display_offset as i32;
    let blank = Style {
        fg: palette.foreground,
        bold: false,
        italic: false,
        underline: false,
        strikeout: false,
    };
    let mut frame = Frame {
        rows: vec![Row::default(); rows],
        ..Default::default()
    };
    for indexed in content.display_iter {
        let line = indexed.point.line.0 + offset;
        if line < 0 || line as usize >= rows {
            continue;
        }
        let (r, col) = (line as usize, indexed.point.column.0);
        if content
            .selection
            .as_ref()
            .is_some_and(|s| s.contains(indexed.point))
        {
            match frame.selection.last_mut() {
                Some(s) if s.row == r && s.end == col => s.end = col + 1,
                _ => frame.selection.push(Span {
                    row: r,
                    start: col,
                    end: col + 1,
                }),
            }
        }
        let row = &mut frame.rows[r];
        let cell = indexed.cell;
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            let style = row.runs.last().map_or(blank, |run| run.style);
            row.push(' ', style);
            continue;
        }
        let mut fg = palette.resolve(cell.fg, content.colors);
        let mut bg = palette.resolve(cell.bg, content.colors);
        if flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if bg != palette.background {
            match row.backgrounds.last_mut() {
                Some((s, c)) if *c == bg && s.end == col => s.end = col + 1,
                _ => row.backgrounds.push((
                    Span {
                        row: r,
                        start: col,
                        end: col + 1,
                    },
                    bg,
                )),
            }
        }
        let style = Style {
            fg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            strikeout: flags.contains(Flags::STRIKEOUT),
        };
        let wide = flags.contains(Flags::WIDE_CHAR);
        if flags.contains(Flags::HIDDEN) {
            row.push(' ', style);
        } else if wide || cell.zerowidth().is_some() {
            let mut text = cell.c.to_string();
            text.extend(cell.zerowidth().unwrap_or_default());
            row.glyphs.push(Glyph {
                col,
                text,
                style,
                wide,
            });
            row.push(' ', style);
        } else {
            row.push(cell.c, style);
        }
    }
    let cursor = content.cursor;
    let line = cursor.point.line.0 + offset;
    frame.cursor = match cursor.shape {
        CursorShape::Hidden => None,
        _ if line < 0 || line as usize >= rows => None,
        shape => Some(Cursor {
            row: line as usize,
            col: cursor.point.column.0,
            kind: match shape {
                _ if !focused => CursorKind::HollowBlock,
                CursorShape::Underline => CursorKind::Underline,
                CursorShape::Beam => CursorKind::Beam,
                CursorShape::HollowBlock => CursorKind::HollowBlock,
                _ => CursorKind::Block,
            },
            wide: term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR),
        }),
    };
    frame
}

/// The OSC 8 link at a visible cell, if any.
pub fn link_at<T>(term: &Term<T>, row: usize, col: usize) -> Option<String> {
    if row >= term.screen_lines() || col >= term.columns() {
        return None;
    }
    let point = viewport_to_point(term.grid().display_offset(), Point::new(row, Column(col)));
    term.grid()[point].hyperlink().map(|h| h.uri().to_string())
}

/// The text of a visible row, for finding paths under the mouse.
pub fn row_text<T>(term: &Term<T>, row: usize) -> String {
    if row >= term.screen_lines() {
        return String::new();
    }
    let offset = term.grid().display_offset();
    (0..term.columns())
        .map(|col| term.grid()[viewport_to_point(offset, Point::new(row, Column(col)))].c)
        .collect()
}

/// `query` as a pattern that matches it literally: people type text, not regexes.
pub fn literal(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The next match of `query` above (`Direction::Left`) or below (`Right`) the current one,
/// starting at the cursor when there is none. The search wraps round the scrollback.
pub fn find_next<T>(
    term: &Term<T>,
    query: &str,
    current: Option<&Match>,
    direction: Direction,
) -> Option<Match> {
    if query.is_empty() {
        return None;
    }
    let mut regex = RegexSearch::new(&literal(query)).ok()?;
    let origin = match (current, direction) {
        (Some(m), Direction::Left) => m.start().sub(term, Boundary::None, 1),
        (Some(m), Direction::Right) => m.end().add(term, Boundary::None, 1),
        (None, _) => term.grid().cursor.point,
    };
    // Matches are compared to the origin by their start.
    term.search_next(&mut regex, origin, direction, Direction::Left, None)
}

#[cfg(test)]
mod tests {
    use hydraui_term::GridSize;
    use hydraui_term::alacritty_terminal::event::VoidListener;
    use hydraui_term::alacritty_terminal::grid::Scroll;
    use hydraui_term::alacritty_terminal::index::{Line, Side};
    use hydraui_term::alacritty_terminal::selection::{Selection, SelectionType};
    use hydraui_term::alacritty_terminal::term::Config;
    use hydraui_term::alacritty_terminal::vte::ansi::Processor;

    use super::*;
    use crate::palette::CAMPBELL;

    fn term(cols: u16, rows: u16, bytes: &[u8]) -> Term<VoidListener> {
        let mut t = Term::new(Config::default(), &GridSize { cols, rows }, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut t, bytes);
        t
    }

    fn frame(t: &Term<VoidListener>) -> Frame {
        build_frame(t, &Palette::default(), true)
    }

    #[test]
    fn plain_text_is_one_char_per_cell() {
        let f = frame(&term(6, 2, b"hi"));
        assert_eq!(f.rows.len(), 2);
        assert_eq!(f.rows[0].text, "hi    ");
        assert_eq!(f.rows[0].runs.len(), 1);
        assert_eq!(
            f.cursor,
            Some(Cursor {
                row: 0,
                col: 2,
                kind: CursorKind::Block,
                wide: false
            })
        );
    }

    #[test]
    fn colours_and_styles() {
        let f = frame(&term(
            8,
            1,
            b"\x1b[31;1mR\x1b[0;3;4mI\x1b[0;9mS\x1b[0;44m \x1b[0m",
        ));
        let runs = &f.rows[0].runs;
        assert_eq!(
            runs[0].style,
            Style {
                fg: CAMPBELL[1],
                bold: true,
                italic: false,
                underline: false,
                strikeout: false
            }
        );
        assert!(runs[1].style.italic && runs[1].style.underline);
        assert!(runs[2].style.strikeout);
        assert_eq!(
            f.rows[0].backgrounds,
            vec![(
                Span {
                    row: 0,
                    start: 3,
                    end: 4
                },
                CAMPBELL[4]
            )]
        );
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let p = Palette::default();
        let f = frame(&term(4, 1, b"\x1b[7mX"));
        assert_eq!(f.rows[0].runs[0].style.fg, p.background);
        assert_eq!(
            f.rows[0].backgrounds,
            vec![(
                Span {
                    row: 0,
                    start: 0,
                    end: 1
                },
                p.foreground
            )]
        );
    }

    #[test]
    fn wide_and_combining_characters_keep_the_columns_aligned() {
        let f = frame(&term(8, 1, "日x e\u{301}y".as_bytes()));
        let row = &f.rows[0];
        // 日 fills columns 0-1, x is column 2, é column 4, y column 5.
        assert_eq!(&row.text[..6], "  x  y");
        assert_eq!(
            row.glyphs[0],
            Glyph {
                col: 0,
                text: "日".into(),
                style: row.runs[0].style,
                wide: true
            }
        );
        assert_eq!(row.glyphs[1].col, 4);
        assert_eq!(row.glyphs[1].text, "e\u{301}");
        assert!(!row.glyphs[1].wide);
        assert_eq!(f.cursor.unwrap().col, 6);
    }

    #[test]
    fn cursor_shapes_focus_and_hiding() {
        assert_eq!(
            frame(&term(4, 1, b"\x1b[6 q")).cursor.unwrap().kind,
            CursorKind::Beam
        );
        assert_eq!(
            frame(&term(4, 1, b"\x1b[4 q")).cursor.unwrap().kind,
            CursorKind::Underline
        );
        assert_eq!(
            build_frame(&term(4, 1, b""), &Palette::default(), false)
                .cursor
                .unwrap()
                .kind,
            CursorKind::HollowBlock
        );
        assert_eq!(frame(&term(4, 1, b"\x1b[?25l")).cursor, None);
    }

    #[test]
    fn selection_is_reported_per_row() {
        let mut t = term(8, 2, b"abcdef");
        let mut s = Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(1)),
            Side::Left,
        );
        s.update(Point::new(Line(0), Column(3)), Side::Right);
        t.selection = Some(s);
        assert_eq!(
            frame(&t).selection,
            vec![Span {
                row: 0,
                start: 1,
                end: 4
            }]
        );
    }

    #[test]
    fn scrolled_history_is_shown_and_the_cursor_hides() {
        let mut t = term(4, 2, b"one\r\ntwo\r\nsix");
        t.scroll_display(Scroll::Delta(1));
        let f = frame(&t);
        assert_eq!(f.rows[0].text.trim_end(), "one");
        assert_eq!(f.rows[1].text.trim_end(), "two");
        assert_eq!(f.cursor, None);
    }

    #[test]
    fn hidden_text_is_blank() {
        assert_eq!(frame(&term(4, 1, b"\x1b[8mpw")).rows[0].text, "    ");
    }

    #[test]
    fn links_are_found_under_their_cells() {
        let t = term(
            10,
            1,
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ x",
        );
        assert_eq!(link_at(&t, 0, 0).as_deref(), Some("https://example.com"));
        assert_eq!(link_at(&t, 0, 3).as_deref(), Some("https://example.com"));
        assert_eq!(link_at(&t, 0, 5), None);
        assert_eq!(link_at(&t, 3, 0), None);
        assert_eq!(link_at(&t, 0, 30), None);
    }

    #[test]
    fn find_walks_up_and_wraps_round() {
        let t = term(20, 2, b"foo bar foo");
        let a = find_next(&t, "foo", None, Direction::Left).unwrap();
        assert_eq!(a.start().column, Column(8));
        let b = find_next(&t, "foo", Some(&a), Direction::Left).unwrap();
        assert_eq!(b.start().column, Column(0));
        let c = find_next(&t, "foo", Some(&b), Direction::Left).unwrap();
        assert_eq!(c.start().column, Column(8));
        assert_eq!(find_next(&t, "", None, Direction::Left), None);
        assert_eq!(find_next(&t, "zzz", None, Direction::Left), None);
    }

    #[test]
    fn find_is_literal() {
        let t = term(20, 1, b"axb a.b");
        let m = find_next(&t, "a.b", None, Direction::Left).unwrap();
        assert_eq!(m.start().column, Column(4));
        assert_eq!(literal("(1+1)"), "\\(1\\+1\\)");
    }
}
