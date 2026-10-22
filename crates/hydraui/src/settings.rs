//! The Settings tab's model: an environment's variables and credentials as its page lists
//! them, read from its env.toml text without resolving borrowed tools.
use std::collections::BTreeMap;

use hydra_core::secret::SecretRef;

/// Colours offered for an environment, as `#rrggbb`.
pub const ENV_COLOURS: [&str; 8] = [
    "#1f9a8a", "#3b82f6", "#8b5cf6", "#d946ef", "#ef4444", "#f59e0b", "#22c55e", "#64748b",
];

/// The shells `default_shell` may name.
pub const SHELLS: [&str; 3] = ["pwsh", "bash", "zsh"];

/// One secret an environment refers to or holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    /// `env/key`.
    pub secret: String,
    /// Where env.toml uses it, e.g. `[jev] JEV_API_KEY`; "not used" when nothing does.
    pub used_by: String,
    /// The variable it becomes in a terminal, which agents can be told about.
    pub var: Option<String>,
    /// Whether the store holds a value for it.
    pub stored: bool,
    /// What `[agents]` tells agents about `var`, when it does.
    pub agents: Option<String>,
}

fn table(env_toml: &str) -> toml::Table {
    toml::from_str(env_toml.strip_prefix('\u{feff}').unwrap_or(env_toml)).unwrap_or_default()
}

fn secret_in(value: Option<&toml::Value>) -> Option<String> {
    let s = value?.as_str()?;
    SecretRef::parse_value(s).ok().flatten().map(|r| r.path())
}

/// The `[env]` variables by name, each with its value, or `secret:env/key` for a secret.
pub fn variables(env_toml: &str) -> Vec<(String, String)> {
    let t = table(env_toml);
    let Some(env) = t.get("env").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    env.iter()
        .map(|(k, v)| {
            let shown = v
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string());
            (k.clone(), shown)
        })
        .collect()
}

/// Whether a variable's name says its value is a secret (`*_KEY`, `*SECRET*`, `*TOKEN*`,
/// `*PASSWORD*`), so a plain value is masked on screen.
pub fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.ends_with("KEY")
        || ["SECRET", "TOKEN", "PASSWORD", "PASSWD"]
            .iter()
            .any(|w| n.contains(w))
}

/// How the Settings tab shows a variable's value: a secret reference as it is, a plain value
/// whose name looks secret masked, with a note that it sits in env.toml in plain text.
pub fn shown_value(name: &str, value: &str) -> (String, Option<&'static str>) {
    if value.starts_with("secret:") || !looks_secret(name) {
        return (value.to_string(), None);
    }
    (
        "········".to_string(),
        Some("plain text in env.toml: store it as a secret instead"),
    )
}

/// `[agents]`: variable -> what agents are told it is for.
pub fn agents(env_toml: &str) -> BTreeMap<String, String> {
    table(env_toml)
        .get("agents")
        .and_then(toml::Value::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The secrets `env`'s env.toml refers to, then any others stored under `env` (`stored`:
/// their keys, as the store lists them), each with where it is used.
pub fn credentials(env: &str, env_toml: &str, stored: &[String]) -> Vec<Credential> {
    let t = table(env_toml);
    let told = agents(env_toml);
    let mut out: Vec<Credential> = Vec::new();
    let add = |out: &mut Vec<Credential>, secret: String, used_by: String, var: Option<String>| {
        if out.iter().any(|c| c.secret == secret && c.var == var) {
            return;
        }
        let key = secret.strip_prefix(&format!("{env}/")).unwrap_or("");
        out.push(Credential {
            stored: stored.iter().any(|s| s == key),
            agents: var.as_ref().and_then(|v| told.get(v).cloned()),
            secret,
            used_by,
            var,
        });
    };
    for (tool, var) in [("jev", "JEV_API_KEY"), ("gemini", "GEMINI_API_KEY")] {
        let section = t.get(tool).and_then(toml::Value::as_table);
        if let Some(s) = secret_in(section.and_then(|s| s.get("api_key"))) {
            add(
                &mut out,
                s,
                format!("[{tool}] {var}"),
                Some(var.to_string()),
            );
        }
    }
    let gws = t.get("gws").and_then(toml::Value::as_table);
    if let Some(s) = secret_in(gws.and_then(|g| g.get("credentials"))) {
        add(&mut out, s, "[gws] credentials file".into(), None);
    }
    if let Some(vars) = t.get("env").and_then(toml::Value::as_table) {
        for (k, v) in vars {
            if let Some(s) = secret_in(Some(v)) {
                add(&mut out, s, format!("[env] {k}"), Some(k.clone()));
            }
        }
    }
    let mut keys = stored.to_vec();
    keys.sort();
    for key in keys {
        let secret = format!("{env}/{key}");
        if !out.iter().any(|c| c.secret == secret) {
            add(&mut out, secret, "not used".into(), None);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: &str = "label = \"Work\"\n\
        [jev]\napi_key = \"secret:work/jev\"\n\
        [gws]\ncredentials = \"secret:work/gws-creds\"\n\
        [env]\nREGION = \"eu-west\"\nLINEAR_API_KEY = \"secret:work/linear\"\n\
        [agents]\nJEV_API_KEY = \"Jev\"\n";

    #[test]
    fn variables_show_secret_references() {
        assert_eq!(
            variables(WORK),
            vec![
                (
                    "LINEAR_API_KEY".to_string(),
                    "secret:work/linear".to_string()
                ),
                ("REGION".to_string(), "eu-west".to_string()),
            ]
        );
        assert!(variables("label = \"x\"\n").is_empty());
    }

    #[test]
    fn credentials_say_where_each_secret_is_used_and_whether_it_is_stored() {
        let got = credentials("work", WORK, &["jev".into(), "old".into()]);
        let row = |s: &str| got.iter().find(|c| c.secret == s).unwrap().clone();
        assert_eq!(
            row("work/jev"),
            Credential {
                secret: "work/jev".into(),
                used_by: "[jev] JEV_API_KEY".into(),
                var: Some("JEV_API_KEY".into()),
                stored: true,
                agents: Some("Jev".into()),
            }
        );
        let linear = row("work/linear");
        assert_eq!(linear.used_by, "[env] LINEAR_API_KEY");
        assert!(!linear.stored && linear.agents.is_none());
        assert_eq!(row("work/gws-creds").var, None);
        assert_eq!(row("work/old").used_by, "not used");
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn plain_values_with_secret_names_are_masked() {
        assert_eq!(
            shown_value("REGION", "eu-west"),
            ("eu-west".to_string(), None)
        );
        assert_eq!(
            shown_value("LINEAR_API_KEY", "secret:work/linear").0,
            "secret:work/linear"
        );
        let (shown, note) = shown_value("GOOGLE_CLIENT_SECRET", "GOCSPX-abc");
        assert!(!shown.contains("GOCSPX") && note.is_some());
        assert!(looks_secret("github_token") && looks_secret("API_KEY"));
        assert!(!looks_secret("KEYBOARD"));
    }

    #[test]
    fn agents_reads_the_table() {
        assert_eq!(agents(WORK)["JEV_API_KEY"], "Jev");
        assert!(agents("").is_empty());
    }
}
