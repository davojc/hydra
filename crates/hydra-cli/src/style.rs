//! Named terminal styles. Output goes through `anstream`, which strips the escape codes when
//! the stream isn't a terminal and honours `NO_COLOR` and `CLICOLOR_FORCE`.
use std::fmt::Display;

use anstyle::{AnsiColor, Effects, RgbColor, Style};

pub const ERROR: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
pub const WARN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
pub const OK: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
pub const DIM: Style = Style::new().effects(Effects::DIMMED);

fn paint(style: Style, s: impl Display) -> String {
    format!("{style}{s}{style:#}")
}

pub fn error(s: impl Display) -> String {
    paint(ERROR, s)
}

pub fn warn(s: impl Display) -> String {
    paint(WARN, s)
}

pub fn ok(s: impl Display) -> String {
    paint(OK, s)
}

pub fn dim(s: impl Display) -> String {
    paint(DIM, s)
}

/// An environment's name in its own colour, or bold when it has none.
pub fn env_name(name: impl Display, rgb: Option<(u8, u8, u8)>) -> String {
    let style = match rgb {
        Some((r, g, b)) => Style::new().fg_color(Some(RgbColor(r, g, b).into())),
        None => Style::new().effects(Effects::BOLD),
    };
    paint(style, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_name_uses_the_24_bit_colour() {
        let s = env_name("work", Some((0x1f, 0x9a, 0x8a)));
        assert!(s.contains("38;2;31;154;138"), "{s:?}");
        assert!(s.contains("work"));
    }

    #[test]
    fn env_name_without_a_colour_is_bold() {
        assert!(env_name("work", None).starts_with("\x1b[1m"));
    }
}
