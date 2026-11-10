//! Markdown with TOML front matter between `+++` lines (spec §2): tickets, decisions, meetings,
//! progress write-ups.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Error;

const FENCE: &str = "+++";

/// Splits `text` into its front matter and its body. A file without front matter is all body.
pub fn split(text: &str) -> (&str, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix(FENCE)
        .and_then(|r| r.strip_prefix("\r\n").or_else(|| r.strip_prefix('\n')))
    else {
        return ("", text);
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == FENCE {
            let body = &rest[at + line.len()..];
            let body = body
                .strip_prefix("\r\n")
                .or_else(|| body.strip_prefix('\n'))
                .unwrap_or(body);
            return (&rest[..at], body);
        }
        at += line.len();
    }
    ("", text)
}

/// Reads a file's front matter as `T` and returns it with the body.
pub fn parse<T: DeserializeOwned>(text: &str) -> Result<(T, String), Error> {
    let (meta, body) = split(text);
    let meta = toml::from_str(meta).map_err(|e| Error::Toml(e.to_string()))?;
    Ok((meta, body.to_string()))
}

/// Writes `meta` as front matter above `body`.
pub fn render<T: Serialize>(meta: &T, body: &str) -> Result<String, Error> {
    let meta = toml::to_string(meta).map_err(|e| Error::Toml(e.to_string()))?;
    let mut out = format!("{FENCE}\n{meta}");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(FENCE);
    out.push('\n');
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body.trim_start_matches(['\r', '\n']));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct Meta {
        title: String,
    }

    #[test]
    fn splits_front_matter_from_body() {
        let (meta, body) = split("+++\ntitle = \"a\"\n+++\n\nHello\n");
        assert_eq!(meta, "title = \"a\"\n");
        assert_eq!(body, "Hello\n");
    }

    #[test]
    fn crlf_files_split_too() {
        let (meta, body) = split("+++\r\ntitle = \"a\"\r\n+++\r\nHello");
        assert_eq!(meta, "title = \"a\"\r\n");
        assert_eq!(body, "Hello");
    }

    #[test]
    fn a_file_without_front_matter_is_all_body() {
        assert_eq!(split("# Notes\n"), ("", "# Notes\n"));
        // An opening fence with no closing one isn't front matter.
        assert_eq!(split("+++\ntitle = 1\n"), ("", "+++\ntitle = 1\n"));
    }

    #[test]
    fn render_round_trips() {
        let meta = Meta {
            title: "Share dialog".into(),
        };
        let text = render(&meta, "Body text").unwrap();
        assert_eq!(text, "+++\ntitle = \"Share dialog\"\n+++\n\nBody text\n");
        let (back, body): (Meta, String) = parse(&text).unwrap();
        assert_eq!(back, meta);
        assert_eq!(body, "Body text\n");
    }

    #[test]
    fn an_empty_body_writes_no_blank_line() {
        let text = render(&Meta { title: "x".into() }, "").unwrap();
        assert_eq!(text, "+++\ntitle = \"x\"\n+++\n");
    }
}
