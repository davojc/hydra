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
    #[error("{0}")]
    Borrow(String),
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
    /// Tools borrowed from another environment (tool id -> owner). Filled by `parse_env`.
    #[serde(skip)]
    pub borrowed: BTreeMap<String, EnvName>,
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
        if self.claude.is_some()
            && self
                .env
                .keys()
                .any(|k| k.eq_ignore_ascii_case("CLAUDE_CONFIG_DIR"))
        {
            return Err("[claude] now sets CLAUDE_CONFIG_DIR itself - remove CLAUDE_CONFIG_DIR from [env]. Sign in again inside the environment (claude auth login); the old folder's sign-in isn't moved".to_string());
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
    if let Some(b) = &cfg.claude_base
        && !(b.starts_with('~') || Path::new(b).is_absolute())
    {
        return Err(ConfigError::Invalid {
            path,
            message: format!(
                "claude_base {b:?} must be an absolute path or start with ~ (like \"~/.claude\")"
            ),
        });
    }
    Ok(cfg)
}

/// Parses env.toml text for `name`: borrowed sections are filled in from their owners,
/// then the result is validated.
pub fn parse_env(paths: &HydraPaths, name: &EnvName, text: &str) -> Result<EnvConfig, ConfigError> {
    parse_with(paths, name, text, |table| {
        crate::borrow::resolve(paths, name, table).map_err(ConfigError::Borrow)
    })
}

/// Parses `text` after `adjust` has changed its table (borrowing); `adjust` returns which tools
/// are borrowed from whom. Then validates.
fn parse_with(
    paths: &HydraPaths,
    name: &EnvName,
    text: &str,
    adjust: impl FnOnce(&mut toml::Table) -> Result<BTreeMap<String, EnvName>, ConfigError>,
) -> Result<EnvConfig, ConfigError> {
    let path = paths.env_file(name);
    let invalid = |message: String| ConfigError::Invalid {
        path: path.clone(),
        message,
    };
    let mut table: toml::Table = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let original = table.clone();
    let borrowed = adjust(&mut table)?;
    // When nothing changed, parse the text itself so errors keep their line numbers.
    let parsed: Result<EnvConfig, toml::de::Error> = if table == original {
        toml::from_str(text)
    } else {
        table.try_into()
    };
    let mut cfg = parsed.map_err(|e| invalid(e.to_string()))?;
    cfg.borrowed = borrowed;
    cfg.validate().map_err(invalid)?;
    Ok(cfg)
}

fn read_env(paths: &HydraPaths, name: &EnvName) -> Result<String, ConfigError> {
    read_toml(&paths.env_file(name))?.ok_or_else(|| ConfigError::UnknownEnv(name.clone()))
}

pub fn load_env(paths: &HydraPaths, name: &EnvName) -> Result<EnvConfig, ConfigError> {
    parse_env(paths, name, &read_env(paths, name)?)
}

/// The environment's own settings, without reading any other environment: a borrowed
/// section is left out, except `[github]`, which keeps its own owners and strict. For the
/// guards and the ssh shim, which must not weaken when an owner's env.toml breaks.
/// `borrowed` is empty.
pub fn load_env_own(paths: &HydraPaths, name: &EnvName) -> Result<EnvConfig, ConfigError> {
    parse_with(paths, name, &read_env(paths, name)?, |table| {
        crate::borrow::keep_own(table);
        Ok(BTreeMap::new())
    })
}

/// Like [`load_env`], but a tool that can't be borrowed is left out instead of failing the
/// environment; it is returned with the error (for `hydra whoami`).
pub fn load_env_lenient(
    paths: &HydraPaths,
    name: &EnvName,
) -> Result<(EnvConfig, Vec<(String, String)>), ConfigError> {
    let mut failed = Vec::new();
    let cfg = parse_with(paths, name, &read_env(paths, name)?, |table| {
        let (borrowed, f) = crate::borrow::resolve_lenient(paths, name, table);
        failed = f;
        Ok(borrowed)
    })?;
    Ok((cfg, failed))
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
    fn claude_config_dir_in_env_is_rejected_with_claude_section() {
        for key in ["CLAUDE_CONFIG_DIR", "claude_config_dir"] {
            let (_d, paths, name) = setup(&format!("[claude]\n[env]\n{key} = \"C:/x\"\n"));
            let msg = load_env(&paths, &name).unwrap_err().to_string();
            assert!(
                msg.contains("[claude] now sets CLAUDE_CONFIG_DIR itself - remove CLAUDE_CONFIG_DIR from [env]. Sign in again inside the environment (claude auth login); the old folder's sign-in isn't moved"),
                "{key}: {msg}"
            );
        }
        let (_d, paths, name) = setup("[env]\nCLAUDE_CONFIG_DIR = \"C:/x\"\n");
        assert!(load_env(&paths, &name).is_ok(), "fine without [claude]");
    }

    #[test]
    fn claude_base_must_be_absolute_or_start_with_tilde() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        std::fs::write(paths.config_file(), "claude_base = \"dotfiles/claude\"\n").unwrap();
        let msg = load_global(&paths).unwrap_err().to_string();
        assert!(msg.contains("claude_base"), "{msg}");
        let abs = dir
            .path()
            .join("dotfiles")
            .to_string_lossy()
            .replace('\\', "/");
        for ok in ["~/dotfiles/claude", "~", abs.as_str()] {
            std::fs::write(paths.config_file(), format!("claude_base = \"{ok}\"\n")).unwrap();
            assert!(load_global(&paths).is_ok(), "{ok}");
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

    fn add_env(paths: &HydraPaths, name: &str, toml: &str) {
        let n = EnvName::parse(name).unwrap();
        std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
        std::fs::write(paths.env_file(&n), toml).unwrap();
    }

    fn borrow_err(work: &str, personal: Option<&str>) -> String {
        let (_d, paths, name) = setup(work);
        if let Some(p) = personal {
            add_env(&paths, "personal", p);
        }
        load_env(&paths, &name).unwrap_err().to_string()
    }

    #[test]
    fn borrowed_section_takes_the_owners_settings() {
        let (_d, paths, name) =
            setup("[claude]\nfrom = \"personal\"\n\n[github]\nfrom = \"personal\"\n");
        add_env(
            &paths,
            "personal",
            "[claude]\nmcp.exclude = [\"x\"]\n\n[github]\nowners = [\"me\"]\n",
        );
        let c = load_env(&paths, &name).unwrap();
        assert_eq!(c.claude.unwrap().mcp.exclude, vec!["x"]);
        // [github] owners and strict are guard policy: never taken from the owner.
        assert_eq!(c.github.unwrap(), GithubConfig::default());
        assert_eq!(c.borrowed["claude"].as_str(), "personal");
        assert_eq!(c.borrowed["github"].as_str(), "personal");
        assert!(!c.borrowed.contains_key("git"));
    }

    #[test]
    fn inline_borrow_and_bom_crlf_owner_work() {
        let (_d, paths, name) = setup("claude = { from = \"personal\" }\r\n");
        add_env(&paths, "personal", "\u{feff}[claude]\r\n");
        let c = load_env(&paths, &name).unwrap();
        assert!(c.claude.is_some());
        assert_eq!(c.borrowed["claude"].as_str(), "personal");
    }

    #[test]
    fn owner_without_the_section_is_an_error() {
        let e = borrow_err("[claude]\nfrom = \"personal\"\n", Some("label = \"p\"\n"));
        assert_eq!(
            e,
            "work borrows [claude] from personal, but personal has no [claude]\n  -> add it there: hydra add claude personal"
        );
    }

    #[test]
    fn chains_are_an_error() {
        let e = borrow_err(
            "[claude]\nfrom = \"personal\"\n",
            Some("[claude]\nfrom = \"client\"\n"),
        );
        assert_eq!(
            e,
            "work borrows [claude] from personal, which borrows it from client\n  -> borrow from the owner: from = \"client\""
        );
    }

    #[test]
    fn unknown_owner_is_an_error() {
        let e = borrow_err("[codex]\nfrom = \"ghost\"\n", None);
        assert_eq!(
            e,
            "work borrows [codex] from ghost, but there's no environment ghost\n  -> fix from in [codex] in work's env.toml"
        );
    }

    #[test]
    fn self_borrow_is_an_error() {
        let e = borrow_err("[codex]\nfrom = \"work\"\n", None);
        assert_eq!(
            e,
            "work can't borrow [codex] from itself\n  -> remove from = \"work\""
        );
    }

    #[test]
    fn borrowed_section_with_other_keys_is_an_error() {
        let e = borrow_err(
            "[github]\nfrom = \"personal\"\nstrict = true\ntoken = \"x\"\n",
            Some("[github]\n"),
        );
        assert_eq!(
            e,
            "[github] borrows from personal, so it can't also set token\n  -> remove token; personal's [github] settings are used"
        );
        // owners and strict are allowed next to from in [github] only.
        let e = borrow_err(
            "[claude]\nfrom = \"personal\"\nstrict = true\n",
            Some("[claude]\n"),
        );
        assert_eq!(
            e,
            "[claude] borrows from personal, so it can't also set strict\n  -> remove strict; personal's [claude] settings are used"
        );
    }

    #[test]
    fn borrowed_github_keeps_the_borrowers_own_owners_and_strict() {
        let (_d, paths, name) =
            setup("[github]\nfrom = \"personal\"\nowners = [\"acme\"]\nstrict = true\n");
        add_env(
            &paths,
            "personal",
            "[github]\nowners = [\"me\"]\nstrict = false\n",
        );
        let c = load_env(&paths, &name).unwrap();
        let g = c.github.unwrap();
        assert_eq!(g.owners, vec!["acme"]);
        assert!(g.strict);
        assert_eq!(c.borrowed["github"].as_str(), "personal");
    }

    #[test]
    fn borrowed_github_does_not_inherit_the_owners_policy() {
        let (_d, paths, name) = setup("[github]\nfrom = \"personal\"\n");
        add_env(
            &paths,
            "personal",
            "[github]\nowners = [\"me\"]\nstrict = true\n",
        );
        let c = load_env(&paths, &name).unwrap();
        assert_eq!(c.github.unwrap(), GithubConfig::default());
        assert_eq!(c.borrowed["github"].as_str(), "personal");
    }

    #[test]
    fn own_load_ignores_the_owner() {
        let work = "[git]\nssh_key = \"~/.ssh/work\"\n[github]\nfrom = \"personal\"\nowners = [\"acme\"]\nstrict = true\n[codex]\nfrom = \"personal\"\n[aws]\nfrom = \"personal\"\n";
        // No personal at all: borrowing fails, the env's own settings still load.
        let (_d, paths, name) = setup(work);
        assert!(matches!(
            load_env(&paths, &name),
            Err(ConfigError::Borrow(_))
        ));
        let c = load_env_own(&paths, &name).unwrap();
        assert_eq!(c.git.unwrap().ssh_key.as_deref(), Some("~/.ssh/work"));
        let g = c.github.unwrap();
        assert_eq!(g.owners, vec!["acme"]);
        assert!(g.strict);
        assert!(c.codex.is_none() && c.aws.is_none());
        assert!(c.borrowed.is_empty());
        // The env's own mistakes are still errors, with line numbers when nothing was borrowed.
        let (_d, paths, name) = setup("[git]\nemial = \"x\"\n");
        let e = load_env_own(&paths, &name).unwrap_err().to_string();
        assert!(e.contains("emial") && e.contains("line 2"), "{e}");
    }

    #[test]
    fn lenient_load_leaves_out_only_the_broken_borrow() {
        let (_d, paths, name) = setup(
            "label = \"w\"\n[claude]\nfrom = \"personal\"\n[codex]\nfrom = \"personal\"\n[gcloud]\n",
        );
        add_env(&paths, "personal", "[claude]\n");
        let (c, failed) = load_env_lenient(&paths, &name).unwrap();
        assert_eq!(c.label.as_deref(), Some("w"));
        assert!(c.claude.is_some() && c.gcloud.is_some() && c.codex.is_none());
        assert_eq!(c.borrowed["claude"].as_str(), "personal");
        assert!(!c.borrowed.contains_key("codex"));
        assert_eq!(
            failed,
            vec![(
                "codex".to_string(),
                "work borrows [codex] from personal, but personal has no [codex]\n  -> add it there: hydra add codex personal".to_string()
            )]
        );
    }

    #[test]
    fn git_cannot_be_borrowed() {
        let e = borrow_err(
            "[git]\nfrom = \"personal\"\n",
            Some("[git]\nname = \"x\"\n"),
        );
        assert_eq!(
            e,
            "[git] can't be borrowed: commit author and SSH key stay per environment\n  -> set [git] in work itself"
        );
    }

    #[test]
    fn from_must_be_a_name() {
        let e = borrow_err("[codex]\nfrom = 3\n", None);
        assert!(
            e.contains("[codex] from must be an environment name"),
            "{e}"
        );
    }

    #[test]
    fn parse_errors_keep_their_line_numbers() {
        let e = borrow_err("[claude]\nnope = 1\n", None);
        assert!(e.contains("line 2"), "{e}");
    }
}
