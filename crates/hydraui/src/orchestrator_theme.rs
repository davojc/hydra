//! The orchestrator views' colours, from the design handoff's tokens
//! (docs/superpowers/specs/2026-10-22-hydraui-orchestrator-design-handoff.md, "Design tokens").
//! hydraui's own shell keeps its theme (orchestrator spec §1).

use gpui::{Hsla, rgb};

/// A design token as a gpui colour.
pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub const TEXT: u32 = 0xe6e9e8;
pub const TEXT_SECONDARY: u32 = 0x949c9a;
pub const TEXT_MUTED: u32 = 0x646c6a;
pub const BORDER: u32 = 0x2a3030;
pub const TEAL: u32 = 0x3fbfa6;
pub const ON_TEAL: u32 = 0x06201b;
pub const RED: u32 = 0xe5705f;
pub const RED_BORDER: u32 = 0x4a2620;
/// The escalation toast's background.
pub const TOAST: u32 = 0x171a1a;
pub const SURFACE: u32 = 0x161919;
pub const RAISED: u32 = 0x1c2020;
pub const BORDER_SUBTLE: u32 = 0x252b2a;
pub const TEAL_TINT: u32 = 0x10302b;

/// `fg` laid over `bg` at `alpha` (0 to 1), as one colour: a pipeline's own colour as a tint or
/// an edge on the dark panels, where a translucent colour would show what's behind it.
pub fn blend(fg: u32, bg: u32, alpha: f32) -> u32 {
    let ch = |c: u32, shift: u32| ((c >> shift) & 0xff) as f32;
    [16, 8, 0].iter().fold(0, |out, &shift| {
        let v = ch(bg, shift) + (ch(fg, shift) - ch(bg, shift)) * alpha.clamp(0.0, 1.0);
        out | ((v.round() as u32) << shift)
    })
}

/// A pipeline colour's tint, behind what's selected or at work.
pub fn tint_of(accent: u32) -> u32 {
    blend(accent, SURFACE, 0.16)
}

/// A pipeline colour's edge, around what's selected or at work.
pub fn edge_of(accent: u32) -> u32 {
    blend(accent, SURFACE, 0.4)
}
pub const TEAL_BORDER: u32 = 0x1f5a50;
pub const RED_TINT: u32 = 0x331916;
pub const AMBER: u32 = 0xe5b53e;
pub const AMBER_TINT: u32 = 0x2e2511;
pub const GREEN: u32 = 0x62c98d;
pub const GREEN_TINT: u32 = 0x12291c;
pub const BORDER_STRONG: u32 = 0x2f3635;
/// The stopped banner's background.
pub const BANNER: u32 = 0x1a1311;
/// A selected session row that waits for you.
pub const WAITING_TINT: u32 = 0x1c1513;
/// The right panel's background, as a terminal's.
pub const PANEL: u32 = 0x0a0b0b;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_blends_over_its_background() {
        assert_eq!(blend(0xffffff, 0x000000, 0.0), 0x000000);
        assert_eq!(blend(0xffffff, 0x000000, 1.0), 0xffffff);
        assert_eq!(blend(0xff0080, 0x000000, 0.5), 0x800040);
        assert_eq!(blend(0x3fbfa6, SURFACE, 2.0), 0x3fbfa6, "clamped");
    }
}
