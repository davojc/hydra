# hydraui Plan 3: App Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `hydraui.exe`, a GPUI desktop terminal with tabs, split panes, per-pane hydra environments, a status bar, error banners and configurable keys, on top of `hydraui-term`.

**Architecture:** A new `hydraui` crate (lib + bin). Everything that can be plain data is a tested module with no window: the settings file, the terminal palette, a `Frame` snapshot of the grid, key/paste/mouse encodings, the split tree, the tab list, the key map, and a `Hydra` service that prepares environments and starts panes. Two GPUI views sit on top: `PaneView` (one terminal: starts its shell off the UI thread, paints a `Frame`, turns input into PTY bytes) and `Workspace` (tab strip, split layout with draggable dividers, status bar, commands). Views are checked by hand against a smoke checklist (spec §9).

**Tech Stack:** Rust 2024 workspace; hydra-core, hydra-platform, hydra-providers, hydraui-term; `gpui` = `gpui-pre` =0.3.9 with `gpui-pre-platform` =0.3.9, `gpui-component` =0.7.3, `gpui-kit-assets` =0.7.3 (pinned by the plan 1 spike); `futures` 0.3; serde + toml 0.8; dirs 6; tempfile for tests.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` §7.1, §7.2, §7.5, §7.6 (roadmap: `docs/superpowers/plans/2026-10-17-hydraui-roadmap.md`; spike: `docs/superpowers/notes/2026-10-17-hydraui-spike.md`; GPUI API notes: `docs/superpowers/notes/2026-10-17-gpui-api-notes.md`).

## Global Constraints

- **Prerequisites:** PowerShell 7 (`pwsh.exe`) on PATH. Git for Windows for the bash checks.
- **Windows 11 x64 only**, like hydra (spec §2).
- **Pinned UI stack** (spike note): `gpui = { package = "gpui-pre", version = "=0.3.9" }`, `gpui_platform = { package = "gpui-pre-platform", version = "=0.3.9", features = ["font-kit", "runtime_shaders"] }`, `gpui-component = "=0.7.3"`, `gpui-kit-assets = "=0.7.3"`. Start the app with `gpui_platform::application().with_assets(gpui_kit_assets::Assets)` (without the assets, built-in icons render blank), call `gpui_component::init(cx)` before opening the window, and wrap the root view in `gpui_component::Root`.
- **Dark by default:** `gpui_component` starts in light mode; call `Theme::change(ThemeMode::Dark, None, cx)` unless config says `theme = "light"`.
- **hydraui's files** live under `<HYDRA_HOME>/hydraui/` (`config.toml` here; `sets/`, `session.toml` in plan 4). hydraui never edits hydra's `config.toml`, `envs/` or `.hydra` files (spec §3.1).
- **Panes launch with guards off:** `PrepareOptions { no_guards: true, .. }` (spec §4.1).
- **GPUI facts the code relies on** (see the notes file): a key bound to an action never reaches `on_key_down`; stopping propagation in `on_key_down` also suppresses the Windows `WM_CHAR`, so plain text must be left to propagate and arrive through the `EntityInputHandler`; element mouse-move/up listeners only fire while hovered, so drags register `window.on_mouse_event` in a canvas paint closure; `Context::spawn` takes `async move |this: WeakEntity<T>, cx: &mut AsyncApp|`.
- **Not in this plan** (plan 4): sets in the app, the launcher (`Ctrl+Shift+P`), the file tree (`Ctrl+B`), sign-in and set-secret actions, packaging (`windows_subsystem = "windows"`, version info, release zip).
- **Tests** never touch the real `~/.hydra`, `~/.claude`, `~/.claude.json`, global git config or credentials: temp `HydraPaths`, and a `KeyringStore` with a `hydraui-test-<pid>` service.
- **Commits:**
  - Repo identity (davojc). No `--no-verify`. No employer names.
  - Every commit message ends with:
    ```
    Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
    ```
- **Before every commit, these must pass:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace` (the task's tests plus no new failures; `keyring_store::tests::round_trip_through_credential_manager` is a known rare flake — rerun once before treating it as a failure)

## Review Focus

1. **Typing with AltGr or a non-English layout** (`@` is AltGr+Q on a German keyboard; `é`): the character is typed, not sent as a Ctrl+Alt control sequence. Pinned in Task 4.
2. **Pasting Windows text (CRLF line endings), or text containing `ESC[201~`:** each line is entered once (no doubled newlines), and pasted text can't end bracketed-paste mode early. Pinned in Task 4.
3. **A mistake in `hydraui/config.toml`** (a bad colour, an unknown key name, a bad keystroke): hydraui still starts, the good values still apply, and the problem is named. Pinned in Tasks 1, 2 and 7.
4. **Closing the last pane of the last tab:** the window stays open and usable (Ctrl+T opens a new tab); tab and focus commands do nothing instead of panicking. Pinned in Task 6.
5. **Wide characters (CJK, emoji) and combining accents:** the text after them stays in the right columns. Pinned in Task 3.

---

### Task 1: `hydraui` crate and `config.toml`

**Files:**
- Create: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`, `crates/hydraui/src/main.rs`, `crates/hydraui/src/config.rs`
- Test: unit tests in `config.rs`

**Interfaces:**
- Produces (used by Tasks 2, 7, 8, 10):
  - `hydraui::config::{UiConfig, ColorsConfig, ThemeChoice}`; `UiConfig` fields `font: String`, `font_size: f32`, `theme: ThemeChoice`, `default_env: Option<String>`, `default_shell: Option<String>`, `editor: Option<String>`, `colors: ColorsConfig`, `keys: BTreeMap<String, String>`; `ColorsConfig` fields `foreground`, `background`, `cursor`, `selection: Option<String>`, `palette: Option<Vec<String>>`; `ThemeChoice::{Dark, Light}`
  - `config::config_file(&HydraPaths) -> PathBuf`, `config::parse(&str) -> Result<UiConfig, String>`, `config::load(&HydraPaths) -> (UiConfig, Option<String>)`

- [ ] **Step 1: Create the crate.** `crates/hydraui/Cargo.toml`:

```toml
[package]
name = "hydraui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[[bin]]
name = "hydraui"
path = "src/main.rs"

[dependencies]
hydra-core.workspace = true
hydra-platform.workspace = true
serde.workspace = true
toml.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/hydraui/src/lib.rs`:

```rust
//! hydraui: a desktop terminal for hydra environments
//! (spec: docs/superpowers/specs/2026-10-17-hydraui-design.md).
pub mod config;
```

`crates/hydraui/src/main.rs`:

```rust
//! hydraui.exe.
fn main() {}
```

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(parse("").unwrap(), UiConfig::default());
        assert_eq!(UiConfig::default().font, "Cascadia Mono");
        assert_eq!(UiConfig::default().font_size, 14.0);
    }

    #[test]
    fn reads_every_setting() {
        let cfg = parse(
            r##"
font = "JetBrains Mono"
font_size = 13
theme = "light"
default_env = "work"
default_shell = "bash"
editor = "code"

[colors]
background = "#101010"
palette = ["#000000", "#111111"]

[keys]
split_right = "ctrl-alt-d"
"##,
        )
        .unwrap();
        assert_eq!(cfg.font, "JetBrains Mono");
        assert_eq!(cfg.font_size, 13.0);
        assert_eq!(cfg.theme, ThemeChoice::Light);
        assert_eq!(cfg.default_env.as_deref(), Some("work"));
        assert_eq!(cfg.default_shell.as_deref(), Some("bash"));
        assert_eq!(cfg.editor.as_deref(), Some("code"));
        assert_eq!(cfg.colors.background.as_deref(), Some("#101010"));
        assert_eq!(cfg.colors.palette.as_ref().map(Vec::len), Some(2));
        assert_eq!(cfg.keys["split_right"], "ctrl-alt-d");
    }

    #[test]
    fn notepad_bom_and_crlf_are_fine() {
        assert_eq!(parse("\u{feff}font_size = 12\r\n").unwrap().font_size, 12.0);
    }

    #[test]
    fn mistakes_are_errors_naming_the_problem() {
        let e = parse("font_size = 14\nfontsize = 12\n").unwrap_err();
        assert!(e.contains("fontsize") && e.contains("line 2"), "{e}");
        assert!(
            parse("font_size = 200")
                .unwrap_err()
                .contains("between 6 and 72")
        );
        assert!(parse("default_shell = \"fish\"").unwrap_err().contains("fish"));
        assert!(parse("font = \" \"").unwrap_err().contains("font"));
        assert!(parse("theme = \"blue\"").is_err());
    }

    #[test]
    fn load_falls_back_to_the_defaults_and_says_why() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert_eq!(load(&paths), (UiConfig::default(), None));
        std::fs::create_dir_all(config_file(&paths).parent().unwrap()).unwrap();
        std::fs::write(config_file(&paths), "font_size = \"big\"").unwrap();
        let (cfg, problem) = load(&paths);
        assert_eq!(cfg, UiConfig::default());
        let problem = problem.unwrap();
        assert!(
            problem.contains("config.toml") && problem.contains("font_size"),
            "{problem}"
        );
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui` — expected: compile errors (`parse`, `UiConfig`, `load`, … not found).

- [ ] **Step 4: Implement.** Above the tests in `config.rs`:

```rust
//! `<HYDRA_HOME>/hydraui/config.toml`: hydraui's own settings (spec §3.1). Every key is
//! optional and unknown keys are errors; a bad file never stops hydraui (the defaults are
//! used and the problem is shown).
use std::collections::BTreeMap;
use std::path::PathBuf;

use hydra_core::paths::HydraPaths;
use hydra_platform::shell::ShellKind;
use serde::Deserialize;

/// The look of hydraui's own controls (tabs, dialogs). The terminal's colours are `[colors]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
}

/// `[colors]`: `#rrggbb` values over the built-in scheme.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorsConfig {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub selection: Option<String>,
    /// The 16 ANSI colours, black to bright white.
    pub palette: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiConfig {
    pub font: String,
    pub font_size: f32,
    pub theme: ThemeChoice,
    /// The environment for a new tab in an unbound folder (spec §7.6).
    pub default_env: Option<String>,
    /// `pwsh` or `bash`; else hydra's `default_shell`, else pwsh.
    pub default_shell: Option<String>,
    /// Opens files from the file tree (plan 4); read now so the file format is stable.
    pub editor: Option<String>,
    pub colors: ColorsConfig,
    /// Command name → keys, e.g. `split_right = "ctrl-alt-d"`; `""` unbinds.
    pub keys: BTreeMap<String, String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            font: "Cascadia Mono".into(),
            font_size: 14.0,
            theme: ThemeChoice::Dark,
            default_env: None,
            default_shell: None,
            editor: None,
            colors: ColorsConfig::default(),
            keys: BTreeMap::new(),
        }
    }
}

pub fn config_file(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("config.toml")
}

/// Parses config text (dropping a UTF-8 BOM) and checks what serde can't.
pub fn parse(text: &str) -> Result<UiConfig, String> {
    let cfg: UiConfig = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|e| e.to_string())?;
    if cfg.font.trim().is_empty() {
        return Err("font can't be empty".into());
    }
    if !(6.0..=72.0).contains(&cfg.font_size) {
        return Err(format!(
            "font_size {} must be between 6 and 72",
            cfg.font_size
        ));
    }
    if let Some(s) = &cfg.default_shell
        && ShellKind::parse(s).is_none()
    {
        return Err(format!("default_shell {s:?}: use pwsh or bash"));
    }
    Ok(cfg)
}

/// The settings, or the defaults plus what was wrong with the file.
pub fn load(paths: &HydraPaths) -> (UiConfig, Option<String>) {
    let path = config_file(paths);
    match std::fs::read_to_string(&path) {
        Ok(text) => match parse(&text) {
            Ok(cfg) => (cfg, None),
            Err(e) => (UiConfig::default(), Some(format!("{}: {e}", path.display()))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (UiConfig::default(), None),
        Err(e) => (
            UiConfig::default(),
            Some(format!("can't read {}: {e}", path.display())),
        ),
    }
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui` — expected: PASS (5 tests). (If `toml` words the line differently, match what it prints for line 2 — don't drop the assertion.)

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui Cargo.lock
git commit -m "feat: hydraui crate with its config file

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Terminal palette and environment tints

**Files:**
- Modify: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/palette.rs`
- Test: unit tests in `palette.rs`

**Interfaces:**
- Consumes: `config::ColorsConfig` (Task 1); `hydra_core::config::Rgb::parse(&str) -> Option<Rgb>` (`Rgb(pub u8, pub u8, pub u8)`); `hydraui_term::alacritty_terminal` (re-export).
- Produces (used by Tasks 3, 8, 9, 10):
  - `palette::Palette { foreground: u32, background: u32, cursor: u32, selection: u32, ansi: [u32; 16] }` (`Default` = Campbell), `Palette::from_config(&ColorsConfig) -> (Palette, Vec<String>)`, `Palette::indexed(u8) -> u32`, `Palette::resolve(Color, &Colors) -> u32`
  - `palette::CAMPBELL: [u32; 16]`, `palette::parse_hex(&str) -> Option<u32>`, `palette::dim(u32) -> u32`, `palette::env_tint(&str, Option<(u8, u8, u8)>) -> u32`
  - Colours are `0xRRGGBB` `u32`s throughout.

- [ ] **Step 1: Add the dependency and module.** In `crates/hydraui/Cargo.toml` `[dependencies]`, add after `hydra-platform.workspace = true`:

```toml
hydraui-term.workspace = true
```

In the root `Cargo.toml` `[workspace.dependencies]`, add after `hydra-providers = { path = "crates/hydra-providers" }`:

```toml
hydraui-term = { path = "crates/hydraui-term" }
```

In `crates/hydraui/src/lib.rs`, add `pub mod palette;` after `pub mod config;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/palette.rs`:

```rust
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
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui palette` — expected: compile errors (`Palette`, `CAMPBELL`, `dim`, `env_tint` not found).

- [ ] **Step 4: Implement.** Above the tests in `palette.rs`:

```rust
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
                        None => problems.push(format!("[colors] palette[{i}] = {v:?}: use #rrggbb")),
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
            Color::Named(n) => overrides[n]
                .map(from_rgb)
                .unwrap_or_else(|| self.named(n)),
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
            n => self.ansi.get(n as usize).copied().unwrap_or(self.foreground),
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
    let hash = name
        .bytes()
        .fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
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
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui palette` — expected: PASS (7 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui
git commit -m "feat: hydraui terminal palette and environment tints

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `Frame`: what the grid shows

**Files:**
- Modify: `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/frame.rs`
- Test: unit tests in `frame.rs`

**Interfaces:**
- Consumes: `palette::{Palette, dim}` (Task 2); `hydraui_term::GridSize` (implements `Dimensions`) and `hydraui_term::alacritty_terminal`.
- Produces (used by Task 9):
  - `frame::Style { fg: u32, bold, italic, underline, strikeout: bool }`, `frame::Run { len: usize /* UTF-8 bytes */, style: Style }`, `frame::Glyph { col: usize, text: String, style: Style, wide: bool }`, `frame::Span { row, start, end: usize }`, `frame::Row { text: String, runs: Vec<Run>, glyphs: Vec<Glyph>, backgrounds: Vec<(Span, u32)> }`, `frame::CursorKind::{Block, HollowBlock, Underline, Beam}`, `frame::Cursor { row, col: usize, kind: CursorKind, wide: bool }`, `frame::Frame { rows: Vec<Row>, cursor: Option<Cursor>, selection: Vec<Span> }`
  - `frame::build_frame<T>(&Term<T>, &Palette, focused: bool) -> Frame`
  - `frame::link_at<T>(&Term<T>, row: usize, col: usize) -> Option<String>`
  - `frame::literal(&str) -> String`, `frame::find_next<T>(&Term<T>, query: &str, current: Option<&Match>, direction: Direction) -> Option<Match>`

- [ ] **Step 1: Add the module.** In `crates/hydraui/src/lib.rs`, add `pub mod frame;` after `pub mod config;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/frame.rs`:

```rust
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
        let f = frame(&term(8, 1, b"\x1b[31;1mR\x1b[0;3;4mI\x1b[0;9mS\x1b[0;44m \x1b[0m"));
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
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui frame` — expected: compile errors (`build_frame`, `Frame`, `link_at`, `find_next`, … not found).

- [ ] **Step 4: Implement.** Above the tests in `frame.rs`:

```rust
//! A snapshot of what the terminal shows, built while holding the grid's lock and painted
//! after it is released (spec §7.2). Plain data, so it is tested without a window.
use hydraui_term::alacritty_terminal::Term;
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
pub fn build_frame<T>(term: &Term<T>, palette: &Palette, focused: bool) -> Frame {
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
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui frame` — expected: PASS (11 tests). If `find_walks_up_and_wraps_round` fails only on which match comes first, check alacritty's `search_next` side argument (`term/search.rs`) and fix `find_next`, not the test.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui frame snapshot of the terminal grid

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Key, paste and mouse encodings

**Files:**
- Modify: `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/input.rs`
- Test: unit tests in `input.rs`

**Interfaces:**
- Consumes: `hydraui_term::alacritty_terminal::term::TermMode`.
- Produces (used by Task 9):
  - `input::Mods { ctrl, alt, shift: bool }` (`Copy, Default`)
  - `input::key_bytes(key: &str, key_char: Option<&str>, m: Mods, mode: TermMode) -> Option<Vec<u8>>` — `None` means plain text, left to the input handler
  - `input::paste_bytes(text: &str, bracketed: bool) -> Vec<u8>`
  - `input::ReportButton::{Left, Middle, Right, WheelUp, WheelDown}`, `input::MouseKind::{Press, Release, Move}`, `input::mouse_report(button: Option<ReportButton>, kind: MouseKind, col: usize, row: usize, m: Mods, mode: TermMode) -> Option<Vec<u8>>`
  - `input::Wheel::{Send(Vec<u8>), Scroll(i32)}`, `input::wheel(lines: i32 /* > 0 is up */, col, row: usize, m: Mods, mode: TermMode) -> Wheel`

- [ ] **Step 1: Add the module.** In `crates/hydraui/src/lib.rs`, add `pub mod input;` after `pub mod frame;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
    };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };
    const SHIFT: Mods = Mods {
        shift: true,
        ..NONE
    };

    fn key(k: &str, ch: Option<&str>, m: Mods) -> Option<Vec<u8>> {
        key_bytes(k, ch, m, TermMode::empty())
    }

    fn b(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    #[test]
    fn plain_text_is_left_to_the_input_handler() {
        assert_eq!(key("a", Some("a"), NONE), None);
        assert_eq!(key("a", Some("A"), SHIFT), None);
        assert_eq!(key("space", Some(" "), NONE), None);
    }

    #[test]
    fn altgr_is_text_not_a_control_sequence() {
        let altgr = Mods {
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert_eq!(key("q", Some("@"), altgr), None);
    }

    #[test]
    fn special_keys() {
        assert_eq!(key("enter", None, NONE), b("\r"));
        assert_eq!(key("enter", None, ALT), b("\x1b\r"));
        assert_eq!(key("escape", None, NONE), b("\x1b"));
        assert_eq!(key("backspace", None, NONE), b("\x7f"));
        assert_eq!(key("backspace", None, CTRL), b("\x08"));
        assert_eq!(key("backspace", None, ALT), b("\x1b\x7f"));
        assert_eq!(key("tab", None, NONE), b("\t"));
        assert_eq!(key("tab", None, SHIFT), b("\x1b[Z"));
        assert_eq!(key("delete", None, NONE), b("\x1b[3~"));
        assert_eq!(key("pageup", None, NONE), b("\x1b[5~"));
    }

    #[test]
    fn arrows_follow_cursor_key_mode_and_modifiers() {
        assert_eq!(key("up", None, NONE), b("\x1b[A"));
        assert_eq!(
            key_bytes("up", None, NONE, TermMode::APP_CURSOR),
            b("\x1bOA")
        );
        assert_eq!(key("right", None, CTRL), b("\x1b[1;5C"));
        assert_eq!(key("home", None, SHIFT), b("\x1b[1;2H"));
        assert_eq!(
            key(
                "delete",
                None,
                Mods {
                    ctrl: true,
                    shift: true,
                    alt: false
                }
            ),
            b("\x1b[3;6~")
        );
    }

    #[test]
    fn function_keys() {
        assert_eq!(key("f1", None, NONE), b("\x1bOP"));
        assert_eq!(key("f5", None, NONE), b("\x1b[15~"));
        assert_eq!(key("f12", None, NONE), b("\x1b[24~"));
        assert_eq!(key("f1", None, CTRL), b("\x1b[1;5P"));
        assert_eq!(key("f5", None, SHIFT), b("\x1b[15;2~"));
        assert_eq!(key("f13", None, NONE), None);
        assert_eq!(key("f", Some("f"), NONE), None);
    }

    #[test]
    fn control_and_alt_combinations() {
        assert_eq!(key("c", None, CTRL), Some(vec![3]));
        assert_eq!(key("a", None, CTRL), Some(vec![1]));
        assert_eq!(key("z", None, CTRL), Some(vec![26]));
        assert_eq!(key("[", None, CTRL), Some(vec![0x1b]));
        assert_eq!(key("space", None, CTRL), Some(vec![0]));
        assert_eq!(key("x", Some("x"), ALT), b("\x1bx"));
        assert_eq!(
            key(
                "x",
                Some("X"),
                Mods {
                    alt: true,
                    shift: true,
                    ctrl: false
                }
            ),
            b("\x1bX")
        );
    }

    #[test]
    fn paste_turns_every_line_ending_into_one_return() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn bracketed_paste_wraps_and_cannot_be_escaped() {
        assert_eq!(
            paste_bytes("x\x1b[201~y\r\n", true),
            b"\x1b[200~x[201~y\r\x1b[201~".to_vec()
        );
    }

    #[test]
    fn mouse_reports_only_when_the_program_asks() {
        assert_eq!(
            mouse_report(
                Some(ReportButton::Left),
                MouseKind::Press,
                0,
                0,
                NONE,
                TermMode::empty()
            ),
            None
        );
        let click = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse_report(None, MouseKind::Move, 0, 0, NONE, click), None);
    }

    #[test]
    fn sgr_mouse_reports() {
        let m = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let r = |b, k, col, row, mods| mouse_report(b, k, col, row, mods, m);
        assert_eq!(
            r(Some(ReportButton::Left), MouseKind::Press, 4, 2, NONE),
            b("\x1b[<0;5;3M")
        );
        assert_eq!(
            r(Some(ReportButton::Left), MouseKind::Release, 4, 2, NONE),
            b("\x1b[<0;5;3m")
        );
        assert_eq!(
            r(Some(ReportButton::Right), MouseKind::Press, 0, 0, CTRL),
            b("\x1b[<18;1;1M")
        );
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Move, 0, 0, NONE, drag),
            b("\x1b[<32;1;1M")
        );
    }

    #[test]
    fn legacy_mouse_reports() {
        let m = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Press, 0, 0, NONE, m),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Release, 0, 0, NONE, m),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Press, 300, 0, NONE, m),
            None
        );
    }

    #[test]
    fn the_wheel_scrolls_reports_or_sends_arrows() {
        assert_eq!(wheel(2, 0, 0, NONE, TermMode::empty()), Wheel::Scroll(2));
        let alt = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            wheel(-1, 0, 0, NONE, alt),
            Wheel::Send(b"\x1b[B".to_vec())
        );
        assert_eq!(
            wheel(1, 0, 0, NONE, alt | TermMode::APP_CURSOR),
            Wheel::Send(b"\x1bOA".to_vec())
        );
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            wheel(-2, 0, 0, NONE, sgr),
            Wheel::Send(b"\x1b[<65;1;1M\x1b[<65;1;1M".to_vec())
        );
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui input` — expected: compile errors (`key_bytes`, `Mods`, `paste_bytes`, … not found).

- [ ] **Step 4: Implement.** Above the tests in `input.rs`:

```rust
//! What keys, pastes and mouse events send to the shell (xterm encodings).
use hydraui_term::alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// The bytes for a key press, or `None` for plain text: that arrives through the window's
/// input handler instead, so IME and AltGr characters work. `key` is GPUI's key name.
pub fn key_bytes(key: &str, key_char: Option<&str>, m: Mods, mode: TermMode) -> Option<Vec<u8>> {
    let param = m.param();
    let csi = |s: String| format!("\x1b[{s}").into_bytes();
    let out = match key {
        "enter" if m.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.ctrl => b"\x08".to_vec(),
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "tab" if m.shift => csi("Z".into()),
        "tab" => b"\t".to_vec(),
        "up" | "down" | "right" | "left" | "home" | "end" => {
            let c = match key {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                "left" => 'D',
                "home" => 'H',
                _ => 'F',
            };
            if param > 1 {
                csi(format!("1;{param}{c}"))
            } else if mode.contains(TermMode::APP_CURSOR) {
                format!("\x1bO{c}").into_bytes()
            } else {
                csi(c.to_string())
            }
        }
        "insert" | "delete" | "pageup" | "pagedown" => {
            let n = match key {
                "insert" => 2,
                "delete" => 3,
                "pageup" => 5,
                _ => 6,
            };
            if param > 1 {
                csi(format!("{n};{param}~"))
            } else {
                csi(format!("{n}~"))
            }
        }
        "space" if m.ctrl => vec![0],
        _ => {
            if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                return function_key(n, param);
            }
            if m.ctrl && !m.alt {
                return control(key);
            }
            if m.alt && !m.ctrl {
                let mut out = vec![0x1b];
                out.extend_from_slice(key_char.unwrap_or(key).as_bytes());
                return Some(out);
            }
            // Plain text, or Ctrl+Alt (AltGr on many layouts): the input handler's.
            return None;
        }
    };
    Some(out)
}

fn function_key(n: u8, param: u8) -> Option<Vec<u8>> {
    let s = match n {
        1..=4 => {
            let c = b"PQRS"[usize::from(n - 1)] as char;
            if param > 1 {
                format!("\x1b[1;{param}{c}")
            } else {
                format!("\x1bO{c}")
            }
        }
        5..=12 => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            if param > 1 {
                format!("\x1b[{code};{param}~")
            } else {
                format!("\x1b[{code}~")
            }
        }
        _ => return None,
    };
    Some(s.into_bytes())
}

/// Ctrl+key: the C0 control code.
fn control(key: &str) -> Option<Vec<u8>> {
    let &[b] = key.as_bytes() else {
        return None;
    };
    let code = match b {
        b'a'..=b'z' => b - b'a' + 1,
        b'@' | b'2' => 0,
        b'[' | b'3' => 0x1b,
        b'\\' | b'4' => 0x1c,
        b']' | b'5' => 0x1d,
        b'^' | b'6' => 0x1e,
        b'_' | b'-' | b'7' => 0x1f,
        b'8' => 0x7f,
        _ => return None,
    };
    Some(vec![code])
}

/// Pasted text for the shell: every line ending becomes one Enter (`\r`). With bracketed
/// paste on, the text is wrapped and ESC is removed so it can't end the bracket early.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let clean: String = text.chars().filter(|c| *c != '\x1b').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Move,
}

/// The report for a mouse event at a 0-based cell when the program asked for mouse
/// reporting; `None` means the event is hydraui's own (selection, scrollback).
pub fn mouse_report(
    button: Option<ReportButton>,
    kind: MouseKind,
    col: usize,
    row: usize,
    m: Mods,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let wanted = match kind {
        MouseKind::Press | MouseKind::Release => mode.intersects(TermMode::MOUSE_MODE),
        MouseKind::Move if button.is_some() => {
            mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        }
        MouseKind::Move => mode.contains(TermMode::MOUSE_MOTION),
    };
    if !wanted {
        return None;
    }
    let mut code: u32 = match button {
        Some(ReportButton::Left) => 0,
        Some(ReportButton::Middle) => 1,
        Some(ReportButton::Right) => 2,
        Some(ReportButton::WheelUp) => 64,
        Some(ReportButton::WheelDown) => 65,
        None => 3,
    };
    if kind == MouseKind::Move {
        code += 32;
    }
    let mods = 4 * u32::from(m.shift) + 8 * u32::from(m.alt) + 16 * u32::from(m.ctrl);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if kind == MouseKind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{};{}{end}", code + mods, col + 1, row + 1).into_bytes());
    }
    // X10 encoding: a release doesn't say which button, and cells stop at 223.
    if kind == MouseKind::Release {
        code = 3;
    }
    let (x, y) = (col + 33, row + 33);
    if x > 255 || y > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', (32 + code + mods) as u8, x as u8, y as u8])
}

/// What a wheel turn does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wheel {
    /// Bytes for the program: mouse reports, or arrow keys on the alternate screen.
    Send(Vec<u8>),
    /// Scroll hydraui's view of the history by this many lines (> 0 is up).
    Scroll(i32),
}

/// A wheel turn of `lines` (> 0 is up) over a 0-based cell.
pub fn wheel(lines: i32, col: usize, row: usize, m: Mods, mode: TermMode) -> Wheel {
    let n = lines.unsigned_abs() as usize;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button = if lines > 0 {
            ReportButton::WheelUp
        } else {
            ReportButton::WheelDown
        };
        if let Some(one) = mouse_report(Some(button), MouseKind::Press, col, row, m, mode) {
            return Wheel::Send(one.repeat(n));
        }
    }
    if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        let c = if lines > 0 { 'A' } else { 'B' };
        let one = if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        };
        return Wheel::Send(one.repeat(n).into_bytes());
    }
    Wheel::Scroll(lines)
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui input` — expected: PASS (12 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui key, paste and mouse encodings

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Split tree

**Files:**
- Modify: `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/split.rs`
- Test: unit tests in `split.rs`

**Interfaces:**
- Produces (used by Tasks 6, 9, 10):
  - `split::PaneId = u64`; `split::Axis::{Row /* side by side */, Column /* stacked */}`; `split::Dir::{Left, Right, Up, Down}`; `split::Rect { x, y, w, h: f32 }`; `split::MIN_RATIO = 0.1`, `split::MAX_RATIO = 0.9`
  - `split::Tree::{Leaf(PaneId), Split { axis: Axis, ratio: f32, first: Box<Tree>, second: Box<Tree> }}` (`Debug, Clone, PartialEq`)
  - `Tree::panes(&self) -> Vec<PaneId>`, `Tree::contains(&self, PaneId) -> bool`, `Tree::split(&mut self, target, axis, new) -> bool`, `Tree::remove(&mut self, target) -> Option<PaneId>` (the pane to focus next; `None` when not found or it is the whole tree), `Tree::layout(&self, Rect) -> Vec<(PaneId, Rect)>`, `Tree::neighbor(&self, from, Dir) -> Option<PaneId>`, `Tree::set_ratio(&mut self, path: &[usize], ratio: f32) -> bool` (path: 0 = first child, 1 = second, from the root)

- [ ] **Step 1: Add the module.** In `crates/hydraui/src/lib.rs`, add `pub mod split;` after `pub mod palette;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/split.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 1 | (2 / 3)
    fn three() -> Tree {
        let mut t = Tree::Leaf(1);
        assert!(t.split(1, Axis::Row, 2));
        assert!(t.split(2, Axis::Column, 3));
        t
    }

    fn leaf(id: PaneId) -> Box<Tree> {
        Box::new(Tree::Leaf(id))
    }

    #[test]
    fn split_builds_the_tree() {
        assert_eq!(
            three(),
            Tree::Split {
                axis: Axis::Row,
                ratio: 0.5,
                first: leaf(1),
                second: Box::new(Tree::Split {
                    axis: Axis::Column,
                    ratio: 0.5,
                    first: leaf(2),
                    second: leaf(3),
                }),
            }
        );
        assert_eq!(three().panes(), vec![1, 2, 3]);
        assert!(three().contains(3) && !three().contains(9));
        assert!(!Tree::Leaf(1).split(9, Axis::Row, 2));
    }

    #[test]
    fn layout_divides_by_ratio() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        assert_eq!(
            three().layout(r(0.0, 0.0, 100.0, 50.0)),
            vec![
                (1, r(0.0, 0.0, 50.0, 50.0)),
                (2, r(50.0, 0.0, 50.0, 25.0)),
                (3, r(50.0, 25.0, 50.0, 25.0)),
            ]
        );
    }

    #[test]
    fn neighbours_by_geometry() {
        let t = three();
        assert_eq!(t.neighbor(1, Dir::Right), Some(2));
        assert_eq!(t.neighbor(3, Dir::Left), Some(1));
        assert_eq!(t.neighbor(3, Dir::Up), Some(2));
        assert_eq!(t.neighbor(2, Dir::Down), Some(3));
        assert_eq!(t.neighbor(1, Dir::Left), None);
        assert_eq!(t.neighbor(2, Dir::Right), None);
        assert_eq!(t.neighbor(9, Dir::Right), None);
    }

    #[test]
    fn removing_a_pane_promotes_its_sibling() {
        let mut t = three();
        assert_eq!(t.remove(2), Some(3));
        assert_eq!(
            t,
            Tree::Split {
                axis: Axis::Row,
                ratio: 0.5,
                first: leaf(1),
                second: leaf(3)
            }
        );
        assert_eq!(t.remove(1), Some(3));
        assert_eq!(t, Tree::Leaf(3));
        assert_eq!(t.remove(3), None);
        assert_eq!(t.remove(9), None);
    }

    #[test]
    fn ratios_are_set_by_path_and_clamped() {
        let mut t = three();
        assert!(t.set_ratio(&[], 0.7));
        assert!(t.set_ratio(&[1], 0.02));
        assert!(!t.set_ratio(&[0], 0.5));
        assert!(!t.set_ratio(&[], f32::NAN));
        let Tree::Split { ratio, second, .. } = &t else {
            panic!()
        };
        assert_eq!(*ratio, 0.7);
        let Tree::Split { ratio, .. } = second.as_ref() else {
            panic!()
        };
        assert_eq!(*ratio, MIN_RATIO);
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui split` — expected: compile errors (`Tree`, `Axis`, … not found).

- [ ] **Step 4: Implement.** Above the tests in `split.rs`:

```rust
//! A tab's panes as a binary split tree (spec §7.1), with the geometry for moving focus.
pub type PaneId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side (split right); `ratio` is the left pane's share.
    Row,
    /// Stacked (split down); `ratio` is the top pane's share.
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tree {
    Leaf(PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Tree>,
        second: Box<Tree>,
    },
}

/// A divider can't be dragged closer than this to either edge.
pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;

impl Tree {
    /// Panes left to right, top to bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Tree::Leaf(id) => out.push(*id),
            Tree::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Tree::Leaf(x) => *x == id,
            Tree::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Splits `target` in half: it keeps the first half and `new` takes the second.
    pub fn split(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        if *self == Tree::Leaf(target) {
            *self = Tree::Split {
                axis,
                ratio: 0.5,
                first: Box::new(Tree::Leaf(target)),
                second: Box::new(Tree::Leaf(new)),
            };
            return true;
        }
        match self {
            Tree::Leaf(_) => false,
            Tree::Split { first, second, .. } => {
                first.split(target, axis, new) || second.split(target, axis, new)
            }
        }
    }

    /// Removes a pane; its sibling takes the parent split's place. Returns the pane to focus
    /// next (the sibling's pane nearest the removed one), or `None` when `target` isn't here
    /// or is the whole tree.
    pub fn remove(&mut self, target: PaneId) -> Option<PaneId> {
        let Tree::Split { first, second, .. } = self else {
            return None;
        };
        let (keep, focus_first) = if **first == Tree::Leaf(target) {
            (std::mem::replace(&mut **second, Tree::Leaf(target)), true)
        } else if **second == Tree::Leaf(target) {
            (std::mem::replace(&mut **first, Tree::Leaf(target)), false)
        } else {
            return first.remove(target).or_else(|| second.remove(target));
        };
        *self = keep;
        let panes = self.panes();
        Some(if focus_first {
            panes[0]
        } else {
            panes[panes.len() - 1]
        })
    }

    /// Each pane's rectangle within `area`.
    pub fn layout(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.layout_into(area, &mut out);
        out
    }

    fn layout_into(&self, a: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Tree::Leaf(id) => out.push((*id, a)),
            Tree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (one, two) = match axis {
                    Axis::Row => (
                        Rect { w: a.w * ratio, ..a },
                        Rect {
                            x: a.x + a.w * ratio,
                            w: a.w * (1.0 - ratio),
                            ..a
                        },
                    ),
                    Axis::Column => (
                        Rect { h: a.h * ratio, ..a },
                        Rect {
                            y: a.y + a.h * ratio,
                            h: a.h * (1.0 - ratio),
                            ..a
                        },
                    ),
                };
                first.layout_into(one, out);
                second.layout_into(two, out);
            }
        }
    }

    /// The pane next to `from` in direction `dir`: the nearest one that shares an edge
    /// stretch; on a tie, the one sharing more of it, then the first.
    pub fn neighbor(&self, from: PaneId, dir: Dir) -> Option<PaneId> {
        const EPS: f32 = 1e-4;
        let rects = self.layout(Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        });
        let (_, f) = *rects.iter().find(|(id, _)| *id == from)?;
        let overlap = |a: f32, al: f32, b: f32, bl: f32| (a + al).min(b + bl) - a.max(b);
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, r)| {
                let (gap, shared) = match dir {
                    Dir::Right => (r.x - (f.x + f.w), overlap(f.y, f.h, r.y, r.h)),
                    Dir::Left => (f.x - (r.x + r.w), overlap(f.y, f.h, r.y, r.h)),
                    Dir::Down => (r.y - (f.y + f.h), overlap(f.x, f.w, r.x, r.w)),
                    Dir::Up => (f.y - (r.y + r.h), overlap(f.x, f.w, r.x, r.w)),
                };
                (gap > -EPS && shared > EPS).then_some((gap, shared, *id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0).then(b.1.total_cmp(&a.1)))
            .map(|(_, _, id)| id)
    }

    /// Sets the ratio of the split at `path` (0 = first child, 1 = second, from the root),
    /// clamped to `MIN_RATIO..=MAX_RATIO`.
    pub fn set_ratio(&mut self, path: &[usize], ratio: f32) -> bool {
        if !ratio.is_finite() {
            return false;
        }
        match (self, path.split_first()) {
            (Tree::Split { ratio: r, .. }, None) => {
                *r = ratio.clamp(MIN_RATIO, MAX_RATIO);
                true
            }
            (Tree::Split { first, .. }, Some((0, rest))) => first.set_ratio(rest, ratio),
            (Tree::Split { second, .. }, Some((1, rest))) => second.set_ratio(rest, ratio),
            _ => false,
        }
    }
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui split` — expected: PASS (5 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui split tree

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Tabs

**Files:**
- Modify: `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/tabs.rs`
- Test: unit tests in `tabs.rs`

**Interfaces:**
- Consumes: `split::{Axis, Dir, PaneId, Tree}` (Task 5).
- Produces (used by Task 10; plan 4 adds sets through `set`):
  - `tabs::TabId = u64`; `tabs::Tab { id: TabId, set: Option<String>, tree: Tree, focused: PaneId }`; `tabs::Closed::{NotFound, Pane, Tab(TabId)}`
  - `tabs::Tabs` (`Default`): `is_empty`, `iter`, `get(TabId) -> Option<&Tab>`, `active() -> Option<&Tab>`, `active_mut() -> Option<&mut Tab>`, `active_id() -> Option<TabId>`, `add(set: Option<String>, pane: PaneId) -> TabId`, `activate(TabId) -> bool`, `next()`, `previous()`, `split_focused(Axis, new: PaneId) -> Option<PaneId>`, `focus_dir(Dir) -> Option<PaneId>`, `focus_pane(PaneId) -> bool`, `tab_of(PaneId) -> Option<TabId>`, `close_pane(PaneId) -> Closed`, `close_tab(TabId) -> Vec<PaneId>`, `groups() -> Vec<(Option<String>, Vec<TabId>)>`

- [ ] **Step 1: Add the module.** In `crates/hydraui/src/lib.rs`, add `pub mod tabs;` after `pub mod split;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/tabs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ids(t: &Tabs) -> Vec<TabId> {
        t.iter().map(|tab| tab.id).collect()
    }

    #[test]
    fn tabs_open_grouped_by_set_with_loose_last_and_become_active() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        let b = t.add(Some("client-a".into()), 2);
        let c = t.add(Some("client-a".into()), 3);
        let d = t.add(Some("personal".into()), 4);
        assert_eq!(ids(&t), vec![b, c, d, a]);
        assert_eq!(t.active_id(), Some(d));
        assert_eq!(
            t.groups(),
            vec![
                (Some("client-a".to_string()), vec![b, c]),
                (Some("personal".to_string()), vec![d]),
                (None, vec![a]),
            ]
        );
        assert_eq!(t.tab_of(3), Some(c));
    }

    #[test]
    fn next_and_previous_wrap_round() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.add(None, 2);
        let c = t.add(None, 3);
        t.next();
        assert_eq!(t.active_id(), Some(a));
        t.previous();
        assert_eq!(t.active_id(), Some(c));
        assert!(t.activate(a));
        assert_eq!(t.active_id(), Some(a));
        assert!(!t.activate(99));
    }

    #[test]
    fn split_and_move_focus() {
        let mut t = Tabs::default();
        t.add(None, 1);
        assert_eq!(t.split_focused(Axis::Row, 2), Some(1));
        assert_eq!(t.active().unwrap().focused, 2);
        assert_eq!(t.focus_dir(Dir::Left), Some(1));
        assert_eq!(t.focus_dir(Dir::Left), None);
        assert_eq!(t.active().unwrap().focused, 1);
    }

    #[test]
    fn focus_pane_activates_its_tab() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.split_focused(Axis::Row, 2);
        t.add(None, 3);
        assert!(t.focus_pane(1));
        assert_eq!(t.active_id(), Some(a));
        assert_eq!(t.active().unwrap().focused, 1);
        assert!(!t.focus_pane(9));
    }

    #[test]
    fn closing_a_pane_then_the_last_pane_of_a_tab() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.split_focused(Axis::Row, 2);
        let b = t.add(None, 3);
        assert_eq!(t.close_pane(2), Closed::Pane);
        assert_eq!(t.get(a).unwrap().focused, 1);
        assert_eq!(t.active_id(), Some(b));
        assert_eq!(t.close_pane(3), Closed::Tab(b));
        assert_eq!(t.active_id(), Some(a));
        assert_eq!(t.close_pane(9), Closed::NotFound);
    }

    #[test]
    fn closing_everything_leaves_a_usable_empty_list() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        t.split_focused(Axis::Column, 2);
        assert_eq!(t.close_tab(a), vec![1, 2]);
        assert!(t.is_empty());
        assert_eq!(t.active(), None);
        t.next();
        t.previous();
        assert_eq!(t.focus_dir(Dir::Up), None);
        assert_eq!(t.split_focused(Axis::Row, 5), None);
        assert_eq!(t.close_tab(a), Vec::<PaneId>::new());
        assert!(t.groups().is_empty());
        let b = t.add(None, 6);
        assert_eq!(t.active_id(), Some(b));
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui tabs` — expected: compile errors (`Tabs`, `Closed`, … not found).

- [ ] **Step 4: Implement.** Above the tests in `tabs.rs`:

```rust
//! The tab list (spec §7.1): tabs in strip order, grouped by set with Loose tabs last, each
//! holding a split tree and its focused pane.
use crate::split::{Axis, Dir, PaneId, Tree};

pub type TabId = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub id: TabId,
    /// The set that opened it; `None` is the Loose group.
    pub set: Option<String>,
    pub tree: Tree,
    pub focused: PaneId,
}

/// What closing a pane closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    NotFound,
    Pane,
    /// It was the tab's last pane.
    Tab(TabId),
}

#[derive(Debug, Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
    next_id: TabId,
}

impl Tabs {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active().map(|t| t.id)
    }

    /// Adds a one-pane tab after the last tab of its set (a new set goes before the Loose
    /// tabs; a Loose tab goes last) and makes it active.
    pub fn add(&mut self, set: Option<String>, pane: PaneId) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        let at = match &set {
            Some(name) => self
                .tabs
                .iter()
                .rposition(|t| t.set.as_ref() == Some(name))
                .map(|i| i + 1)
                .unwrap_or_else(|| {
                    self.tabs
                        .iter()
                        .position(|t| t.set.is_none())
                        .unwrap_or(self.tabs.len())
                }),
            None => self.tabs.len(),
        };
        self.tabs.insert(
            at,
            Tab {
                id,
                set,
                tree: Tree::Leaf(pane),
                focused: pane,
            },
        );
        self.active = at;
        id
    }

    pub fn activate(&mut self, id: TabId) -> bool {
        match self.tabs.iter().position(|t| t.id == id) {
            Some(i) => {
                self.active = i;
                true
            }
            None => false,
        }
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    /// Splits the active tab's focused pane and focuses `new`. Returns the pane that was split.
    pub fn split_focused(&mut self, axis: Axis, new: PaneId) -> Option<PaneId> {
        let tab = self.active_mut()?;
        let old = tab.focused;
        if !tab.tree.split(old, axis, new) {
            return None;
        }
        tab.focused = new;
        Some(old)
    }

    /// Moves focus within the active tab; returns the newly focused pane.
    pub fn focus_dir(&mut self, dir: Dir) -> Option<PaneId> {
        let tab = self.active_mut()?;
        let next = tab.tree.neighbor(tab.focused, dir)?;
        tab.focused = next;
        Some(next)
    }

    /// Activates the pane's tab and focuses the pane.
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.tree.contains(pane)) else {
            return false;
        };
        self.active = i;
        self.tabs[i].focused = pane;
        true
    }

    pub fn tab_of(&self, pane: PaneId) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|t| t.tree.contains(pane))
            .map(|t| t.id)
    }

    /// Closes a pane; a tab's last pane closes the tab.
    pub fn close_pane(&mut self, pane: PaneId) -> Closed {
        let Some(i) = self.tabs.iter().position(|t| t.tree.contains(pane)) else {
            return Closed::NotFound;
        };
        let tab = &mut self.tabs[i];
        match tab.tree.remove(pane) {
            Some(next) => {
                if tab.focused == pane {
                    tab.focused = next;
                }
                Closed::Pane
            }
            None => {
                let id = tab.id;
                self.remove_at(i);
                Closed::Tab(id)
            }
        }
    }

    /// Closes a tab and returns its panes.
    pub fn close_tab(&mut self, id: TabId) -> Vec<PaneId> {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let panes = self.tabs[i].tree.panes();
        self.remove_at(i);
        panes
    }

    /// The tab to the right becomes active, or the one to the left if it was the last.
    fn remove_at(&mut self, i: usize) {
        self.tabs.remove(i);
        if i < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Tab ids by group, in strip order; `None` is the Loose group.
    pub fn groups(&self) -> Vec<(Option<String>, Vec<TabId>)> {
        let mut out: Vec<(Option<String>, Vec<TabId>)> = Vec::new();
        for t in &self.tabs {
            match out.last_mut() {
                Some((set, ids)) if *set == t.set => ids.push(t.id),
                _ => out.push((t.set.clone(), vec![t.id])),
            }
        }
        out
    }
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui tabs` — expected: PASS (6 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui tab list

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Commands, actions and the key map

**Files:**
- Modify: `Cargo.toml` (workspace: the GPUI stack and `futures`)
- Modify: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/keys.rs`
- Test: unit tests in `keys.rs`

**Interfaces:**
- Consumes: `UiConfig.keys` (Task 1); `gpui::{Keystroke, KeyBinding, DummyKeyboardMapper, Action, actions!}`.
- Produces (used by Tasks 9, 10):
  - GPUI actions `keys::{NewTab, ClosePane, SplitRight, SplitDown, FocusLeft, FocusRight, FocusUp, FocusDown, NextTab, PreviousTab, CopyText, PasteText, Find}` (namespace `hydraui`)
  - `keys::Command` with the same 13 variants (`Copy` → `CopyText`, `Paste` → `PasteText`): `Command::ALL`, `Command::name(self) -> &'static str` (the `[keys]` name), `Command::default_keys(self) -> &'static str`, `Command::action(self) -> Box<dyn Action>`
  - `keys::keymap(&BTreeMap<String, String>) -> (Vec<(Command, String)>, Vec<String>)`, `keys::bindings(&[(Command, String)]) -> Vec<KeyBinding>`

- [ ] **Step 1: Add the UI stack.** In the root `Cargo.toml` `[workspace.dependencies]`, add after `hydraui-term = { path = "crates/hydraui-term" }`:

```toml
gpui = { package = "gpui-pre", version = "=0.3.9" }
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.9", features = ["font-kit", "runtime_shaders"] }
gpui-component = "=0.7.3"
gpui-kit-assets = "=0.7.3"
futures = "0.3"
```

In `crates/hydraui/Cargo.toml` `[dependencies]`, add after `hydraui-term.workspace = true`:

```toml
gpui.workspace = true
```

In `crates/hydraui/src/lib.rs`, add `pub mod keys;` after `pub mod input;`.

The first build of the GPUI stack takes several minutes (the spike's clean release build took about 5).

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/keys.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_defaults_bind_every_command() {
        let (keys, problems) = keymap(&BTreeMap::new());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(keys.len(), Command::ALL.len());
        assert!(keys.contains(&(Command::SplitRight, "ctrl-shift-d".to_string())));
        assert!(keys.contains(&(Command::NextTab, "ctrl-tab".to_string())));
        assert_eq!(Command::Copy.name(), "copy");
    }

    #[test]
    fn overrides_replace_and_an_empty_string_unbinds() {
        let (keys, problems) = keymap(&map(&[("split_right", "ctrl-alt-d"), ("find", "")]));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(keys.contains(&(Command::SplitRight, "ctrl-alt-d".to_string())));
        assert!(!keys.iter().any(|(c, _)| *c == Command::Find));
        assert_eq!(keys.len(), Command::ALL.len() - 1);
    }

    #[test]
    fn mistakes_are_reported_and_the_default_kept() {
        let (keys, problems) =
            keymap(&map(&[("split_rigth", "ctrl-x"), ("copy", "ctrl-shift-nope-c")]));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("split_rigth")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("copy")), "{problems:?}");
        assert!(keys.contains(&(Command::Copy, "ctrl-shift-c".to_string())));
    }

    #[test]
    fn the_same_keys_on_two_commands_is_reported() {
        let (_, problems) = keymap(&map(&[("find", "ctrl-t")]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("new_tab") && problems[0].contains("find"),
            "{problems:?}"
        );
    }

    #[test]
    fn every_entry_becomes_a_binding() {
        let (keys, _) = keymap(&BTreeMap::new());
        assert_eq!(bindings(&keys).len(), Command::ALL.len());
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui keys` — expected: compile errors (`keymap`, `Command`, `bindings` not found).

- [ ] **Step 4: Implement.** Above the tests in `keys.rs`:

```rust
//! hydraui's commands and their keys (spec §7.6): the defaults, `[keys]` overrides from
//! config.toml, and the GPUI actions they dispatch. The launcher and file tree commands
//! arrive with those features (plan 4).
use std::collections::BTreeMap;

use gpui::{Action, DummyKeyboardMapper, KeyBinding, Keystroke};

gpui::actions!(
    hydraui,
    [
        NewTab,
        ClosePane,
        SplitRight,
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        NextTab,
        PreviousTab,
        CopyText,
        PasteText,
        Find
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    NewTab,
    ClosePane,
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    NextTab,
    PreviousTab,
    Copy,
    Paste,
    Find,
}

impl Command {
    pub const ALL: [Command; 13] = [
        Command::NewTab,
        Command::ClosePane,
        Command::SplitRight,
        Command::SplitDown,
        Command::FocusLeft,
        Command::FocusRight,
        Command::FocusUp,
        Command::FocusDown,
        Command::NextTab,
        Command::PreviousTab,
        Command::Copy,
        Command::Paste,
        Command::Find,
    ];

    /// The name under `[keys]` in config.toml.
    pub fn name(self) -> &'static str {
        match self {
            Command::NewTab => "new_tab",
            Command::ClosePane => "close_pane",
            Command::SplitRight => "split_right",
            Command::SplitDown => "split_down",
            Command::FocusLeft => "focus_left",
            Command::FocusRight => "focus_right",
            Command::FocusUp => "focus_up",
            Command::FocusDown => "focus_down",
            Command::NextTab => "next_tab",
            Command::PreviousTab => "previous_tab",
            Command::Copy => "copy",
            Command::Paste => "paste",
            Command::Find => "find",
        }
    }

    /// The spec §7.6 defaults.
    pub fn default_keys(self) -> &'static str {
        match self {
            Command::NewTab => "ctrl-t",
            Command::ClosePane => "ctrl-w",
            Command::SplitRight => "ctrl-shift-d",
            Command::SplitDown => "ctrl-shift-e",
            Command::FocusLeft => "alt-left",
            Command::FocusRight => "alt-right",
            Command::FocusUp => "alt-up",
            Command::FocusDown => "alt-down",
            Command::NextTab => "ctrl-tab",
            Command::PreviousTab => "ctrl-shift-tab",
            Command::Copy => "ctrl-shift-c",
            Command::Paste => "ctrl-shift-v",
            Command::Find => "ctrl-f",
        }
    }

    pub fn action(self) -> Box<dyn Action> {
        match self {
            Command::NewTab => Box::new(NewTab),
            Command::ClosePane => Box::new(ClosePane),
            Command::SplitRight => Box::new(SplitRight),
            Command::SplitDown => Box::new(SplitDown),
            Command::FocusLeft => Box::new(FocusLeft),
            Command::FocusRight => Box::new(FocusRight),
            Command::FocusUp => Box::new(FocusUp),
            Command::FocusDown => Box::new(FocusDown),
            Command::NextTab => Box::new(NextTab),
            Command::PreviousTab => Box::new(PreviousTab),
            Command::Copy => Box::new(CopyText),
            Command::Paste => Box::new(PasteText),
            Command::Find => Box::new(Find),
        }
    }
}

/// Each command's keys after `[keys]` overrides (`""` unbinds), plus what was wrong. A bad
/// keystroke keeps the default, so one typo doesn't lose a command.
pub fn keymap(overrides: &BTreeMap<String, String>) -> (Vec<(Command, String)>, Vec<String>) {
    let mut problems = Vec::new();
    for name in overrides.keys() {
        if !Command::ALL.iter().any(|c| c.name() == name) {
            problems.push(format!("[keys]: unknown command {name:?}"));
        }
    }
    let mut out = Vec::new();
    for c in Command::ALL {
        let keys = overrides
            .get(c.name())
            .map_or(c.default_keys(), |k| k.trim());
        if keys.is_empty() {
            continue;
        }
        match keys
            .split_whitespace()
            .try_for_each(|k| Keystroke::parse(k).map(|_| ()))
        {
            Ok(()) => out.push((c, keys.to_string())),
            Err(e) => {
                problems.push(format!("[keys] {} = {keys:?}: {e}", c.name()));
                out.push((c, c.default_keys().to_string()));
            }
        }
    }
    let mut seen: BTreeMap<String, Command> = BTreeMap::new();
    for (c, k) in &out {
        if let Some(earlier) = seen.insert(k.to_lowercase(), *c) {
            problems.push(format!(
                "[keys] {k:?} is bound to both {} and {}",
                earlier.name(),
                c.name()
            ));
        }
    }
    (out, problems)
}

/// GPUI key bindings for `keymap`'s result. They have no context, so they work wherever
/// the focus is.
pub fn bindings(keys: &[(Command, String)]) -> Vec<KeyBinding> {
    keys.iter()
        .filter_map(|(c, k)| {
            KeyBinding::load(k, c.action(), None, false, None, &DummyKeyboardMapper).ok()
        })
        .collect()
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui keys` — expected: PASS (5 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui
git commit -m "feat: hydraui commands and configurable keys

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: `Hydra`: environments, prepare, panes and whoami

**Files:**
- Modify: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/hydra.rs`
- Test: unit tests in `hydra.rs`

**Interfaces:**
- Consumes: `hydra_core::{bindings::resolve, config::{GlobalConfig, load_global, load_env}, envs::list, resolve::{prepare, PrepareOptions, ENV_VARS_MARKER, LaunchEnv}, provider::{Ctx, IdentityReport, Status, Warnings}}`; `hydra_platform::{keyring_store::KeyringStore, process::EnvRunner, shell::ShellKind}`; `hydra_providers::all()`; `hydraui_term::{Pane, SpawnSpec, GridSize, TermEvent}`; `palette::env_tint` (Task 2).
- Produces (used by Tasks 9, 10):
  - `hydra::PaneRequest { env: String, cwd: PathBuf, shell: ShellKind, command: Option<String> }` (`Debug, Clone, PartialEq`)
  - `hydra::Started { pane: Pane, events: Receiver<TermEvent>, warnings: Vec<String> }`
  - `hydra::Hydra` (`Send + Sync`; pub fields `paths: HydraPaths`, `user_home: PathBuf`, `global: GlobalConfig`): `from_env() -> Result<(Hydra, Option<String>), String>`, `new(HydraPaths, PathBuf, KeyringStore) -> (Hydra, Option<String>)`, `env_names() -> Vec<String>`, `env_exists(&str) -> bool`, `adhoc_env(&Path, default_env: Option<&str>) -> Option<String>`, `shell(ui_default: Option<&str>) -> ShellKind`, `tint(&str) -> u32`, `prepare(&str) -> Result<LaunchEnv, String>`, `start(&PaneRequest, GridSize) -> Result<Started, String>`, `whoami(&str) -> Vec<IdentityReport>`
  - `hydra::accounts_summary(&[IdentityReport]) -> String`; `hydra::STATUS_TTL: Duration` (60 s); `hydra::StatusCache` (`Default`): `get(&str) -> Option<&str>`, `is_fresh(&str, Instant) -> bool`, `store(String, String, Instant)`

- [ ] **Step 1: Add the dependencies and module.** In `crates/hydraui/Cargo.toml` `[dependencies]`, add after `hydra-platform.workspace = true`:

```toml
hydra-providers.workspace = true
dirs.workspace = true
```

In `crates/hydraui/src/lib.rs`, add `pub mod hydra;` after `pub mod frame;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/hydra.rs`:

```rust
#[cfg(test)]
mod tests {
    use hydra_platform::shell::find_shell;

    use super::*;

    struct Fx {
        _d: tempfile::TempDir,
        hydra: Hydra,
        bound: PathBuf,
        free: PathBuf,
    }

    /// `work` (no tools) and `personal` (coloured); `config` with BOUND standing for a
    /// folder bound to personal.
    fn fx(config: &str) -> Fx {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path().join(".hydra"));
        for (env, toml) in [("work", ""), ("personal", "color = \"#112233\"\n")] {
            let n = EnvName::parse(env).unwrap();
            std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
            std::fs::write(paths.env_file(&n), toml).unwrap();
        }
        let bound = d.path().join("bound");
        let free = d.path().join("free");
        std::fs::create_dir_all(&bound).unwrap();
        std::fs::create_dir_all(&free).unwrap();
        let fwd = bound.to_string_lossy().replace('\\', "/");
        std::fs::write(paths.config_file(), config.replace("BOUND", &fwd)).unwrap();
        let store = KeyringStore::new(
            format!("hydraui-test-{}", std::process::id()),
            paths.secret_index(),
        );
        let (hydra, problem) = Hydra::new(paths, d.path().join("home"), store);
        assert_eq!(problem, None);
        Fx {
            _d: d,
            hydra,
            bound,
            free,
        }
    }

    const BOUND: &str = "[bindings]\n\"BOUND/**\" = \"personal\"\n";

    #[test]
    fn lists_environments() {
        let f = fx("");
        let mut names = f.hydra.env_names();
        names.sort();
        assert_eq!(names, vec!["personal", "work"]);
        assert!(f.hydra.env_exists("work"));
        assert!(!f.hydra.env_exists("nobody"));
        assert!(!f.hydra.env_exists("Not A Name"));
    }

    #[test]
    fn a_new_tab_uses_the_binding_then_default_env_then_asks() {
        let f = fx(BOUND);
        assert_eq!(
            f.hydra.adhoc_env(&f.bound, Some("work")).as_deref(),
            Some("personal")
        );
        assert_eq!(
            f.hydra.adhoc_env(&f.free, Some("work")).as_deref(),
            Some("work")
        );
        assert_eq!(f.hydra.adhoc_env(&f.free, Some("nobody")), None);
        assert_eq!(f.hydra.adhoc_env(&f.free, None), None);
    }

    #[test]
    fn shell_choice_is_hydraui_then_hydra_then_pwsh() {
        assert_eq!(fx("").hydra.shell(Some("bash")), ShellKind::Bash);
        assert_eq!(fx("").hydra.shell(None), ShellKind::Pwsh);
        let f = fx("default_shell = \"bash\"\n");
        assert_eq!(f.hydra.shell(None), ShellKind::Bash);
        assert_eq!(f.hydra.shell(Some("pwsh")), ShellKind::Pwsh);
        assert_eq!(f.hydra.shell(Some("fish")), ShellKind::Bash);
    }

    #[test]
    fn tints_come_from_env_toml_or_the_name() {
        let f = fx("");
        assert_eq!(f.hydra.tint("personal"), 0x112233);
        assert_eq!(f.hydra.tint("work"), env_tint("work", None));
    }

    #[test]
    fn a_bad_hydra_config_is_reported_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::write(paths.config_file(), "bindings = 3\n").unwrap();
        let store = KeyringStore::new("hydraui-test-unused", paths.secret_index());
        let (hydra, problem) = Hydra::new(paths, d.path().to_path_buf(), store);
        assert!(problem.unwrap().contains("config.toml"));
        assert!(hydra.global.bindings.is_empty());
    }

    #[test]
    fn prepare_works_and_a_missing_environment_says_so() {
        let f = fx("");
        assert_eq!(f.hydra.prepare("work").unwrap().name.as_str(), "work");
        assert!(f.hydra.prepare("nobody").unwrap_err().contains("nobody"));
    }

    #[test]
    fn whoami_of_an_environment_without_tools_is_empty() {
        assert!(fx("").hydra.whoami("work").is_empty());
        assert!(fx("").hydra.whoami("nobody").is_empty());
    }

    #[test]
    fn start_runs_a_real_pane() {
        if find_shell(ShellKind::Pwsh, None).is_none() {
            eprintln!("skipped: pwsh isn't installed");
            return;
        }
        let f = fx("");
        let req = PaneRequest {
            env: "work".into(),
            cwd: f.free.clone(),
            shell: ShellKind::Pwsh,
            command: None,
        };
        let started = f.hydra.start(&req, GridSize { cols: 80, rows: 24 }).unwrap();
        assert!(started.warnings.is_empty());
        let missing = PaneRequest {
            cwd: f.free.join("gone"),
            ..req
        };
        assert!(f
            .hydra
            .start(&missing, GridSize { cols: 80, rows: 24 })
            .err()
            .unwrap()
            .contains("gone"));
    }

    #[test]
    fn accounts_summary_skips_info_rows() {
        let r = |provider: &str, status, detail: &str| IdentityReport {
            provider: provider.into(),
            status,
            detail: detail.into(),
        };
        let rows = [
            r("github", Status::Ok, "davojc"),
            r("env", Status::Info, "3 variables"),
            r("claude", Status::Missing, "not signed in"),
        ];
        assert_eq!(
            accounts_summary(&rows),
            "github: davojc · claude: not signed in (!)"
        );
    }

    #[test]
    fn status_cache_is_fresh_for_a_minute() {
        let mut c = StatusCache::default();
        let t0 = Instant::now();
        assert!(!c.is_fresh("work", t0));
        assert_eq!(c.get("work"), None);
        c.store("work".into(), "github: x".into(), t0);
        assert!(c.is_fresh("work", t0 + Duration::from_secs(59)));
        assert!(!c.is_fresh("work", t0 + STATUS_TTL));
        assert_eq!(c.get("work"), Some("github: x"));
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui hydra` — expected: compile errors (`Hydra`, `PaneRequest`, … not found).

- [ ] **Step 4: Implement.** Above the tests in `hydra.rs`:

```rust
//! hydraui's link to hydra (spec §3.1, §4, §7.1): environments, `prepare` with guards off,
//! starting panes, and the identity checks for the status bar. No UI.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use hydra_core::bindings;
use hydra_core::config::{GlobalConfig, load_env, load_global};
use hydra_core::envs;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::provider::{Ctx, IdentityReport, Status, Warnings};
use hydra_core::resolve::{ENV_VARS_MARKER, LaunchEnv, PrepareOptions, prepare};
use hydra_platform::keyring_store::KeyringStore;
use hydra_platform::process::EnvRunner;
use hydra_platform::shell::ShellKind;
use hydraui_term::{GridSize, Pane, SpawnSpec, TermEvent};

use crate::palette::env_tint;

/// What a pane runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneRequest {
    pub env: String,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// Shell text run after the shell starts.
    pub command: Option<String>,
}

/// A started pane, with the warnings `prepare` gave (shown as the pane's strip).
pub struct Started {
    pub pane: Pane,
    pub events: Receiver<TermEvent>,
    pub warnings: Vec<String>,
}

pub struct Hydra {
    pub paths: HydraPaths,
    pub user_home: PathBuf,
    /// hydra's config.toml: bindings, `default_shell`, `git_bash`.
    pub global: GlobalConfig,
    store: KeyringStore,
}

impl Hydra {
    /// Finds `HYDRA_HOME` (else `~/.hydra`) and the user's home (`HYDRA_USER_HOME`, else the
    /// real one) as the CLI does. A config.toml that can't be read gives defaults plus the
    /// problem.
    pub fn from_env() -> Result<(Hydra, Option<String>), String> {
        let paths = HydraPaths::discover().map_err(|e| e.to_string())?;
        let user_home = match std::env::var_os("HYDRA_USER_HOME") {
            Some(h) => PathBuf::from(h),
            None => dirs::home_dir().ok_or("can't find your home folder")?,
        };
        let store = KeyringStore::from_env(&paths);
        Ok(Hydra::new(paths, user_home, store))
    }

    pub fn new(paths: HydraPaths, user_home: PathBuf, store: KeyringStore) -> (Hydra, Option<String>) {
        let (global, problem) = match load_global(&paths) {
            Ok(g) => (g, None),
            Err(e) => (
                GlobalConfig::default(),
                Some(format!(
                    "{}: {e} - folder bindings ignored",
                    paths.config_file().display()
                )),
            ),
        };
        (
            Hydra {
                paths,
                user_home,
                global,
                store,
            },
            problem,
        )
    }

    pub fn env_names(&self) -> Vec<String> {
        envs::list(&self.paths)
            .map(|l| l.valid.iter().map(|n| n.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn env_exists(&self, env: &str) -> bool {
        EnvName::parse(env).is_ok_and(|n| self.paths.env_file(&n).is_file())
    }

    /// The environment for a new Loose tab in `cwd`: the folder's binding, else
    /// `default_env`; `None` means ask (spec §7.6).
    pub fn adhoc_env(&self, cwd: &Path, default_env: Option<&str>) -> Option<String> {
        bindings::resolve(cwd, &self.global.bindings)
            .map(|b| b.env)
            .filter(|e| self.env_exists(e))
            .or_else(|| {
                default_env
                    .filter(|e| self.env_exists(e))
                    .map(str::to_string)
            })
    }

    /// hydraui's `default_shell`, else hydra's, else pwsh.
    pub fn shell(&self, ui_default: Option<&str>) -> ShellKind {
        ui_default
            .and_then(ShellKind::parse)
            .or_else(|| self.global.default_shell.as_deref().and_then(ShellKind::parse))
            .unwrap_or(ShellKind::Pwsh)
    }

    /// The environment's tab tint (spec §7.1).
    pub fn tint(&self, env: &str) -> u32 {
        let rgb = EnvName::parse(env)
            .ok()
            .and_then(|n| load_env(&self.paths, &n).ok())
            .and_then(|c| c.rgb())
            .map(|c| (c.0, c.1, c.2));
        env_tint(env, rgb)
    }

    /// `prepare` for a hydraui pane: guards off (spec §4.1).
    pub fn prepare(&self, env: &str) -> Result<LaunchEnv, String> {
        let name = EnvName::parse(env).map_err(|e| e.to_string())?;
        let opts = PrepareOptions {
            no_guards: true,
            inherited_env_vars: std::env::var(ENV_VARS_MARKER).ok(),
            ..Default::default()
        };
        prepare(
            &self.paths,
            &name,
            &self.user_home,
            &hydra_providers::all(),
            &self.store,
            &opts,
        )
        .map_err(|e| e.to_string())
    }

    /// Prepares the environment and starts the pane's shell. The error is the pane's banner
    /// text (spec §7.5).
    pub fn start(&self, req: &PaneRequest, size: GridSize) -> Result<Started, String> {
        let launch = self.prepare(&req.env)?;
        let warnings = launch.warnings.clone();
        let spec = SpawnSpec {
            paths: self.paths.clone(),
            launch,
            cwd: req.cwd.clone(),
            shell: req.shell,
            git_bash: self.global.git_bash.clone(),
            command: req.command.clone(),
            size,
            inherited: std::env::vars_os().collect(),
        };
        let (pane, events) = Pane::spawn(spec).map_err(|e| e.to_string())?;
        Ok(Started {
            pane,
            events,
            warnings,
        })
    }

    /// What `hydra whoami` reports for `env`'s configured tools, with missing secrets
    /// allowed. Slow (it runs the tools): call it off the UI thread.
    pub fn whoami(&self, env: &str) -> Vec<IdentityReport> {
        let Ok(name) = EnvName::parse(env) else {
            return Vec::new();
        };
        let opts = PrepareOptions {
            allow_missing_secrets: true,
            no_guards: true,
            ..Default::default()
        };
        let providers = hydra_providers::all();
        let Ok(launch) = prepare(
            &self.paths,
            &name,
            &self.user_home,
            &providers,
            &self.store,
            &opts,
        ) else {
            return Vec::new();
        };
        let warnings = Warnings::default();
        let ctx = Ctx {
            name: &name,
            env: &launch.config,
            paths: &self.paths,
            user_home: &self.user_home,
            secrets: &self.store,
            guards: false,
            warnings: &warnings,
        };
        let runner = EnvRunner { launch: &launch };
        providers
            .iter()
            .filter(|p| p.is_configured(&launch.config))
            .map(|p| p.check(&ctx, &runner))
            .collect()
    }
}

/// The status bar's accounts: `github: davojc · claude: me@x.com`, skipping info-only rows
/// and marking problems with `(!)`.
pub fn accounts_summary(reports: &[IdentityReport]) -> String {
    reports
        .iter()
        .filter(|r| r.status != Status::Info)
        .map(|r| {
            let mark = if r.status == Status::Ok { "" } else { " (!)" };
            format!("{}: {}{mark}", r.provider, r.detail)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// How long an environment's accounts stay on the status bar before they are checked again
/// (spec §7.1).
pub const STATUS_TTL: Duration = Duration::from_secs(60);

/// The last accounts summary per environment.
#[derive(Debug, Default)]
pub struct StatusCache {
    entries: HashMap<String, (Instant, String)>,
}

impl StatusCache {
    /// The last summary, however old.
    pub fn get(&self, env: &str) -> Option<&str> {
        self.entries.get(env).map(|(_, s)| s.as_str())
    }

    pub fn is_fresh(&self, env: &str, now: Instant) -> bool {
        self.entries
            .get(env)
            .is_some_and(|(at, _)| now.duration_since(*at) < STATUS_TTL)
    }

    pub fn store(&mut self, env: String, summary: String, now: Instant) {
        self.entries.insert(env, (now, summary));
    }
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui hydra` — expected: PASS (10 tests; `start_runs_a_real_pane` prints "skipped" only without pwsh).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.lock crates/hydraui
git commit -m "feat: hydraui hydra service for environments, panes and whoami

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: `PaneView`: the terminal view

**Files:**
- Modify: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/pane_view.rs`
- Test: unit test in `pane_view.rs` (cell hit-testing); the rest is checked by hand in Task 10's smoke checklist

**Interfaces:**
- Consumes: `frame::{build_frame, link_at, find_next, Frame, Style, CursorKind}` (Task 3); `input::{key_bytes, paste_bytes, mouse_report, wheel, Mods, ReportButton, MouseKind, Wheel}` (Task 4); `split::PaneId` (Task 5); `keys::{CopyText, PasteText, Find}` (Task 7); `hydra::{Hydra, PaneRequest, Started}` (Task 8); `palette::Palette` (Task 2); `hydraui_term::{Pane, GridSize, TermEvent, PromptState}`.
- Produces (used by Task 10):
  - `pane_view::TermStyle { font: SharedString, font_size: Pixels, palette: Palette }`
  - `pane_view::PaneEvent::{Focused, CwdChanged, TitleChanged, Exited}` (`Debug, Clone, PartialEq`)
  - `pane_view::PaneView` (`Render`, `Focusable`, `EventEmitter<PaneEvent>`, `EntityInputHandler`): `PaneView::new(id: PaneId, request: PaneRequest, hydra: Arc<Hydra>, style: Arc<TermStyle>, window: &mut Window, cx: &mut Context<Self>) -> Self`, pub fields `id`, `request`; `cwd(&self) -> &PathBuf`, `env(&self) -> &str`, `prompt(&self) -> Option<PromptState>`, `label(&self) -> String`

- [ ] **Step 1: Add the dependencies and module.** In `crates/hydraui/Cargo.toml` `[dependencies]`, add after `gpui.workspace = true`:

```toml
gpui-component.workspace = true
futures.workspace = true
```

In `crates/hydraui/src/lib.rs`, add `pub mod pane_view;` after `pub mod palette;`.

- [ ] **Step 2: Write the failing test.** `crates/hydraui/src/pane_view.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_found_from_offsets_and_clamped_to_the_grid() {
        let size = GridSize { cols: 80, rows: 24 };
        let cell = (10.0, 20.0);
        assert_eq!(grid_cell(0.0, 0.0, cell, size), (0, 0, Side::Left));
        assert_eq!(grid_cell(26.0, 41.0, cell, size), (2, 2, Side::Right));
        assert_eq!(grid_cell(24.0, 41.0, cell, size), (2, 2, Side::Left));
        assert_eq!(grid_cell(-5.0, -5.0, cell, size), (0, 0, Side::Left));
        assert_eq!(grid_cell(5000.0, 5000.0, cell, size), (23, 79, Side::Left));
    }
}
```

- [ ] **Step 3: Run it to see it fail.** `cargo test -p hydraui pane_view` — expected: compile error (`grid_cell` not found).

- [ ] **Step 4: Implement.** Above the test in `pane_view.rs`:

```rust
//! One terminal pane (spec §7.2, §7.5, §6.4): starts its shell through hydra off the UI
//! thread, paints the grid, and turns keys, mouse and clipboard into input for the shell.
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, ClipboardItem, Context, DispatchPhase,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    Hsla, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, StrikethroughStyle, Subscription,
    Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, canvas, div, fill, font,
    outline, point, px, rgb, size,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use hydraui_term::alacritty_terminal::grid::Scroll;
use hydraui_term::alacritty_terminal::index::{Column, Direction, Point as GridPoint, Side};
use hydraui_term::alacritty_terminal::selection::{Selection, SelectionType};
use hydraui_term::alacritty_terminal::term::search::Match;
use hydraui_term::alacritty_terminal::term::{TermMode, viewport_to_point};
use hydraui_term::{GridSize, Pane, PromptState, TermEvent};

use crate::frame::{CursorKind, Frame, Style, build_frame, find_next, link_at};
use crate::hydra::{Hydra, PaneRequest, Started};
use crate::input::{
    Mods, MouseKind, ReportButton, Wheel, key_bytes, mouse_report, paste_bytes, wheel,
};
use crate::keys::{CopyText, Find, PasteText};
use crate::palette::Palette;
use crate::split::PaneId;

/// The font and colours every pane draws with.
pub struct TermStyle {
    pub font: SharedString,
    pub font_size: Pixels,
    pub palette: Palette,
}

/// What a pane tells the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneEvent {
    Focused,
    CwdChanged,
    TitleChanged,
    Exited,
}

enum Slot {
    Starting,
    Running(Pane),
    /// The shell ended; its pane is closed (job, PTY and lock released).
    Exited(Option<i32>),
    /// `prepare` or the spawn failed: hydra's error text.
    Failed(String),
}

struct Finder {
    input: Entity<InputState>,
    current: Option<Match>,
    _sub: Subscription,
}

pub struct PaneView {
    pub id: PaneId,
    pub request: PaneRequest,
    hydra: Arc<Hydra>,
    style: Arc<TermStyle>,
    slot: Slot,
    warnings: Vec<String>,
    cwd: PathBuf,
    prompt: Option<PromptState>,
    title: Option<String>,
    focus: FocusHandle,
    size: GridSize,
    /// Cell width and height, measured at the last paint.
    cell: Option<(Pixels, Pixels)>,
    /// The grid's top-left corner in window coordinates, at the last paint.
    origin: Point<Pixels>,
    cursor_cell: Option<(usize, usize)>,
    selecting: bool,
    finder: Option<Finder>,
    /// Bumped on every start, so events from an older shell are ignored.
    generation: u64,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for PaneView {}

impl Focusable for PaneView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn color(c: u32) -> Hsla {
    rgb(c).into()
}

fn mods(m: Modifiers) -> Mods {
    Mods {
        ctrl: m.control,
        alt: m.alt,
        shift: m.shift,
    }
}

fn report_button(b: MouseButton) -> Option<ReportButton> {
    match b {
        MouseButton::Left => Some(ReportButton::Left),
        MouseButton::Middle => Some(ReportButton::Middle),
        MouseButton::Right => Some(ReportButton::Right),
        _ => None,
    }
}

/// The cell at an offset (in pixels) from the grid's corner, clamped to the grid, and which
/// half of the cell it is in.
fn grid_cell(dx: f32, dy: f32, cell: (f32, f32), size: GridSize) -> (usize, usize, Side) {
    let x = (dx / cell.0).max(0.0);
    let y = (dy / cell.1).max(0.0);
    let col = (x as usize).min(usize::from(size.cols) - 1);
    let row = (y as usize).min(usize::from(size.rows) - 1);
    let side = if x - x.floor() < 0.5 || x as usize > col {
        Side::Left
    } else {
        Side::Right
    };
    (row, col, side)
}

fn grid_point(display_offset: usize, row: usize, col: usize) -> GridPoint {
    viewport_to_point(display_offset, GridPoint::new(row, Column(col)))
}

impl PaneView {
    pub fn new(
        id: PaneId,
        request: PaneRequest,
        hydra: Arc<Hydra>,
        style: Arc<TermStyle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let on_focus = cx.on_focus(&focus, window, |_, _, cx| cx.emit(PaneEvent::Focused));
        let mut view = Self {
            id,
            cwd: request.cwd.clone(),
            request,
            hydra,
            style,
            slot: Slot::Starting,
            warnings: Vec::new(),
            prompt: None,
            title: None,
            focus,
            size: GridSize { cols: 80, rows: 24 },
            cell: None,
            origin: Point::default(),
            cursor_cell: None,
            selecting: false,
            finder: None,
            generation: 0,
            _tasks: Vec::new(),
            _subs: vec![on_focus],
        };
        view.start(cx);
        view
    }

    /// The shell's current folder (OSC 7), else where it started.
    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn env(&self) -> &str {
        &self.request.env
    }

    /// Where the shell is (OSC 133); `None` when it doesn't say.
    pub fn prompt(&self) -> Option<PromptState> {
        self.prompt
    }

    /// The tab label: the title a program set, else the folder's name.
    pub fn label(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.cwd.display().to_string())
        })
    }

    /// Runs `prepare` and starts the shell off the UI thread. Also Retry and restart, so an
    /// edited env.toml applies (spec §6.4).
    fn start(&mut self, cx: &mut Context<Self>) {
        self.slot = Slot::Starting;
        self.prompt = None;
        self.generation += 1;
        let generation = self.generation;
        let (hydra, request, size) = (self.hydra.clone(), self.request.clone(), self.size);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { hydra.start(&request, size) })
                .await;
            let _ = this.update(cx, |view, cx| view.started(generation, result, cx));
        });
        self._tasks = vec![task];
        cx.notify();
    }

    fn started(&mut self, generation: u64, result: Result<Started, String>, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        match result {
            Ok(s) => {
                // The view may have been laid out at another size while the shell started.
                let _ = s.pane.resize(self.size);
                self.warnings = s.warnings;
                self._tasks.push(pump(s.events, generation, cx));
                self.slot = Slot::Running(s.pane);
            }
            Err(e) => self.slot = Slot::Failed(e),
        }
        cx.notify();
    }

    fn on_term_event(&mut self, generation: u64, event: TermEvent, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        match event {
            TermEvent::Wakeup => cx.notify(),
            TermEvent::CwdChanged(p) => {
                self.cwd = p;
                cx.emit(PaneEvent::CwdChanged);
            }
            TermEvent::Prompt(p) => self.prompt = Some(p),
            TermEvent::Title(t) => {
                self.title = (!t.is_empty()).then_some(t);
                cx.emit(PaneEvent::TitleChanged);
            }
            TermEvent::Bell => {}
            TermEvent::Exited(code) => {
                self.slot = Slot::Exited(code);
                self.prompt = None;
                self.finder = None;
                cx.emit(PaneEvent::Exited);
                cx.notify();
            }
        }
    }

    fn mode(&self) -> Option<TermMode> {
        match &self.slot {
            Slot::Running(pane) => Some(pane.with_term(|t| *t.mode())),
            _ => None,
        }
    }

    /// Input for the shell. Typing returns the view to the live screen.
    fn send(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if let Slot::Running(pane) = &self.slot {
            pane.with_term(|t| t.scroll_display(Scroll::Bottom));
            let _ = pane.write(bytes);
            cx.notify();
        }
    }

    fn resize(&mut self, size: GridSize) {
        if size != self.size {
            self.size = size;
            if let Slot::Running(pane) = &self.slot {
                let _ = pane.resize(size);
            }
        }
    }

    fn frame(&self, focused: bool) -> Option<Frame> {
        let Slot::Running(pane) = &self.slot else {
            return None;
        };
        Some(pane.with_term(|t| build_frame(t, &self.style.palette, focused)))
    }

    fn cell_at(&self, pos: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let (cw, ch) = self.cell?;
        Some(grid_cell(
            f32::from(pos.x - self.origin.x),
            f32::from(pos.y - self.origin.y),
            (f32::from(cw), f32::from(ch)),
            self.size,
        ))
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if !self.focus.is_focused(window) {
            // The find box has the focus: Escape closes it, other keys are the box's.
            if k.key == "escape" && self.finder.is_some() {
                self.close_finder(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if ev.prefer_character_input {
            // AltGr text: it arrives through the input handler.
            return;
        }
        if matches!(self.slot, Slot::Exited(_) | Slot::Failed(_)) {
            if k.key == "enter" && !k.modifiers.modified() {
                self.start(cx);
                cx.stop_propagation();
            }
            return;
        }
        let Some(mode) = self.mode() else {
            return;
        };
        if let Some(bytes) = key_bytes(&k.key, k.key_char.as_deref(), mods(k.modifiers), mode) {
            self.send(&bytes, cx);
            // Also stops Windows turning the key into text for the input handler.
            cx.stop_propagation();
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (Slot::Running(pane), Some((row, col, side))) = (&self.slot, self.cell_at(ev.position))
        else {
            return;
        };
        if ev.button == MouseButton::Left && ev.modifiers.control {
            if let Some(url) = pane.with_term(|t| link_at(t, row, col)) {
                cx.open_url(&url);
            }
            return;
        }
        let mode = pane.with_term(|t| *t.mode());
        // Shift+click selects even when the program reports the mouse.
        if !ev.modifiers.shift
            && let Some(button) = report_button(ev.button)
            && let Some(bytes) =
                mouse_report(Some(button), MouseKind::Press, col, row, mods(ev.modifiers), mode)
        {
            let _ = pane.write(&bytes);
            return;
        }
        if ev.button == MouseButton::Left {
            let ty = match ev.click_count {
                2 => SelectionType::Semantic,
                n if n >= 3 => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            pane.with_term(|t| {
                let at = grid_point(t.grid().display_offset(), row, col);
                t.selection = Some(Selection::new(ty, at, side));
            });
            self.selecting = true;
            cx.notify();
        }
    }

    /// A selection drag, from the window-wide listener (it keeps going outside the pane).
    fn drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let (Slot::Running(pane), Some((row, col, side)), Some((_, ch))) =
            (&self.slot, self.cell_at(pos), self.cell)
        else {
            return;
        };
        let above = pos.y < self.origin.y;
        let below = pos.y > self.origin.y + ch * f32::from(self.size.rows);
        pane.with_term(|t| {
            if above {
                t.scroll_display(Scroll::Delta(1));
            } else if below {
                t.scroll_display(Scroll::Delta(-1));
            }
            let at = grid_point(t.grid().display_offset(), row, col);
            if let Some(s) = t.selection.as_mut() {
                s.update(at, side);
            }
        });
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Slot::Running(pane) = &self.slot {
            pane.with_term(|t| {
                if t.selection.as_ref().is_some_and(|s| s.is_empty()) {
                    t.selection = None;
                }
            });
        }
        cx.notify();
    }

    fn report(&self, button: Option<ReportButton>, kind: MouseKind, pos: Point<Pixels>, m: Modifiers) {
        let (Slot::Running(pane), Some((row, col, _))) = (&self.slot, self.cell_at(pos)) else {
            return;
        };
        if m.shift {
            return;
        }
        let mode = pane.with_term(|t| *t.mode());
        if let Some(bytes) = mouse_report(button, kind, col, row, mods(m), mode) {
            let _ = pane.write(&bytes);
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.selecting {
            self.report(report_button(ev.button), MouseKind::Release, ev.position, ev.modifiers);
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.selecting {
            self.report(
                ev.pressed_button.and_then(report_button),
                MouseKind::Move,
                ev.position,
                ev.modifiers,
            );
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Slot::Running(pane), Some((_, ch))) = (&self.slot, self.cell) else {
            return;
        };
        let lines = match ev.delta {
            ScrollDelta::Lines(p) => (p.y * 3.0).round() as i32,
            ScrollDelta::Pixels(p) => (f32::from(p.y) / f32::from(ch)).round() as i32,
        };
        if lines == 0 {
            return;
        }
        let (row, col, _) = self.cell_at(ev.position).unwrap_or((0, 0, Side::Left));
        let mode = pane.with_term(|t| *t.mode());
        match wheel(lines, col, row, mods(ev.modifiers), mode) {
            Wheel::Send(bytes) => {
                let _ = pane.write(&bytes);
            }
            Wheel::Scroll(n) => {
                pane.with_term(|t| t.scroll_display(Scroll::Delta(n)));
                cx.notify();
            }
        }
    }

    fn copy_text(&mut self, _: &CopyText, _: &mut Window, cx: &mut Context<Self>) {
        if let Slot::Running(pane) = &self.slot
            && let Some(text) = pane.with_term(|t| t.selection_to_string())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_text(&mut self, _: &PasteText, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(mode) = self.mode() else {
            return;
        };
        self.send(&paste_bytes(&text, mode.contains(TermMode::BRACKETED_PASTE)), cx);
    }

    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.slot, Slot::Running(_)) {
            return;
        }
        let input = match &self.finder {
            Some(f) => f.input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
                let sub = cx.subscribe_in(&input, window, |view, _, ev: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { shift, .. } = ev {
                        let direction = if *shift {
                            Direction::Right
                        } else {
                            Direction::Left
                        };
                        view.find_step(direction, cx);
                    }
                });
                self.finder = Some(Finder {
                    input: input.clone(),
                    current: None,
                    _sub: sub,
                });
                input
            }
        };
        input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// The next match up (Enter) or down (Shift+Enter), selected and scrolled into view.
    fn find_step(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let (Some(finder), Slot::Running(pane)) = (&mut self.finder, &self.slot) else {
            return;
        };
        let query = finder.input.read(cx).value().to_string();
        let current = finder.current.clone();
        finder.current = pane.with_term(|t| {
            let found = find_next(t, &query, current.as_ref(), direction);
            if let Some(m) = &found {
                t.scroll_to_point(*m.start());
                let mut s = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                s.update(*m.end(), Side::Right);
                t.selection = Some(s);
            }
            found
        });
        cx.notify();
    }

    fn close_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn grid(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let view = cx.entity();
        let style = self.style.clone();
        let focus = self.focus.clone();
        let selecting = self.selecting;
        let prepaint = {
            let (view, style, focus) = (view.clone(), style.clone(), focus.clone());
            move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                let cell = cell_size(window, &style);
                let size = GridSize {
                    cols: ((bounds.size.width / cell.0) as u16).max(2),
                    rows: ((bounds.size.height / cell.1) as u16).max(1),
                };
                let focused = focus.is_focused(window);
                let frame = view.update(cx, |view, _| {
                    view.cell = Some(cell);
                    view.origin = bounds.origin;
                    view.resize(size);
                    let frame = view.frame(focused);
                    view.cursor_cell = frame
                        .as_ref()
                        .and_then(|f| f.cursor)
                        .map(|c| (c.row, c.col));
                    frame
                });
                (frame, cell)
            }
        };
        canvas(prepaint, move |bounds, (frame, cell), window, cx| {
            if let Some(frame) = frame {
                paint(bounds, &frame, cell, &style, window, cx);
            }
            window.handle_input(&focus, ElementInputHandler::new(bounds, view.clone()), cx);
            if selecting {
                let on_move = view.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && ev.dragging() {
                        on_move.update(cx, |view, cx| view.drag_to(ev.position, cx));
                    }
                });
                let on_up = view.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_up.update(cx, |view, cx| view.end_drag(cx));
                    }
                });
            }
        })
        .size_full()
    }
}

/// Forwards a pane's events to the view: a thread blocks on the pane's channel and the view
/// awaits an async one (a blocked executor thread per pane would starve GPUI's pool).
fn pump(events: Receiver<TermEvent>, generation: u64, cx: &mut Context<PaneView>) -> Task<()> {
    let (tx, mut rx) = unbounded();
    let _ = std::thread::Builder::new()
        .name("hydraui-events".into())
        .spawn(move || {
            while let Ok(e) = events.recv() {
                if tx.unbounded_send(e).is_err() {
                    break;
                }
            }
        });
    cx.spawn(async move |this, cx| {
        while let Some(e) = rx.next().await {
            if this
                .update(cx, |view, cx| view.on_term_event(generation, e, cx))
                .is_err()
            {
                break;
            }
        }
    })
}

/// One cell: the advance of "M" and 1.2 × its height, as the spike measured it.
fn cell_size(window: &Window, style: &TermStyle) -> (Pixels, Pixels) {
    let run = TextRun {
        len: 1,
        font: font(style.font.clone()),
        ..Default::default()
    };
    let m = window
        .text_system()
        .shape_line("M".into(), style.font_size, &[run], None);
    (m.width, (m.ascent + m.descent) * 1.2)
}

fn text_run(len: usize, s: &Style, base: &Font) -> TextRun {
    let mut f = base.clone();
    if s.bold {
        f = f.bold();
    }
    if s.italic {
        f = f.italic();
    }
    TextRun {
        len,
        font: f,
        color: color(s.fg),
        background_color: None,
        underline: s.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: None,
            wavy: false,
        }),
        strikethrough: s.strikeout.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    }
}

/// Backgrounds, selection, cursor, then text: one shaped line per row with a fixed advance,
/// plus wide and combined characters drawn at their own columns.
fn paint(
    bounds: Bounds<Pixels>,
    frame: &Frame,
    (cw, ch): (Pixels, Pixels),
    style: &TermStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let p = &style.palette;
    window.paint_quad(fill(bounds, color(p.background)));
    let at = |row: usize, col: usize| {
        point(
            bounds.origin.x + cw * col as f32,
            bounds.origin.y + ch * row as f32,
        )
    };
    let cells = |row: usize, start: usize, end: usize| {
        Bounds::new(at(row, start), size(cw * (end - start) as f32, ch))
    };
    for row in &frame.rows {
        for (s, c) in &row.backgrounds {
            window.paint_quad(fill(cells(s.row, s.start, s.end), color(*c)));
        }
    }
    for s in &frame.selection {
        window.paint_quad(fill(cells(s.row, s.start, s.end), color(p.selection)));
    }
    if let Some(c) = frame.cursor {
        let w = if c.wide { cw * 2.0 } else { cw };
        let b = Bounds::new(at(c.row, c.col), size(w, ch));
        let cc = color(p.cursor);
        let quad = match c.kind {
            CursorKind::Block => fill(b, cc.opacity(0.6)),
            CursorKind::HollowBlock => outline(b, cc, BorderStyle::default()),
            CursorKind::Underline => fill(
                Bounds::new(point(b.origin.x, b.origin.y + ch - px(2.)), size(w, px(2.))),
                cc,
            ),
            CursorKind::Beam => fill(Bounds::new(b.origin, size(px(2.), ch)), cc),
        };
        window.paint_quad(quad);
    }
    let base = font(style.font.clone());
    for (r, row) in frame.rows.iter().enumerate() {
        if !row.text.trim_end().is_empty() {
            let runs: Vec<TextRun> = row
                .runs
                .iter()
                .map(|run| text_run(run.len, &run.style, &base))
                .collect();
            let line = window.text_system().shape_line(
                row.text.clone().into(),
                style.font_size,
                &runs,
                Some(cw),
            );
            let _ = line.paint(at(r, 0), ch, TextAlign::Left, None, window, cx);
        }
        for g in &row.glyphs {
            let run = text_run(g.text.len(), &g.style, &base);
            let line =
                window
                    .text_system()
                    .shape_line(g.text.clone().into(), style.font_size, &[run], None);
            let _ = line.paint(at(r, g.col), ch, TextAlign::Left, None, window, cx);
        }
    }
}

fn message(text: String, color: Hsla) -> AnyElement {
    div()
        .size_full()
        .p_4()
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// Text typed into the pane, including IME results, goes straight to the shell. There is no
/// in-line composition preview: the IME's own window shows it until it is committed.
impl EntityInputHandler for PaneView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        None
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !text.is_empty() {
            self.send(text.as_bytes(), cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    /// The IME's candidate window goes at the cursor.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let ((cw, ch), (row, col)) = (self.cell?, self.cursor_cell?);
        Some(Bounds::new(
            point(
                element_bounds.origin.x + cw * col as f32,
                element_bounds.origin.y + ch * row as f32,
            ),
            size(cw, ch),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for PaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, warning, muted, border) = (
            theme.danger,
            theme.warning,
            theme.muted_foreground,
            theme.border,
        );
        let background = color(self.style.palette.background);
        let body = match &self.slot {
            Slot::Running(_) => self.grid(cx).into_any_element(),
            Slot::Starting => message(format!("starting {}…", self.request.env), muted),
            Slot::Exited(code) => message(
                match code {
                    Some(c) => format!("exited ({c}) - Enter to restart, Ctrl+W to close"),
                    None => "exited - Enter to restart, Ctrl+W to close".into(),
                },
                muted,
            ),
            Slot::Failed(e) => v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .child(div().text_color(danger).child(e.clone()))
                .child(
                    Button::new("retry")
                        .label("Retry")
                        .small()
                        .on_click(cx.listener(|view, _, _, cx| view.start(cx))),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(background)
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy_text))
            .on_action(cx.listener(Self::paste_text))
            .on_action(cx.listener(Self::find))
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .when(!self.warnings.is_empty(), |d| {
                d.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .text_color(warning)
                        .text_sm()
                        .child(div().flex_1().child(self.warnings.join(" · ")))
                        .child(
                            Button::new("dismiss")
                                .label("Dismiss")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.warnings.clear();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.finder.as_ref(), |d, f| {
                d.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .child(div().flex_1().child(Input::new(&f.input).small()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("Enter: up · Shift+Enter: down · Esc: close"),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_1()
                    .cursor_text()
                    .child(body),
            )
    }
}
```

- [ ] **Step 5: Run the test and build.** `cargo test -p hydraui pane_view` — expected: PASS (1 test). `cargo clippy -p hydraui --all-targets -- -D warnings` — expected: clean. GPUI's API moves between versions: where a call doesn't compile, check the signature in the pinned source (`~/.cargo/registry/src/*/gpui-pre-0.3.9`, `gpui-component-0.7.3`) and the notes file, make the smallest change that keeps the behaviour described in the doc comments, and record it as a ruling.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.lock crates/hydraui
git commit -m "feat: hydraui terminal pane view

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: `Workspace`, startup and the smoke check

**Files:**
- Modify: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`, `crates/hydraui/src/main.rs`
- Create: `crates/hydraui/src/workspace.rs`
- Test: none automated (GPUI views); the smoke checklist in Step 6

**Interfaces:**
- Consumes: everything above: `config::{load, ThemeChoice, UiConfig}`, `palette::Palette`, `keys::{keymap, bindings, NewTab, ClosePane, SplitRight, SplitDown, FocusLeft, FocusRight, FocusUp, FocusDown, NextTab, PreviousTab}`, `split::{Axis, Dir, PaneId, Tree}`, `tabs::{Tabs, TabId}`, `hydra::{Hydra, PaneRequest, StatusCache, STATUS_TTL, accounts_summary}`, `pane_view::{PaneView, PaneEvent, TermStyle}`; `hydraui_term::clear_pane_inits`.
- Produces: `hydraui.exe`; `workspace::Workspace::new(hydra: Arc<Hydra>, ui: UiConfig, style: TermStyle, problems: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self` (plan 4 adds sets and the launcher to it).

- [ ] **Step 1: Add the dependencies and module.** In `crates/hydraui/Cargo.toml` `[dependencies]`, add after `gpui.workspace = true`:

```toml
gpui_platform.workspace = true
gpui-kit-assets.workspace = true
```

In `crates/hydraui/src/lib.rs`, add `pub mod workspace;` after `pub mod tabs;`.

- [ ] **Step 2: Write `workspace.rs`.**

```rust
//! The window's content (spec §7.1, §7.5, §7.6): the tab strip, the active tab's split panes
//! with draggable dividers, the status bar, and the commands bound to keys.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, DispatchPhase, Entity, FocusHandle, Hsla,
    MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Subscription, Task,
    Window, canvas, div, px, relative, rgb, transparent_black,
};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, WindowExt, h_flex, v_flex};

use crate::config::UiConfig;
use crate::hydra::{Hydra, PaneRequest, STATUS_TTL, StatusCache, accounts_summary};
use crate::keys::{
    ClosePane, FocusDown, FocusLeft, FocusRight, FocusUp, NewTab, NextTab, PreviousTab,
    SplitDown, SplitRight,
};
use crate::pane_view::{PaneEvent, PaneView, TermStyle};
use crate::split::{Axis, Dir, PaneId, Tree};
use crate::tabs::{TabId, Tabs};

/// A divider being dragged: the split's path in the active tab's tree.
struct Drag {
    path: Vec<usize>,
    axis: Axis,
}

pub struct Workspace {
    hydra: Arc<Hydra>,
    ui: UiConfig,
    style: Arc<TermStyle>,
    tabs: Tabs,
    panes: HashMap<PaneId, Entity<PaneView>>,
    tints: HashMap<String, u32>,
    next_pane: PaneId,
    status: StatusCache,
    refreshing: HashSet<String>,
    drag: Option<Drag>,
    /// Each split's on-screen bounds at the last paint, by path, for divider drags.
    split_bounds: Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>,
    focus: FocusHandle,
    title: String,
    subs: HashMap<PaneId, Subscription>,
    _timer: Task<()>,
}

impl Workspace {
    pub fn new(
        hydra: Arc<Hydra>,
        ui: UiConfig,
        style: TermStyle,
        problems: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_TTL).await;
                if this.update(cx, |ws, cx| ws.refresh_status(cx)).is_err() {
                    break;
                }
            }
        });
        // After the window's Root exists: toasts and dialogs live in it.
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |ws, window, cx| ws.startup(problems, window, cx));
        })
        .detach();
        Self {
            hydra,
            ui,
            style: Arc::new(style),
            tabs: Tabs::default(),
            panes: HashMap::new(),
            tints: HashMap::new(),
            next_pane: 0,
            status: StatusCache::default(),
            refreshing: HashSet::new(),
            drag: None,
            split_bounds: Rc::default(),
            focus: cx.focus_handle(),
            title: String::new(),
            subs: HashMap::new(),
            _timer: timer,
        }
    }

    /// Shows what was wrong with the settings, then opens the first tab in the home folder.
    fn startup(&mut self, problems: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        let home = self.hydra.user_home.clone();
        self.new_tab_in(home, window, cx);
    }

    /// A Loose tab in `cwd`: the folder's environment, else `default_env`, else ask.
    fn new_tab_in(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match self.hydra.adhoc_env(&cwd, self.ui.default_env.as_deref()) {
            Some(env) => self.open_tab(env, cwd, window, cx),
            None => self.pick_env(cwd, window, cx),
        }
    }

    fn pick_env(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let envs = self.hydra.env_names();
        if envs.is_empty() {
            window.push_notification(
                Notification::warning(
                    "No hydra environments yet: create one with `hydra env new <name>`.",
                )
                .title("hydraui"),
                cx,
            );
            return;
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (choices, workspace, cwd) = (envs.clone(), workspace.clone(), cwd.clone());
            dialog.title("Open a tab in…").child(
                Command::new(&state)
                    .items(envs.iter().map(|e| CommandItem::new().label(e.clone())))
                    .placeholder("Environment")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(env) = choices.get(ix.row).cloned() {
                            window.close_dialog(cx);
                            workspace.update(cx, |ws, cx| ws.open_tab(env, cwd.clone(), window, cx));
                        }
                    }),
            )
        });
    }

    fn open_tab(&mut self, env: String, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let shell = self.hydra.shell(self.ui.default_shell.as_deref());
        let id = self.new_pane(
            PaneRequest {
                env,
                cwd,
                shell,
                command: None,
            },
            window,
            cx,
        );
        self.tabs.add(None, id);
        self.focus_soon(id, window, cx);
        cx.notify();
    }

    fn new_pane(&mut self, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        self.next_pane += 1;
        let id = self.next_pane;
        let tint = self.hydra.tint(&request.env);
        self.tints.insert(request.env.clone(), tint);
        let (hydra, style) = (self.hydra.clone(), self.style.clone());
        let view = cx.new(|cx| PaneView::new(id, request, hydra, style, window, cx));
        let sub = cx.subscribe_in(&view, window, move |ws, _, ev: &PaneEvent, _, cx| {
            if *ev == PaneEvent::Focused {
                ws.tabs.focus_pane(id);
                ws.refresh_status(cx);
            }
            cx.notify();
        });
        self.subs.insert(id, sub);
        self.panes.insert(id, view);
        id
    }

    fn focused(&self) -> Option<Entity<PaneView>> {
        self.tabs
            .active()
            .and_then(|t| self.panes.get(&t.focused))
            .cloned()
    }

    fn focus_pane(&self, id: PaneId, window: &mut Window, cx: &mut App) {
        match self.panes.get(&id) {
            Some(view) => {
                let handle = view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            None => window.focus(&self.focus, cx),
        }
    }

    /// Focuses now and again on the next frame, after a closing dialog has handed the focus
    /// back to where it was.
    fn focus_soon(&self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane(id, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |ws, window, cx| ws.focus_pane(id, window, cx));
        })
        .detach();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        match self.tabs.active() {
            Some(tab) => self.focus_pane(tab.focused, window, cx),
            None => window.focus(&self.focus, cx),
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self
            .focused()
            .map(|v| v.read(cx).cwd().clone())
            .unwrap_or_else(|| self.hydra.user_home.clone());
        self.new_tab_in(cwd, window, cx);
    }

    /// The new pane gets the focused pane's environment and current folder (spec §7.6).
    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.focused() else {
            return;
        };
        let request = {
            let v = from.read(cx);
            PaneRequest {
                env: v.env().to_string(),
                cwd: v.cwd().clone(),
                shell: v.request.shell,
                command: None,
            }
        };
        let id = self.new_pane(request, window, cx);
        self.tabs.split_focused(axis, id);
        self.focus_pane(id, window, cx);
        cx.notify();
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Row, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Column, window, cx);
    }

    fn drop_pane(&mut self, id: PaneId) {
        // Dropping the view drops its Pane: the job ends the shell's whole process tree.
        self.panes.remove(&id);
        self.subs.remove(&id);
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.active().map(|t| t.focused) else {
            return;
        };
        self.tabs.close_pane(id);
        self.drop_pane(id);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        for id in self.tabs.close_tab(tab) {
            self.drop_pane(id);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn move_focus(&mut self, dir: Dir, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.focus_dir(dir) {
            self.focus_pane(id, window, cx);
            cx.notify();
        }
    }

    fn focus_left(&mut self, _: &FocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Left, window, cx);
    }

    fn focus_right(&mut self, _: &FocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Right, window, cx);
    }

    fn focus_up(&mut self, _: &FocusUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Up, window, cx);
    }

    fn focus_down(&mut self, _: &FocusDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(Dir::Down, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.next();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.previous();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Checks the focused environment's accounts off the UI thread, at most once a minute.
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let Some(env) = self.focused().map(|v| v.read(cx).env().to_string()) else {
            return;
        };
        if self.status.is_fresh(&env, Instant::now()) || !self.refreshing.insert(env.clone()) {
            return;
        }
        let hydra = self.hydra.clone();
        cx.spawn(async move |this, cx| {
            let e = env.clone();
            let summary = cx
                .background_spawn(async move { accounts_summary(&hydra.whoami(&e)) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                ws.refreshing.remove(&env);
                ws.status.store(env, summary, Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    fn tint(&self, env: &str) -> Hsla {
        rgb(self.tints.get(env).copied().unwrap_or(0x808080)).into()
    }

    fn drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(b) = self.split_bounds.borrow().get(&drag.path).copied() else {
            return;
        };
        let ratio = match drag.axis {
            Axis::Row => (pos.x - b.origin.x) / b.size.width,
            Axis::Column => (pos.y - b.origin.y) / b.size.height,
        };
        let path = drag.path.clone();
        if let Some(tab) = self.tabs.active_mut() {
            tab.tree.set_ratio(&path, ratio);
        }
        cx.notify();
    }

    /// Window-wide move/up listeners while a divider is dragged, so the drag keeps going
    /// when the pointer leaves the divider.
    fn drag_listener(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ws = cx.entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let on_move = ws.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_move.update(cx, |ws, cx| ws.drag_to(ev.position, cx));
                    }
                });
                let on_up = ws.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_up.update(cx, |ws, cx| {
                            ws.drag = None;
                            cx.notify();
                        });
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn render_tree(
        &self,
        tree: &Tree,
        path: Vec<usize>,
        focused: PaneId,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match tree {
            Tree::Leaf(id) => {
                let Some(view) = self.panes.get(id) else {
                    return div().into_any_element();
                };
                let edge = if *id == focused {
                    self.tint(view.read(cx).env())
                } else {
                    transparent_black()
                };
                div()
                    .size_full()
                    .border_1()
                    .border_color(edge)
                    .child(view.clone())
                    .into_any_element()
            }
            Tree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let axis = *axis;
                let row = axis == Axis::Row;
                let child_path = |i: usize| {
                    let mut p = path.clone();
                    p.push(i);
                    p
                };
                let store = self.split_bounds.clone();
                let key = path.clone();
                let drag_path = path.clone();
                let divider = div()
                    .id(SharedString::from(format!("divider-{path:?}")))
                    .flex_none()
                    .bg(border)
                    .map(|d| {
                        if row {
                            d.w(px(4.)).h_full().cursor(CursorStyle::ResizeLeftRight)
                        } else {
                            d.h(px(4.)).w_full().cursor(CursorStyle::ResizeUpDown)
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| {
                            ws.drag = Some(Drag {
                                path: drag_path.clone(),
                                axis,
                            });
                            cx.notify();
                        }),
                    );
                let one = div()
                    .flex_none()
                    .overflow_hidden()
                    .map(|d| {
                        if row {
                            d.h_full().w(relative(*ratio))
                        } else {
                            d.w_full().h(relative(*ratio))
                        }
                    })
                    .child(self.render_tree(first, child_path(0), focused, border, cx));
                let two = div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_tree(second, child_path(1), focused, border, cx));
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .map(|d| if row { d.flex_row() } else { d.flex_col() })
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                store.borrow_mut().insert(key, bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .child(one)
                    .child(divider)
                    .child(two)
                    .into_any_element()
            }
        }
    }

    fn render_tab_strip(&self, bg: Hsla, border: Hsla, muted: Hsla, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.tabs.active_id();
        let mut strip = h_flex()
            .h(px(34.))
            .px_1()
            .gap_1()
            .bg(bg)
            .border_b_1()
            .border_color(border);
        for (set, ids) in self.tabs.groups() {
            strip = strip.child(
                div()
                    .px_2()
                    .text_sm()
                    .text_color(muted)
                    .child(set.unwrap_or_else(|| "Loose".into())),
            );
            for id in ids {
                let Some(tab) = self.tabs.get(id) else {
                    continue;
                };
                let (label, env) = self
                    .panes
                    .get(&tab.focused)
                    .map(|v| {
                        let v = v.read(cx);
                        (v.label(), v.env().to_string())
                    })
                    .unwrap_or_default();
                let tint = self.tint(&env);
                strip = strip.child(
                    h_flex()
                        .id(("tab", id))
                        .px_2()
                        .py_1()
                        .gap_2()
                        .rounded_md()
                        .when(Some(id) == active, |d| d.bg(tint.opacity(0.25)))
                        .hover(|d| d.bg(tint.opacity(0.15)))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.tabs.activate(id);
                            ws.focus_active(window, cx);
                            cx.notify();
                        }))
                        .child(div().size(px(8.)).rounded_full().bg(tint))
                        .child(div().text_sm().child(label))
                        .child(
                            div()
                                .id(("close", id))
                                .px_1()
                                .text_color(muted)
                                .hover(|d| d.text_color(tint))
                                .child("×")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    cx.stop_propagation();
                                    ws.close_tab(id, window, cx);
                                })),
                        ),
                );
            }
        }
        strip.child(
            div()
                .id("new-tab")
                .px_2()
                .text_color(muted)
                .child("+")
                .on_click(cx.listener(|ws, _, window, cx| ws.new_tab(&NewTab, window, cx))),
        )
    }

    fn render_status(&self, border: Hsla, muted: Hsla, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut bar = h_flex()
            .h(px(26.))
            .px_2()
            .gap_3()
            .text_sm()
            .border_t_1()
            .border_color(border)
            .text_color(muted);
        if let Some(view) = self.focused() {
            let v = view.read(cx);
            let env = v.env().to_string();
            let accounts = self.status.get(&env).unwrap_or("…").to_string();
            bar = bar
                .child(div().text_color(self.tint(&env)).child(env))
                .child(div().flex_1().truncate().child(accounts))
                .child(div().child(v.cwd().display().to_string()));
        }
        bar
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.split_bounds.borrow_mut().clear();
        let title = match self.focused() {
            Some(v) => format!("hydraui - {}", v.read(cx).label()),
            None => "hydraui".to_string(),
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let theme = cx.theme();
        let (bg, fg, muted, border, bar) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.tab_bar,
        );
        let body = match self.tabs.active() {
            Some(tab) => self.render_tree(&tab.tree, Vec::new(), tab.focused, border, cx),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child("No tabs open. Ctrl+T opens one.")
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .on_action(cx.listener(Self::focus_up))
            .on_action(cx.listener(Self::focus_down))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .child(self.render_tab_strip(bar, border, muted, cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status(border, muted, cx))
            .when(self.drag.is_some(), |d| d.child(self.drag_listener(cx)))
    }
}
```

- [ ] **Step 3: Write `main.rs`.** Replace `crates/hydraui/src/main.rs` with:

```rust
//! hydraui.exe: a desktop terminal for hydra environments.
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, Theme, ThemeMode};
use hydraui::config::{self, ThemeChoice};
use hydraui::hydra::Hydra;
use hydraui::keys;
use hydraui::palette::Palette;
use hydraui::pane_view::TermStyle;
use hydraui::workspace::Workspace;

fn main() {
    let (hydra, hydra_problem) = match Hydra::from_env() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hydraui: {e}");
            std::process::exit(1);
        }
    };
    let (ui, ui_problem) = config::load(&hydra.paths);
    let (palette, colour_problems) = Palette::from_config(&ui.colors);
    let (keymap, key_problems) = keys::keymap(&ui.keys);
    let problems: Vec<String> = hydra_problem
        .into_iter()
        .chain(ui_problem)
        .chain(colour_problems)
        .chain(key_problems)
        .collect();
    // Init scripts left by earlier runs (spec §4.5).
    hydraui_term::clear_pane_inits(&hydra.paths);
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let mode = match ui.theme {
                ThemeChoice::Dark => ThemeMode::Dark,
                ThemeChoice::Light => ThemeMode::Light,
            };
            Theme::change(mode, None, cx);
            cx.bind_keys(keys::bindings(&keymap));
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let style = TermStyle {
                font: ui.font.clone().into(),
                font_size: px(ui.font_size),
                palette,
            };
            let bounds = Bounds::centered(None, size(px(1200.), px(760.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("hydraui".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(480.), px(320.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                let workspace =
                    cx.new(|cx| Workspace::new(Arc::new(hydra), ui, style, problems, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("hydraui can't open its window");
            cx.activate(true);
        });
}
```

- [ ] **Step 4: Build and gate.** `cargo build -p hydraui --release`, then `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` — expected: all pass. As in Task 9, fix GPUI signature mismatches against the pinned source with the smallest change that keeps the documented behaviour, and record each as a ruling.

- [ ] **Step 5: Start it.** `cargo run -p hydraui --release` (with a real `~/.hydra` that has at least one environment). Expected: a dark window titled "hydraui - <folder>", one Loose tab in your home folder (or the environment picker when home isn't bound and `default_env` isn't set), and a pwsh prompt tagged `[<env>]`. If you can capture the screen, take a screenshot as evidence; otherwise say the hand check is still to do.

- [ ] **Step 6: Smoke checklist** (spec §9: rendering is checked by hand). Run each item and note pass/fail in the commit message body or the ledger:
  1. Typing, Backspace, arrows, Home/End, Ctrl+C (stops `ping -t localhost`), Enter, Tab completion. `é` (and an AltGr character if your keyboard has one) types correctly.
  2. `Get-ChildItem C:\Windows`: colours right; the wheel scrolls back through history; typing jumps back to the bottom.
  3. Ctrl+Shift+D and Ctrl+Shift+E split; each new pane is in the same environment and folder (`$env:HYDRA_ENV`, `Get-Location`); Alt+arrows move the focus and the tinted border follows; dragging a divider resizes both panes and the shells follow (`$Host.UI.RawUI.WindowSize`).
  4. Ctrl+T opens a tab in the focused pane's folder; Ctrl+Tab / Ctrl+Shift+Tab and clicking switch tabs; the tab's `×` closes it and ends its processes (`ping -t localhost` disappears from Task Manager).
  5. Mouse selection: drag, double-click a word, triple-click a line; Ctrl+Shift+C copies; Ctrl+Shift+V pastes; a multi-line paste into pwsh arrives as one block (bracketed paste), not line by line.
  6. Ctrl+F: type text, Enter walks up through matches (Shift+Enter down), Esc closes the box and typing goes to the shell again.
  7. In a Git Bash pane (set `default_shell = "bash"` or split from one), `printf '\e]8;;https://example.com\e\\link\e]8;;\e\\\n'`; Ctrl+click on "link" opens the browser.
  8. `less /etc/profile` in Git Bash: the wheel scrolls inside less; `vim` with `:set mouse=a` takes clicks.
  9. `exit 3` → "exited (3) - Enter to restart, Ctrl+W to close"; Enter starts a fresh shell (edit env.toml first to see it re-prepared).
  10. Break an environment (e.g. `[git]` `ssh_key = "C:/missing"` in its env.toml) and open a tab in it: the banner shows hydra's error and Retry; fix the file and Retry starts the shell.
  11. The status bar shows the environment (tinted), its accounts (after a moment), and the folder, which follows `cd`.
  12. Put `font_size = 200` in `~/.hydra/hydraui/config.toml` and restart: a toast names the problem and the defaults apply. Remove it again.
  13. Close the last tab: the window shows "No tabs open. Ctrl+T opens one."; Ctrl+T works.
  14. Resize the window: the panes and shells follow; nothing is garbled.

- [ ] **Step 7: Commit.**

```bash
git add Cargo.lock crates/hydraui
git commit -m "feat: hydraui window with tabs, split panes and status bar

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```
