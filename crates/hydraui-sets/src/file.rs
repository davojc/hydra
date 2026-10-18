//! The set file as written: every field optional, checked later by `resolve`.
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, rename = "tab")]
    pub tabs: Vec<TabFile>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TabFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<NodeFile>,
}

/// A layout node: a split when `split` is set, otherwise a pane. One struct (not an untagged
/// enum) so a typo gets serde's "unknown field" error instead of "no variant matched".
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panes: Option<Vec<NodeFile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// A file pane (spec §7.8): the files it shows, absolute or relative like `cwd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Index into `files` of the file tab shown first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
}

/// Parses a set file, dropping a UTF-8 BOM (Notepad adds one).
pub fn parse(text: &str) -> Result<SetFile, toml::de::Error> {
    toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
env = "work"
cwd = "E:/work/client-a"

[[tab]]
title = "api"
cwd = "api"
layout = { split = "right", ratio = 0.6, panes = [
  { command = "claude" },
  { split = "down", panes = [ { command = "npm run dev" }, { } ] },
] }

[[tab]]
title = "infra"
env = "personal"
command = "terraform plan"
"#;

    #[test]
    fn reads_the_spec_example() {
        let s = parse(SAMPLE).unwrap();
        assert_eq!(s.env.as_deref(), Some("work"));
        assert_eq!(s.tabs.len(), 2);
        let layout = s.tabs[0].layout.as_ref().unwrap();
        assert_eq!(layout.split.as_deref(), Some("right"));
        assert_eq!(layout.ratio, Some(0.6));
        let panes = layout.panes.as_ref().unwrap();
        assert_eq!(panes[0].command.as_deref(), Some("claude"));
        assert_eq!(panes[1].panes.as_ref().unwrap().len(), 2);
        assert_eq!(s.tabs[1].command.as_deref(), Some("terraform plan"));
    }

    #[test]
    fn notepad_bom_and_crlf_read_the_same() {
        let crlf = format!("\u{feff}{}", SAMPLE.replace('\n', "\r\n"));
        let a = parse(SAMPLE).unwrap();
        let b = parse(&crlf).unwrap();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn unknown_keys_are_errors_with_a_line() {
        let e = parse("env = \"work\"\n\n[[tab]]\ntitel = \"x\"\n").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("titel"), "{msg}");
        assert!(msg.contains("line 4"), "{msg}");
    }
}
