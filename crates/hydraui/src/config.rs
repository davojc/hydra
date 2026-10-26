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

/// The terminal font when config.toml names none: a monospace font the platform ships.
pub const DEFAULT_FONT: &str = if cfg!(windows) {
    "Cascadia Mono"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

/// The font the terminal draws with: `configured` when it is installed, else
/// `DEFAULT_FONT`. A missing font would fall back to a proportional one, whose letters sit
/// unevenly on the fixed grid. Windows keeps the configured font as it always has.
pub fn terminal_font(configured: &str, installed: &[String]) -> String {
    if cfg!(windows) || installed.iter().any(|f| f.eq_ignore_ascii_case(configured)) {
        configured.to_string()
    } else {
        DEFAULT_FONT.to_string()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiConfig {
    pub font: String,
    pub font_size: f32,
    pub theme: ThemeChoice,
    /// The environment for a new tab in an unbound folder (spec §7.6).
    pub default_env: Option<String>,
    /// `pwsh`, `bash` or `zsh`; else hydra's `default_shell`, else the platform's (zsh on macOS).
    pub default_shell: Option<String>,
    /// Opens files from the file tree (plan 4); read now so the file format is stable.
    pub editor: Option<String>,
    pub colors: ColorsConfig,
    /// Command name → keys, e.g. `new_terminal = "ctrl-alt-d"`; `""` unbinds.
    pub keys: BTreeMap<String, String>,
    /// The share of the seven-day Claude limit, in percent, a pipeline stage needs left
    /// (orchestrator spec §5.1).
    pub min_week_left: u8,
    /// The team's Canopy, which finishes **Connect Slack**'s sign-in (it holds the Slack app's
    /// secret) and hands each person's own token back, e.g. `https://canopy.dev.iov42.net`.
    pub slack_connect_url: Option<String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            font: DEFAULT_FONT.into(),
            font_size: 14.0,
            theme: ThemeChoice::Dark,
            default_env: None,
            default_shell: None,
            editor: None,
            colors: ColorsConfig::default(),
            keys: BTreeMap::new(),
            min_week_left: crate::runs::MIN_WEEK_LEFT,
            slack_connect_url: None,
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
                    return Err(format!("{s:?}: use pwsh, bash or zsh"));
                }
                cfg.default_shell = Some(s);
                Ok(())
            }),
            "editor" => get(value).map(|e| cfg.editor = Some(e)),
            "keys" => get(value).map(|k| cfg.keys = k),
            "min_week_left" => get::<i64>(value).and_then(|n| {
                let n = u8::try_from(n)
                    .ok()
                    .filter(|n| *n <= 100)
                    .ok_or(format!("{n} must be between 0 and 100"))?;
                cfg.min_week_left = n;
                Ok(())
            }),
            "slack_connect_url" => get::<String>(value).and_then(|u| {
                let u = u.trim().trim_end_matches('/').to_string();
                if !u.starts_with("https://") {
                    return Err(format!("{u:?} must be an https:// address"));
                }
                cfg.slack_connect_url = Some(u);
                Ok(())
            }),
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

/// `text` with top-level setting `key` set to `value`, or removed with `None`, keeping
/// comments and every other setting. Refused when the result has a problem with `key`.
pub fn with_setting(
    text: &str,
    key: &str,
    value: Option<toml_edit::Value>,
) -> Result<String, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    match value {
        Some(v) => match doc.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
            Some(old) => {
                let decor = old.decor().clone();
                *old = v;
                *old.decor_mut() = decor;
            }
            None => {
                doc.insert(key, toml_edit::Item::Value(v));
            }
        },
        None => {
            doc.remove(key);
        }
    }
    let out = doc.to_string();
    let (_, problems) = parse(&out);
    match problems
        .into_iter()
        .find(|p| p.starts_with(&format!("{key}:")))
    {
        Some(p) => Err(p),
        None => Ok(out),
    }
}

/// Writes one setting to `paths`' config.toml (made if missing), as `with_setting` does.
pub fn set(paths: &HydraPaths, key: &str, value: Option<toml_edit::Value>) -> Result<(), String> {
    let path = config_file(paths);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("can't read {}: {e}", path.display())),
    };
    let out = with_setting(&text, key, value)?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, out).map_err(|e| format!("can't write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_changes_in_place_and_a_bad_one_is_refused() {
        let src = "# mine\ntheme = \"dark\" # look\nfont_size = 13\n\n[keys]\nnew_terminal = \"ctrl-t\"\n";
        let out = with_setting(src, "theme", Some("light".into())).unwrap();
        assert_eq!(out, src.replace("\"dark\"", "\"light\""));
        let size = with_setting(src, "font_size", Some(15.0.into())).unwrap();
        assert_eq!(clean(&size).font_size, 15.0);
        let editor = with_setting(src, "editor", Some("code".into())).unwrap();
        assert_eq!(clean(&editor).editor.as_deref(), Some("code"));
        assert!(editor.contains("[keys]"));
        assert!(!with_setting(src, "theme", None).unwrap().contains("theme"));
        assert!(with_setting(src, "font_size", Some(200.0.into())).is_err());
        assert!(with_setting("", "default_shell", Some("pwsh".into())).is_ok());
    }

    fn clean(text: &str) -> UiConfig {
        let (cfg, problems) = parse(text);
        assert!(problems.is_empty(), "{problems:?}");
        cfg
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(clean(""), UiConfig::default());
        let font = if cfg!(windows) {
            "Cascadia Mono"
        } else if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "DejaVu Sans Mono"
        };
        assert_eq!(UiConfig::default().font, font);
        assert_eq!(DEFAULT_FONT, font);
        assert_eq!(UiConfig::default().font_size, 14.0);
    }

    /// A font that isn't installed would fall back to a proportional font, whose letters sit
    /// unevenly on the terminal's fixed grid: use the monospace default instead (not on
    /// Windows, where the configured font is used as it always was).
    #[test]
    fn a_terminal_font_that_isnt_installed_falls_back_to_the_monospace_default() {
        let installed = ["Menlo".to_string(), "JetBrains Mono".to_string()];
        assert_eq!(
            terminal_font("JetBrains Mono", &installed),
            "JetBrains Mono"
        );
        assert_eq!(
            terminal_font("jetbrains mono", &installed),
            "jetbrains mono"
        );
        let missing = terminal_font("Cascadia Mono", &installed);
        if cfg!(windows) {
            assert_eq!(missing, "Cascadia Mono");
        } else {
            assert_eq!(missing, DEFAULT_FONT);
        }
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
min_week_left = 25
slack_connect_url = "https://canopy.dev.iov42.net/"

[colors]
background = "#101010"
palette = ["#000000", "#111111"]

[keys]
new_terminal = "ctrl-alt-d"
"##,
        );
        assert_eq!(cfg.font, "JetBrains Mono");
        assert_eq!(cfg.font_size, 13.0);
        assert_eq!(cfg.theme, ThemeChoice::Light);
        assert_eq!(cfg.default_env.as_deref(), Some("work"));
        assert_eq!(cfg.default_shell.as_deref(), Some("bash"));
        assert_eq!(cfg.editor.as_deref(), Some("code"));
        assert_eq!(cfg.min_week_left, 25);
        assert_eq!(
            cfg.slack_connect_url.as_deref(),
            Some("https://canopy.dev.iov42.net")
        );
        assert_eq!(cfg.colors.background.as_deref(), Some("#101010"));
        assert_eq!(cfg.colors.palette.as_ref().map(Vec::len), Some(2));
        assert_eq!(cfg.keys["new_terminal"], "ctrl-alt-d");
    }

    #[test]
    fn slack_connect_needs_https() {
        let (cfg, problems) = parse("slack_connect_url = \"http://canopy\"\n");
        assert_eq!(cfg.slack_connect_url, None);
        assert!(problems[0].contains("https"), "{problems:?}");
    }

    #[test]
    fn notepad_bom_and_crlf_are_fine() {
        assert_eq!(clean("\u{feff}font_size = 12\r\n").font_size, 12.0);
    }

    #[test]
    fn one_bad_setting_keeps_the_rest() {
        let (cfg, problems) = parse(
            "font = \"Fira Code\"\nfontsize = 12\nfont_size = 200\ntheme = \"blue\"\n\
             default_shell = \"fish\"\ndefault_env = \"work\"\nmin_week_left = 101\n\
             [colors]\nforground = \"#000000\"\nbackground = \"#101010\"\n",
        );
        assert_eq!(cfg.font, "Fira Code");
        assert_eq!(cfg.default_env.as_deref(), Some("work"));
        assert_eq!(cfg.colors.background.as_deref(), Some("#101010"));
        assert_eq!(cfg.font_size, 14.0);
        assert_eq!(cfg.theme, ThemeChoice::Dark);
        assert_eq!(cfg.default_shell, None);
        assert_eq!(cfg.min_week_left, crate::runs::MIN_WEEK_LEFT);
        assert_eq!(problems.len(), 6, "{problems:?}");
        for word in [
            "fontsize",
            "between 6 and 72",
            "theme",
            "fish",
            "forground",
            "between 0 and 100",
        ] {
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
