//! Terminal colours (spec §7.2): the 16 ANSI colours plus foreground, background, cursor and
//! selection, from `[colors]` over Campbell (Windows Terminal's default scheme); and each
//! environment's tint (spec §7.1). Colours are 0xRRGGBB.
use hydra_core::config::Rgb as HydraRgb;
use hydraui_term::alacritty_terminal::term::color::Colors;
use hydraui_term::alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use crate::config::ColorsConfig;

/// Campbell, black to bright white.
pub const CAMPBELL: [u32; 16] = [
    0x0c0c0c, 0xc50f1f, 0x13a10e, 0xc19c00, 0x0037da, 0x881798, 0x3a96dd, 0xcccccc, 0x767676,
    0xe74856, 0x16c60c, 0xf9f1a5, 0x3b78ff, 0xb4009e, 0x61d6d6, 0xf2f2f2,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub selection: u32,
    pub ansi: [u32; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: 0xcccccc,
            background: 0x0c0c0c,
            cursor: 0xffffff,
            selection: 0x264f78,
            ansi: CAMPBELL,
        }
    }
}

/// `#rrggbb` → 0xRRGGBB.
pub fn parse_hex(s: &str) -> Option<u32> {
    HydraRgb::parse(s).map(|c| u32::from(c.0) << 16 | u32::from(c.1) << 8 | u32::from(c.2))
}

impl Palette {
    /// The built-in scheme with `[colors]` applied. A bad value keeps the built-in colour and
    /// is reported, so one typo doesn't throw the rest away.
    pub fn from_config(c: &ColorsConfig) -> (Palette, Vec<String>) {
        let mut p = Palette::default();
        let mut problems = Vec::new();
        for (name, value, slot) in [
            ("foreground", &c.foreground, &mut p.foreground),
            ("background", &c.background, &mut p.background),
            ("cursor", &c.cursor, &mut p.cursor),
            ("selection", &c.selection, &mut p.selection),
        ] {
            if let Some(v) = value {
                match parse_hex(v) {
                    Some(x) => *slot = x,
                    None => problems.push(format!("[colors] {name} = {v:?}: use #rrggbb")),
                }
            }
        }
        if let Some(list) = &c.palette {
            if list.len() != 16 {
                problems.push(format!(
                    "[colors] palette has {} colours; it needs 16",
                    list.len()
                ));
            } else {
                for (i, v) in list.iter().enumerate() {
                    match parse_hex(v) {
                        Some(x) => p.ansi[i] = x,
                        None => {
                            problems.push(format!("[colors] palette[{i}] = {v:?}: use #rrggbb"))
                        }
                    }
                }
            }
        }
        (p, problems)
    }

    /// One of the 256 indexed colours: the palette, the 6×6×6 cube, then the grey ramp.
    pub fn indexed(&self, i: u8) -> u32 {
        let i = usize::from(i);
        match i {
            0..=15 => self.ansi[i],
            16..=231 => {
                let n = i - 16;
                let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u32 };
                level(n / 36) << 16 | level(n / 6 % 6) << 8 | level(n % 6)
            }
            _ => {
                let v = (8 + (i - 232) * 10) as u32;
                v << 16 | v << 8 | v
            }
        }
    }

    /// A cell's colour: the program's own overrides (OSC 4/10/11) first, then this palette.
    pub fn resolve(&self, color: Color, overrides: &Colors) -> u32 {
        match color {
            Color::Spec(c) => from_rgb(c),
            Color::Indexed(i) => overrides[usize::from(i)]
                .map(from_rgb)
                .unwrap_or_else(|| self.indexed(i)),
            Color::Named(n) => overrides[n].map(from_rgb).unwrap_or_else(|| self.named(n)),
        }
    }

    fn named(&self, n: NamedColor) -> u32 {
        match n {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => dim(self.foreground),
            NamedColor::DimBlack => dim(self.ansi[0]),
            NamedColor::DimRed => dim(self.ansi[1]),
            NamedColor::DimGreen => dim(self.ansi[2]),
            NamedColor::DimYellow => dim(self.ansi[3]),
            NamedColor::DimBlue => dim(self.ansi[4]),
            NamedColor::DimMagenta => dim(self.ansi[5]),
            NamedColor::DimCyan => dim(self.ansi[6]),
            NamedColor::DimWhite => dim(self.ansi[7]),
            n => self
                .ansi
                .get(n as usize)
                .copied()
                .unwrap_or(self.foreground),
        }
    }
}

fn from_rgb(c: Rgb) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// Two thirds of each channel, for faint (SGR 2) text.
pub fn dim(c: u32) -> u32 {
    let channel = |shift: u32| ((c >> shift & 0xff) * 2 / 3) << shift;
    channel(16) | channel(8) | channel(0)
}

/// An environment's tab tint: its env.toml `color`, else a hue picked from its name, so the
/// same environment always gets the same colour.
pub fn env_tint(name: &str, color: Option<(u8, u8, u8)>) -> u32 {
    if let Some((r, g, b)) = color {
        return u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
    }
    let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    hsl((hash % 360) as f32, 0.55, 0.55)
}

/// HSL (hue in degrees, saturation and lightness 0–1) → 0xRRGGBB.
fn hsl(h: f32, s: f32, l: f32) -> u32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..60 => (c, x, 0.0),
        60..120 => (x, c, 0.0),
        120..180 => (0.0, c, x),
        180..240 => (0.0, x, c),
        240..300 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f32| ((v + m) * 255.0).round() as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_scheme_is_campbell() {
        let p = Palette::default();
        assert_eq!(p.ansi, CAMPBELL);
        assert_eq!(p.ansi[1], 0xc50f1f);
        assert_eq!(p.background, 0x0c0c0c);
        assert_eq!(p.foreground, 0xcccccc);
    }

    #[test]
    fn config_overrides_and_bad_values_are_reported_one_by_one() {
        let c = ColorsConfig {
            foreground: Some("#ffffff".into()),
            background: Some("black".into()),
            cursor: None,
            selection: Some("#123456".into()),
            palette: Some(vec!["#000000".into(); 3]),
        };
        let (p, problems) = Palette::from_config(&c);
        assert_eq!(p.foreground, 0xffffff);
        assert_eq!(p.selection, 0x123456);
        assert_eq!(p.background, Palette::default().background);
        assert_eq!(p.ansi, CAMPBELL);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("background"), "{problems:?}");
        assert!(problems[1].contains("16"), "{problems:?}");
    }

    #[test]
    fn a_full_palette_replaces_campbell_except_bad_entries() {
        let mut list = vec!["#010203".to_string(); 16];
        list[4] = "blue".into();
        let c = ColorsConfig {
            palette: Some(list),
            ..Default::default()
        };
        let (p, problems) = Palette::from_config(&c);
        assert_eq!(p.ansi[0], 0x010203);
        assert_eq!(p.ansi[4], CAMPBELL[4]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("palette[4]"), "{problems:?}");
    }

    #[test]
    fn indexed_colours_cover_palette_cube_and_greys() {
        let p = Palette::default();
        assert_eq!(p.indexed(1), CAMPBELL[1]);
        assert_eq!(p.indexed(16), 0x000000);
        assert_eq!(p.indexed(196), 0xff0000);
        assert_eq!(p.indexed(231), 0xffffff);
        assert_eq!(p.indexed(232), 0x080808);
        assert_eq!(p.indexed(255), 0xeeeeee);
    }

    #[test]
    fn resolve_prefers_the_programs_own_colours() {
        let p = Palette::default();
        let mut o = Colors::default();
        assert_eq!(p.resolve(Color::Named(NamedColor::Red), &o), CAMPBELL[1]);
        assert_eq!(
            p.resolve(Color::Spec(Rgb { r: 1, g: 2, b: 3 }), &o),
            0x010203
        );
        assert_eq!(p.resolve(Color::Indexed(196), &o), 0xff0000);
        assert_eq!(
            p.resolve(Color::Named(NamedColor::DimRed), &o),
            dim(CAMPBELL[1])
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Background), &o),
            p.background
        );
        o[NamedColor::Red] = Some(Rgb {
            r: 0xaa,
            g: 0,
            b: 0,
        });
        assert_eq!(p.resolve(Color::Named(NamedColor::Red), &o), 0xaa0000);
    }

    #[test]
    fn dim_is_two_thirds() {
        assert_eq!(dim(0x969696), 0x646464);
    }

    #[test]
    fn env_tint_uses_the_colour_or_a_stable_hue() {
        assert_eq!(env_tint("work", Some((1, 2, 3))), 0x010203);
        assert_eq!(env_tint("work", None), env_tint("work", None));
        assert_ne!(env_tint("work", None), env_tint("personal", None));
    }
}
