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
    /// The phone link is on.
    pub phone_access: bool,
    /// The paired phone's endpoint id.
    pub phone: Option<String>,
    /// Your name, as the people in your swarms see it. Swarms don't start until it is set.
    pub peer_name: Option<String>,
    /// This machine's name in your swarms; the host name when unset.
    pub machine_name: Option<String>,
    /// The share of the seven-day Claude limit, in percent, a pipeline stage needs left
    /// (orchestrator spec §5.1).
    pub min_week_left: u8,
    /// The team's Canopy, which finishes **Connect Slack**'s sign-in (it holds the Slack app's
    /// secret) and hands each person's own token back, e.g. `https://canopy.dev.iov42.net`.
    pub slack_connect_url: Option<String>,
    /// The team's Desktop-type Google OAuth client, for **Connect Gmail** (email triage spec
    /// §3). A desktop client's secret isn't secret by Google's own account of it, so it sits
    /// here; PKCE protects the sign-in.
    pub gmail_client_id: Option<String>,
    pub gmail_client_secret: Option<String>,
    /// Environments whose set-up checklist was hidden from the sidebar (spec §48).
    pub setup_hidden: Vec<String>,
    /// Claude plugins the team expects in every environment, listed on the set-up checklist
    /// (spec §48).
    pub setup_plugins: Vec<SetupPlugin>,
    /// The Pipelines tab shows only what's running, one line per ticket (spec §49).
    pub pipelines_condensed: bool,
    /// When History was last opened, as a run-log time (core spec §50).
    pub history_seen: Option<String>,
    /// The Linear teams and states the Tickets page opens on (core spec §53).
    pub tickets_teams: Vec<String>,
    pub tickets_states: Vec<String>,
    /// `anyone`, `unassigned` or `assigned`; with `assigned`, `tickets_people` are the Linear
    /// user ids to narrow to (none: anyone assigned).
    pub tickets_assignee: String,
    pub tickets_people: Vec<String>,
    /// Labels the Tickets page shows on a row when the ticket has them.
    pub tickets_labels: Vec<String>,
    /// Hours an orchestrator's runs may sit with nothing changing before it is parked to resume
    /// later (core spec §54); 0 never parks.
    pub park_idle_hours: u32,
}

/// A Claude plugin the set-up checklist asks for, e.g. the team's skill router.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupPlugin {
    /// `name@marketplace`, as `claude plugin install` takes it.
    pub plugin: String,
    /// The marketplace's source for `claude plugin marketplace add`, such as `owner/repo`;
    /// left out when the marketplace is one everyone already has.
    pub marketplace: Option<String>,
    /// What the checklist calls it; the plugin's name when left out.
    pub label: Option<String>,
}

impl UiConfig {
    /// The Google OAuth client for **Connect Gmail**, once both its id and secret are set.
    pub fn gmail_client(&self) -> Option<crate::gmail::Client> {
        Some(crate::gmail::Client {
            id: self.gmail_client_id.clone()?,
            secret: self.gmail_client_secret.clone().filter(|s| !s.is_empty())?,
        })
    }
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
            phone_access: false,
            phone: None,
            peer_name: None,
            machine_name: None,
            min_week_left: crate::runs::MIN_WEEK_LEFT,
            slack_connect_url: None,
            gmail_client_id: None,
            gmail_client_secret: None,
            setup_hidden: Vec::new(),
            setup_plugins: Vec::new(),
            pipelines_condensed: false,
            history_seen: None,
            tickets_teams: Vec::new(),
            tickets_states: Vec::new(),
            tickets_assignee: "anyone".to_string(),
            tickets_people: Vec::new(),
            tickets_labels: Vec::new(),
            park_idle_hours: crate::run_park::DEFAULT_HOURS,
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
            "phone_access" => get(value).map(|b| cfg.phone_access = b),
            "phone" => get(value).map(|p| cfg.phone = Some(p)),
            "peer_name" => get(value).map(|n| cfg.peer_name = Some(n)),
            "machine_name" => get(value).map(|n| cfg.machine_name = Some(n)),
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
            "gmail_client_id" => get::<String>(value).and_then(|id| {
                let id = id.trim().to_string();
                if !id.ends_with(".apps.googleusercontent.com") {
                    return Err(format!(
                        "{id:?} must be a Google client id ending .apps.googleusercontent.com"
                    ));
                }
                cfg.gmail_client_id = Some(id);
                Ok(())
            }),
            "gmail_client_secret" => {
                get::<String>(value).map(|s| cfg.gmail_client_secret = Some(s.trim().to_string()))
            }
            "setup_hidden" => get(value).map(|e| cfg.setup_hidden = e),
            "pipelines_condensed" => get(value).map(|c| cfg.pipelines_condensed = c),
            "history_seen" => get(value).map(|h| cfg.history_seen = Some(h)),
            "tickets_teams" => get(value).map(|t| cfg.tickets_teams = t),
            "tickets_states" => get(value).map(|s| cfg.tickets_states = s),
            "tickets_assignee" => get::<String>(value).and_then(|a| match a.as_str() {
                "anyone" | "unassigned" | "assigned" => {
                    cfg.tickets_assignee = a;
                    Ok(())
                }
                _ => Err(format!("{a:?} isn't anyone, unassigned or assigned")),
            }),
            "tickets_people" => get(value).map(|p| cfg.tickets_people = p),
            "tickets_labels" => get(value).map(|l| cfg.tickets_labels = l),
            "park_idle_hours" => get::<i64>(value).and_then(|n| {
                cfg.park_idle_hours = u32::try_from(n)
                    .ok()
                    .filter(|n| *n <= 168)
                    .ok_or(format!("{n} must be between 0 and 168 hours"))?;
                Ok(())
            }),
            "setup_plugins" => get::<Vec<SetupPlugin>>(value).and_then(|p| {
                match p.iter().find(|p| !p.plugin.contains('@')) {
                    Some(bad) => Err(format!(
                        "{:?} isn't name@marketplace, as claude plugin install takes it",
                        bad.plugin
                    )),
                    None => {
                        cfg.setup_plugins = p;
                        Ok(())
                    }
                }
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
    // Written beside it, then moved over it: another window reading the settings (the phone
    // link reads them each time it tries to start) never sees a half-written file.
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, &out).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&path, out).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    }
    Ok(())
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
    fn the_swarm_names_are_set_and_read() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert_eq!(load(&paths).0.peer_name, None);
        set(&paths, "peer_name", Some("Davo".into())).unwrap();
        set(&paths, "machine_name", Some("Davo's desktop".into())).unwrap();
        let (cfg, problems) = load(&paths);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(cfg.peer_name.as_deref(), Some("Davo"));
        assert_eq!(cfg.machine_name.as_deref(), Some("Davo's desktop"));
        assert!(set(&paths, "peer_name", Some(3.into())).is_err());
    }

    #[test]
    fn config_reads_phone_settings() {
        let cfg = clean(
            "phone_access = true
phone = \"abc\"",
        );
        assert!(cfg.phone_access);
        assert_eq!(cfg.phone.as_deref(), Some("abc"));
        let cfg = clean("");
        assert!(!cfg.phone_access);
        assert_eq!(cfg.phone, None);
        let (_, problems) = parse("phone_access = \"yes\"");
        assert!(problems.iter().any(|p| p.starts_with("phone_access:")));
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
gmail_client_id = "123-abc.apps.googleusercontent.com"
gmail_client_secret = "GOCSPX-test"
setup_hidden = ["work"]
pipelines_condensed = true
history_seen = "2026-11-10 09:00:00.000"
tickets_teams = ["PRE", "INTH"]
tickets_states = ["Todo", "In Review"]
tickets_assignee = "assigned"
tickets_people = ["u2"]
tickets_labels = ["Bug"]
park_idle_hours = 6
setup_plugins = [{ plugin = "skill-router@iov42", marketplace = "iov42/iov42-marketplace", label = "Team skills" }]

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
        assert_eq!(cfg.setup_hidden, vec!["work".to_string()]);
        assert!(cfg.pipelines_condensed);
        assert_eq!(cfg.history_seen.as_deref(), Some("2026-11-10 09:00:00.000"));
        assert_eq!(cfg.tickets_teams, vec!["PRE", "INTH"]);
        assert_eq!(cfg.tickets_states, vec!["Todo", "In Review"]);
        assert_eq!(cfg.tickets_assignee, "assigned");
        assert_eq!(cfg.tickets_people, vec!["u2"]);
        assert_eq!(cfg.tickets_labels, vec!["Bug"]);
        assert_eq!(cfg.park_idle_hours, 6);
        assert_eq!(
            cfg.setup_plugins,
            vec![SetupPlugin {
                plugin: "skill-router@iov42".into(),
                marketplace: Some("iov42/iov42-marketplace".into()),
                label: Some("Team skills".into()),
            }]
        );
        assert_eq!(cfg.min_week_left, 25);
        assert_eq!(
            cfg.slack_connect_url.as_deref(),
            Some("https://canopy.dev.iov42.net")
        );
        assert_eq!(
            cfg.gmail_client(),
            Some(crate::gmail::Client {
                id: "123-abc.apps.googleusercontent.com".into(),
                secret: "GOCSPX-test".into(),
            })
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
    fn a_gmail_client_needs_a_google_id_and_its_secret() {
        let (cfg, problems) = parse("gmail_client_id = \"not-google\"\n");
        assert_eq!(cfg.gmail_client_id, None);
        assert!(
            problems[0].contains("apps.googleusercontent.com"),
            "{problems:?}"
        );
        let cfg = clean("gmail_client_id = \"1.apps.googleusercontent.com\"\n");
        assert_eq!(cfg.gmail_client(), None);
    }

    #[test]
    fn a_setup_plugin_without_its_marketplace_is_reported() {
        let (cfg, problems) = parse("setup_plugins = [{ plugin = \"skill-router\" }]\n");
        assert!(cfg.setup_plugins.is_empty());
        assert!(problems[0].contains("name@marketplace"), "{problems:?}");
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
