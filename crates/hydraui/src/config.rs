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
    let cfg: UiConfig =
        toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).map_err(|e| e.to_string())?;
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
            Err(e) => (
                UiConfig::default(),
                Some(format!("{}: {e}", path.display())),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (UiConfig::default(), None),
        Err(e) => (
            UiConfig::default(),
            Some(format!("can't read {}: {e}", path.display())),
        ),
    }
}

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
        assert!(
            parse("default_shell = \"fish\"")
                .unwrap_err()
                .contains("fish")
        );
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
