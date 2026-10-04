use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::name::EnvName;
use crate::paths::HydraPaths;
use crate::secret::SecretRef;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("can't read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
    #[error("environment {0} doesn't exist (create it with `hydra env new {0}`)")]
    UnknownEnv(EnvName),
}

/// `~/.hydra/config.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GlobalConfig {
    pub default_shell: Option<String>,
    pub shells: Option<Vec<String>>,
    pub git_bash: Option<PathBuf>,
    pub claude_base: Option<String>,
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

/// `~/.hydra/envs/<name>/env.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvConfig {
    pub label: Option<String>,
    pub color: Option<String>,
    pub home: Option<String>,
    pub claude: Option<ClaudeConfig>,
    pub github: Option<GithubConfig>,
    pub git: Option<GitConfig>,
    pub aws: Option<AwsConfig>,
    pub azure: Option<Empty>,
    pub gcloud: Option<Empty>,
    pub gws: Option<GwsConfig>,
    pub kube: Option<KubeConfig>,
    pub codex: Option<Empty>,
    pub gemini: Option<GeminiConfig>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// A provider section with no settings, e.g. `[gcloud]`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClaudeConfig {
    #[serde(default)]
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    pub name: Option<String>,
    pub email: Option<String>,
    pub ssh_key: Option<String>,
    pub signing_key: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AwsConfig {
    pub profile: String,
    #[serde(default)]
    pub isolate: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GwsConfig {
    pub credentials: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KubeConfig {
    pub config: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GeminiConfig {
    pub api_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb`.
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

fn valid_var_name(k: &str) -> bool {
    let mut b = k.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn require_secret(section: &str, value: &str, example: &str) -> Result<(), String> {
    match SecretRef::parse_value(value) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!(
            "{section} must be a secret reference like \"{example}\""
        )),
        Err(e) => Err(format!("{section}: {e}")),
    }
}

impl EnvConfig {
    pub fn label_or<'a>(&'a self, name: &'a EnvName) -> &'a str {
        self.label.as_deref().unwrap_or(name.as_str())
    }

    pub fn rgb(&self) -> Option<Rgb> {
        self.color.as_deref().and_then(Rgb::parse)
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(c) = &self.color
            && Rgb::parse(c).is_none()
        {
            return Err(format!("color {c:?} must look like \"#1f9a8a\""));
        }
        let mut seen: Vec<&str> = Vec::new();
        for (k, v) in &self.env {
            if !valid_var_name(k) {
                return Err(format!("[env] {k:?} isn't a valid variable name"));
            }
            if k.eq_ignore_ascii_case("PATH")
                || k.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("HYDRA_"))
            {
                return Err(format!(
                    "[env] {k:?} can't be set here: hydra manages PATH and HYDRA_* itself; remove it"
                ));
            }
            if let Some(other) = seen.iter().find(|s| s.eq_ignore_ascii_case(k)) {
                return Err(format!(
                    "[env] {other:?} and {k:?} are the same variable on Windows; keep one"
                ));
            }
            seen.push(k);
            SecretRef::parse_value(v).map_err(|e| format!("[env] {k}: {e}"))?;
        }
        if let Some(GwsConfig {
            credentials: Some(c),
        }) = &self.gws
        {
            require_secret("[gws] credentials", c, "secret:work/gws-creds")?;
        }
        if let Some(g) = &self.gemini {
            require_secret("[gemini] api_key", &g.api_key, "secret:work/gemini")?;
        }
        Ok(())
    }
}

/// Reads a TOML file, dropping a UTF-8 BOM. `Ok(None)` if the file doesn't exist.
fn read_toml(path: &Path) -> Result<Option<String>, ConfigError> {
    let s = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if let Some(rest) = s.strip_prefix('\u{feff}') {
        return Ok(Some(rest.to_string()));
    }
    Ok(Some(s))
}

pub fn load_global(paths: &HydraPaths) -> Result<GlobalConfig, ConfigError> {
    let path = paths.config_file();
    let Some(text) = read_toml(&path)? else {
        return Ok(GlobalConfig::default());
    };
    let cfg: GlobalConfig = toml::from_str(&text).map_err(|e| ConfigError::Invalid {
        path: path.clone(),
        message: e.to_string(),
    })?;
    for (pattern, env) in &cfg.bindings {
        EnvName::parse(env).map_err(|e| ConfigError::Invalid {
            path: path.clone(),
            message: format!("binding {pattern:?}: {e}"),
        })?;
    }
    Ok(cfg)
}

pub fn load_env(paths: &HydraPaths, name: &EnvName) -> Result<EnvConfig, ConfigError> {
    let path = paths.env_file(name);
    let Some(text) = read_toml(&path)? else {
        return Err(ConfigError::UnknownEnv(name.clone()));
    };
    let cfg: EnvConfig = toml::from_str(&text).map_err(|e| ConfigError::Invalid {
        path: path.clone(),
        message: e.to_string(),
    })?;
    cfg.validate()
        .map_err(|message| ConfigError::Invalid { path, message })?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: &str = r##"
label = "acme work"
color = "#1f9a8a"
home  = "E:/acme"

[git]
name    = "David"
email   = "work@example.com"
ssh_key = "~/.ssh/id_ed25519_work"

[github]
owners = ["acme"]
strict = true

[aws]
profile = "acme-dev"

[gcloud]
[gws]

[claude]
mcp.exclude = ["acme*"]

[env]
LINEAR_API_KEY = "secret:work/linear"
"##;

    fn setup(env_toml: &str) -> (tempfile::TempDir, HydraPaths, EnvName) {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        let name = EnvName::parse("work").unwrap();
        std::fs::create_dir_all(paths.env_dir(&name)).unwrap();
        std::fs::write(paths.env_file(&name), env_toml).unwrap();
        (dir, paths, name)
    }

    #[test]
    fn loads_full_env() {
        let (_d, paths, name) = setup(WORK);
        let c = load_env(&paths, &name).unwrap();
        assert_eq!(c.label_or(&name), "acme work");
        assert_eq!(c.rgb(), Some(Rgb(0x1f, 0x9a, 0x8a)));
        assert_eq!(
            c.git.as_ref().unwrap().email.as_deref(),
            Some("work@example.com")
        );
        assert_eq!(c.github.as_ref().unwrap().owners, vec!["acme"]);
        assert!(c.github.as_ref().unwrap().strict);
        assert_eq!(c.aws.as_ref().unwrap().profile, "acme-dev");
        assert!(c.gcloud.is_some() && c.gws.is_some() && c.azure.is_none());
        assert_eq!(c.claude.as_ref().unwrap().mcp.exclude, vec!["acme*"]);
        assert_eq!(c.env["LINEAR_API_KEY"], "secret:work/linear");
    }

    #[test]
    fn label_defaults_to_name() {
        let (_d, paths, name) = setup("");
        assert_eq!(load_env(&paths, &name).unwrap().label_or(&name), "work");
    }

    #[test]
    fn loads_env_with_bom() {
        let (_d, paths, name) = setup("\u{feff}label = \"w\"\n");
        assert_eq!(load_env(&paths, &name).unwrap().label.as_deref(), Some("w"));
    }

    #[test]
    fn unknown_fields_are_errors() {
        let (_d, paths, name) = setup("[git]\nemial = \"x\"\n");
        let msg = load_env(&paths, &name).unwrap_err().to_string();
        assert!(msg.contains("emial"), "{msg}");
    }

    #[test]
    fn bad_colour_is_an_error() {
        let (_d, paths, name) = setup("color = \"teal\"\n");
        let msg = load_env(&paths, &name).unwrap_err().to_string();
        assert!(msg.contains("must look like \"#1f9a8a\""), "{msg}");
    }

    #[test]
    fn bad_secret_reference_is_an_error() {
        let (_d, paths, name) = setup("[env]\nX = \"secret:nope\"\n");
        let msg = load_env(&paths, &name).unwrap_err().to_string();
        assert!(msg.contains("[env] X"), "{msg}");
    }

    #[test]
    fn gemini_key_must_be_a_secret() {
        let (_d, paths, name) = setup("[gemini]\napi_key = \"AIza-plain\"\n");
        assert!(
            load_env(&paths, &name)
                .unwrap_err()
                .to_string()
                .contains("[gemini] api_key")
        );
    }

    #[test]
    fn missing_env_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        let err = load_env(&paths, &EnvName::parse("nope").unwrap()).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownEnv(_)));
        assert!(err.to_string().contains("hydra env new nope"));
    }

    #[test]
    fn global_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_global(&HydraPaths::new(dir.path())).unwrap(),
            GlobalConfig::default()
        );
    }

    #[test]
    fn global_parses_bindings_and_rejects_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        std::fs::write(
            paths.config_file(),
            "default_shell = \"bash\"\n[bindings]\n\"E:/acme/**\" = \"work\"\n",
        )
        .unwrap();
        let g = load_global(&paths).unwrap();
        assert_eq!(g.default_shell.as_deref(), Some("bash"));
        assert_eq!(g.bindings["E:/acme/**"], "work");

        std::fs::write(paths.config_file(), "[bindings]\n\"E:/x/**\" = \"Work\"\n").unwrap();
        assert!(
            load_global(&paths)
                .unwrap_err()
                .to_string()
                .contains("binding \"E:/x/**\"")
        );
    }

    #[test]
    fn env_rejects_hydra_and_path_variables() {
        for key in ["HYDRA_ENV", "hydra_env_vars", "PATH", "Path"] {
            let (_d, paths, name) = setup(&format!("[env]\n{key} = \"x\"\n"));
            let msg = load_env(&paths, &name).unwrap_err().to_string();
            assert!(
                msg.contains(&format!("[env] {key:?} can't be set here")),
                "{key}: {msg}"
            );
        }
    }

    #[test]
    fn env_rejects_keys_differing_only_in_case() {
        let (_d, paths, name) = setup("[env]\nRegion = \"a\"\nREGION = \"b\"\n");
        let msg = load_env(&paths, &name).unwrap_err().to_string();
        assert!(
            msg.contains("[env] \"REGION\" and \"Region\" are the same variable"),
            "{msg}"
        );
    }
}
