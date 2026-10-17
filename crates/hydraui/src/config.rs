//! `<HYDRA_HOME>/hydraui/config.toml`: hydraui's own settings (spec §3.1). Every key is
//! optional. Each setting is checked on its own: a bad or unknown one is reported and keeps
//! its default, and the rest still apply, so a typo never costs the whole file.
use std::collections::BTreeMap;
use std::path::PathBuf;

use hydra_core::paths::HydraPaths;
use hydra_platform::shell::ShellKind;
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// The look of hydraui's own controls (tabs, dialogs). The terminal's colours are `[colors]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
}

/// `[colors]`: `#rrggbb` values over the built-in scheme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColorsConfig {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub selection: Option<String>,
    /// The 16 ANSI colours, black to bright white.
    pub palette: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
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

fn get<T: DeserializeOwned>(value: toml::Value) -> Result<T, String> {
    value
        .try_into()
        .map_err(|e: toml::de::Error| e.to_string().trim().to_string())
}

/// Parses config text (dropping a UTF-8 BOM): the settings, plus one problem per setting that
/// was wrong. Text that isn't TOML at all gives the defaults and the parse error.
pub fn parse(text: &str) -> (UiConfig, Vec<String>) {
    let mut cfg = UiConfig::default();
    let mut problems = Vec::new();
    let table: toml::Table = match toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)) {
        Ok(t) => t,
        Err(e) => return (cfg, vec![e.to_string()]),
    };
    for (key, value) in table {
        let result = match key.as_str() {
            "font" => get::<String>(value).and_then(|f| {
                if f.trim().is_empty() {
                    return Err("can't be empty".into());
                }
                cfg.font = f;
                Ok(())
            }),
            "font_size" => get::<f32>(value).and_then(|s| {
                if !(6.0..=72.0).contains(&s) {
                    return Err(format!("{s} must be between 6 and 72"));
                }
                cfg.font_size = s;
                Ok(())
            }),
            "theme" => get(value).map(|t| cfg.theme = t),
            "default_env" => get(value).map(|e| cfg.default_env = Some(e)),
            "default_shell" => get::<String>(value).and_then(|s| {
                if ShellKind::parse(&s).is_none() {
                    return Err(format!("{s:?}: use pwsh or bash"));
                }
                cfg.default_shell = Some(s);
                Ok(())
            }),
            "editor" => get(value).map(|e| cfg.editor = Some(e)),
            "keys" => get(value).map(|k| cfg.keys = k),
            "colors" => match value {
                toml::Value::Table(colors) => {
                    parse_colors(colors, &mut cfg.colors, &mut problems);
                    Ok(())
                }
                _ => Err("must be a table".into()),
            },
            _ => Err("unknown setting".into()),
        };
        if let Err(e) = result {
            problems.push(format!("{key}: {e}"));
        }
    }
    (cfg, problems)
}

fn parse_colors(table: toml::Table, colors: &mut ColorsConfig, problems: &mut Vec<String>) {
    for (key, value) in table {
        let result = match key.as_str() {
            "foreground" => get(value).map(|c| colors.foreground = Some(c)),
            "background" => get(value).map(|c| colors.background = Some(c)),
            "cursor" => get(value).map(|c| colors.cursor = Some(c)),
            "selection" => get(value).map(|c| colors.selection = Some(c)),
            "palette" => get(value).map(|p| colors.palette = Some(p)),
            _ => Err("unknown setting".into()),
        };
        if let Err(e) = result {
            problems.push(format!("colors.{key}: {e}"));
        }
    }
}

/// The settings, plus what was wrong with the file, each naming it.
pub fn load(paths: &HydraPaths) -> (UiConfig, Vec<String>) {
    let path = config_file(paths);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, problems) = parse(&text);
            let named = problems
                .into_iter()
                .map(|p| format!("{}: {p}", path.display()))
                .collect();
            (cfg, named)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (UiConfig::default(), Vec::new()),
        Err(e) => (
            UiConfig::default(),
            vec![format!("can't read {}: {e}", path.display())],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> UiConfig {
        let (cfg, problems) = parse(text);
        assert!(problems.is_empty(), "{problems:?}");
        cfg
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(clean(""), UiConfig::default());
        assert_eq!(UiConfig::default().font, "Cascadia Mono");
        assert_eq!(UiConfig::default().font_size, 14.0);
    }

    #[test]
    fn reads_every_setting() {
        let cfg = clean(
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
        );
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
        assert_eq!(clean("\u{feff}font_size = 12\r\n").font_size, 12.0);
    }

    #[test]
    fn one_bad_setting_keeps_the_rest() {
        let (cfg, problems) = parse(
            "font = \"Fira Code\"\nfontsize = 12\nfont_size = 200\ntheme = \"blue\"\n\
             default_shell = \"fish\"\ndefault_env = \"work\"\n\
             [colors]\nforground = \"#000000\"\nbackground = \"#101010\"\n",
        );
        assert_eq!(cfg.font, "Fira Code");
        assert_eq!(cfg.default_env.as_deref(), Some("work"));
        assert_eq!(cfg.colors.background.as_deref(), Some("#101010"));
        assert_eq!(cfg.font_size, 14.0);
        assert_eq!(cfg.theme, ThemeChoice::Dark);
        assert_eq!(cfg.default_shell, None);
        assert_eq!(problems.len(), 5, "{problems:?}");
        for word in ["fontsize", "between 6 and 72", "theme", "fish", "forground"] {
            assert!(
                problems.iter().any(|p| p.contains(word)),
                "{word}: {problems:?}"
            );
        }
        assert!(parse("font = \" \"").1[0].contains("font"));
    }

    #[test]
    fn a_file_that_isnt_toml_is_the_defaults_and_one_problem() {
        let (cfg, problems) = parse("font_size = 14\nfont = \"x\n");
        assert_eq!(cfg, UiConfig::default());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("line 2"), "{problems:?}");
    }

    #[test]
    fn load_names_the_file_in_each_problem() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert_eq!(load(&paths), (UiConfig::default(), Vec::new()));
        std::fs::create_dir_all(config_file(&paths).parent().unwrap()).unwrap();
        std::fs::write(
            config_file(&paths),
            "font_size = \"big\"\nfont = \"Fira Code\"\n",
        )
        .unwrap();
        let (cfg, problems) = load(&paths);
        assert_eq!(cfg.font, "Fira Code");
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].contains("config.toml") && problems[0].contains("font_size"),
            "{problems:?}"
        );
    }
}
