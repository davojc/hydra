//! Is this file an OpenAPI spec (spec §7.8)? Decided from its extension and its first 4 KB
//! only, so other YAML and JSON never pays for a parse.
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    OpenApi3,
    Swagger2,
}

const HEAD: usize = 4096;

pub fn spec_kind(path: &Path, text: &str) -> Option<SpecKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mut end = text.len().min(HEAD);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = text[..end].trim_start_matches('\u{feff}');
    let (key, value) = match ext.as_str() {
        "yaml" | "yml" => yaml_top_level(head)?,
        "json" => json_top_level(head)?,
        _ => return None,
    };
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
    match key {
        "openapi" if value.starts_with("3.") => Some(SpecKind::OpenApi3),
        "swagger" if value.starts_with("2.") => Some(SpecKind::Swagger2),
        _ => None,
    }
}

/// The first unindented `openapi:` or `swagger:` line, as (key, value).
fn yaml_top_level(head: &str) -> Option<(&'static str, &str)> {
    head.lines().find_map(|line| {
        ["openapi", "swagger"].into_iter().find_map(|key| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(|v| (key, v.split(" #").next().unwrap_or(v)))
        })
    })
}

/// `"openapi"` or `"swagger"` as a key of the outermost object, as (key, value).
fn json_top_level(head: &str) -> Option<(&'static str, &str)> {
    let bytes = head.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                let word = head.get(start..j.min(bytes.len()))?;
                let rest = head.get(j + 1..).unwrap_or("").trim_start();
                if depth == 1 && rest.starts_with(':') {
                    for key in ["openapi", "swagger"] {
                        if word == key {
                            let v = rest[1..].trim_start();
                            let v = v.split([',', '}', '\n']).next().unwrap_or(v);
                            return Some((key, v));
                        }
                    }
                }
                i = j;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str, text: &str) -> Option<SpecKind> {
        spec_kind(Path::new(name), text)
    }

    #[test]
    fn openapi_3_in_yaml_and_json() {
        assert_eq!(
            kind("api.yaml", "openapi: 3.0.3\ninfo: {}\n"),
            Some(SpecKind::OpenApi3)
        );
        assert_eq!(
            kind("api.YML", "openapi: '3.1.0'\n"),
            Some(SpecKind::OpenApi3)
        );
        assert_eq!(
            kind("api.yaml", "# c\n---\nopenapi: \"3.1.0\"\n"),
            Some(SpecKind::OpenApi3)
        );
        assert_eq!(
            kind(
                "api.json",
                "{\n  \"info\": {\"x\": {\"y\": 1}},\n  \"openapi\": \"3.0.0\"\n}"
            ),
            Some(SpecKind::OpenApi3)
        );
        assert_eq!(
            kind("api.yaml", "\u{feff}openapi: 3.0.0\n"),
            Some(SpecKind::OpenApi3)
        );
    }

    #[test]
    fn swagger_2_is_told_apart() {
        assert_eq!(
            kind("s.yaml", "swagger: \"2.0\"\n"),
            Some(SpecKind::Swagger2)
        );
        assert_eq!(
            kind("s.json", "{\"swagger\": \"2.0\"}"),
            Some(SpecKind::Swagger2)
        );
    }

    #[test]
    fn only_a_top_level_key_counts() {
        assert_eq!(kind("a.yaml", "x-meta:\n  openapi: 3.0.0\n"), None);
        assert_eq!(kind("a.json", "{\"x\": {\"openapi\": \"3.0.0\"}}"), None);
        assert_eq!(
            kind("a.json", "{\"note\": \"openapi: 3.0.0\"}"),
            None,
            "inside a string"
        );
        let late = format!("{}openapi: 3.0.0\n", "# pad\n".repeat(800));
        assert_eq!(kind("a.yaml", &late), None, "after the first 4 KB");
    }

    #[test]
    fn other_files_and_versions_are_not_specs() {
        assert_eq!(kind("ci.yml", "on: push\njobs: {}\n"), None);
        assert_eq!(kind("a.yaml", "openapi: 2.5\n"), None);
        assert_eq!(kind("a.txt", "openapi: 3.0.0\n"), None, "extension");
        assert_eq!(kind("a.toml", "openapi = \"3.0.0\"\n"), None);
    }
}
