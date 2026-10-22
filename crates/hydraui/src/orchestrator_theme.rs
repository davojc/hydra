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
