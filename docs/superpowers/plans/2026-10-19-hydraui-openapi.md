# hydraui OpenAPI View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OpenAPI 3.x files (`.yaml`, `.yml`, `.json`) open in the file pane's Rendered view as native, read-only API docs: operations grouped by tag, expandable rows, schema trees, a filter, and errors that point at the line.

**Architecture:** A pure `openapi` module in hydraui (no UI): `detect` (is this file a spec, from its first 4 KB), `model` (YAML/JSON → `serde_json::Value` → `Api` with tag groups, operations, parameters, bodies, responses), and `schema` (one level of a schema at a time, following local `$ref`s lazily, with cycle, missing and external-file cases). A new `ApiView` entity renders an `Api` and keeps what is open; the file pane gives OpenAPI text bodies an `ApiView`, shows Rendered first, reparses 300 ms after edits, and routes the view's Go to line / open-file events.

**Tech Stack:** Rust 2024, `serde-saphyr` 1.3 (already in Cargo.lock via rust-i18n), `serde_json` with `preserve_order` (already a workspace dependency; keeps file order), GPUI + gpui-component (`h_flex`/`v_flex`, `Button`, `input::{Input, InputState}`, `ActiveTheme`), `gpui_base::TextView::markdown` for descriptions.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` — §7.8 File pane, "OpenAPI specs" (and the Raw / Rendered lines above it).

## Global Constraints

- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`.
- Gate before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (check its exit status), `cargo test --workspace`.
- Tests use `tempfile` dirs only; never the real `~/.hydra`.
- Detection: a `.yaml`/`.yml`/`.json` file whose **top level** has `openapi:` with a value starting `3.`, read from the first 4 KB only. `swagger: "2.0"` opens in Raw with "Swagger 2.0 isn't rendered" and no switch (§7.8).
- Nothing is fetched from the network; server URLs are text; only `http`, `https`, `mailto` links in descriptions open (as Markdown already does).
- Refs: `#/...` followed lazily; a schema already open above shows `↻ <name>`; unresolvable shows `⚠ <ref> (missing)`; `other.yaml#/X` is a label that opens that file relative to the spec's folder.
- Method colours: GET blue (`info`), POST green (`success`), PUT amber (`warning`), PATCH teal, DELETE red (`danger`), others grey (`muted_foreground`). Status: 2xx green, 4xx amber, 5xx red, others muted.
- Reparse 300 ms after typing stops; open rows stay open by `METHOD path`.
- Write edit scripts with the Write/Edit tools (shell heredocs halve backslashes here).

## Review Focus

1. **Unquoted YAML status codes** (`200:` under `responses`) are integers to YAML; parsed into `serde_json::Value` they must become the string key `"200"`, not a parse error. Pinned in Task 2 (`unquoted_status_codes_are_keys`).
2. **A huge spec** (~1 MB, thousands of operations) must open without freezing: collapsed rows only, schema trees built only when expanded, parsing off the UI thread. Pinned by Task 5's smoke (a generated 1 MB spec, timed) and Task 4's `ApiView::set_text` running the parse in `background_spawn`.
3. **`openapi` nested under another key** (e.g. `x-meta:\n  openapi: 3.0.0`) or appearing only after 4 KB must not be detected; JSON with `"openapi"` inside a nested object likewise. Pinned in Task 1 (`only_a_top_level_key_counts`).
4. **Self-referencing schemas** (`Node { children: Node[] }`) must show `↻ Node` one level down instead of recursing forever. Pinned in Task 3 (`a_schema_open_above_shows_a_cycle`).
5. **Editing a spec into an invalid state** must keep the last good render (greyed) with the error's line, and a later valid edit must restore it with open rows still open. Pinned in Task 4 (`a_failed_parse_keeps_the_last_good_api`).

---

### Task 1: Detecting a spec

**Files:**
- Create: `crates/hydraui/src/openapi/mod.rs`, `crates/hydraui/src/openapi/detect.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod openapi;`)
- Test: in `detect.rs`

**Interfaces:**
- Produces:
  - `pub enum SpecKind { OpenApi3, Swagger2 }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn spec_kind(path: &Path, text: &str) -> Option<SpecKind>` — `None` for anything else; looks at `text[..4096]` (on a char boundary) and the extension (`yaml`, `yml`, `json`, any case)
  - `mod.rs`: `pub mod detect; pub use detect::{SpecKind, spec_kind};` (Tasks 2–3 add `model`, `schema`)

- [ ] **Step 1: Write the failing tests** — `detect.rs` with `spec_kind` bodied `todo!()` and:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str, text: &str) -> Option<SpecKind> {
        spec_kind(Path::new(name), text)
    }

    #[test]
    fn openapi_3_in_yaml_and_json() {
        assert_eq!(kind("api.yaml", "openapi: 3.0.3\ninfo: {}\n"), Some(SpecKind::OpenApi3));
        assert_eq!(kind("api.YML", "openapi: '3.1.0'\n"), Some(SpecKind::OpenApi3));
        assert_eq!(kind("api.yaml", "# c\n---\nopenapi: \"3.1.0\"\n"), Some(SpecKind::OpenApi3));
        assert_eq!(
            kind("api.json", "{\n  \"info\": {\"x\": {\"y\": 1}},\n  \"openapi\": \"3.0.0\"\n}"),
            Some(SpecKind::OpenApi3)
        );
        assert_eq!(kind("api.yaml", "\u{feff}openapi: 3.0.0\n"), Some(SpecKind::OpenApi3));
    }

    #[test]
    fn swagger_2_is_told_apart() {
        assert_eq!(kind("s.yaml", "swagger: \"2.0\"\n"), Some(SpecKind::Swagger2));
        assert_eq!(kind("s.json", "{\"swagger\": \"2.0\"}"), Some(SpecKind::Swagger2));
    }

    #[test]
    fn only_a_top_level_key_counts() {
        assert_eq!(kind("a.yaml", "x-meta:\n  openapi: 3.0.0\n"), None);
        assert_eq!(kind("a.json", "{\"x\": {\"openapi\": \"3.0.0\"}}"), None);
        assert_eq!(kind("a.json", "{\"note\": \"openapi: 3.0.0\"}"), None, "inside a string");
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
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib -- openapi::detect`
Expected: FAIL — `not yet implemented` in all four.

- [ ] **Step 3: Implement**

```rust
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
```

`mod.rs`:

```rust
//! OpenAPI specs as API docs (spec §7.8): detection, the model, and schema trees. No UI.
pub mod detect;

pub use detect::{SpecKind, spec_kind};
```

- [ ] **Step 4: Run them**

Run: `cargo test -p hydraui --lib -- openapi::detect`
Expected: PASS (4).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: hydraui tells OpenAPI specs from other YAML and JSON"
```

---

### Task 2: The model

**Files:**
- Create: `crates/hydraui/src/openapi/model.rs`
- Modify: `crates/hydraui/src/openapi/mod.rs` (`pub mod model;` + re-exports), `crates/hydraui/Cargo.toml` (`serde-saphyr = "1.3"`, `serde_json.workspace = true` if not present)
- Test: in `model.rs`

**Interfaces:**
- Consumes: nothing from Task 1 at the code level.
- Produces:
  - `pub struct ParseError { pub message: String, pub line: Option<usize> }` (`Debug, Clone, PartialEq`; `line` 1-based)
  - `pub fn parse(text: &str, json: bool) -> Result<Api, ParseError>`
  - `pub struct Api { pub title: String, pub version: String, pub description: Option<String>, pub servers: Vec<String>, pub groups: Vec<TagGroup>, pub schemas: Vec<String>, pub missing: Vec<&'static str>, pub root: serde_json::Value }` (`Debug, Clone`)
  - `pub struct TagGroup { pub name: String, pub description: Option<String>, pub operations: Vec<Operation> }`
  - `pub struct Operation { pub method: String /* upper case */, pub path: String, pub summary: Option<String>, pub description: Option<String>, pub operation_id: Option<String>, pub deprecated: bool, pub parameters: Vec<Parameter>, pub request_body: Option<Body>, pub responses: Vec<Response> }` with `pub fn key(&self) -> String` = `"{method} {path}"`
  - `pub struct Parameter { pub name: String, pub location: String, pub required: bool, pub description: Option<String>, pub schema: serde_json::Value }`
  - `pub struct Body { pub description: Option<String>, pub content: Vec<(String, serde_json::Value)> }` (media type, schema)
  - `pub struct Response { pub status: String, pub description: Option<String>, pub content: Vec<(String, serde_json::Value)> }`
  - `pub fn resolve<'a>(root: &'a serde_json::Value, pointer: &str) -> Option<&'a serde_json::Value>` — `#/a/b~1c` (JSON pointer unescaping `~1`→`/`, `~0`→`~`)
  - All derive `Debug, Clone, PartialEq` where `Value` allows.

- [ ] **Step 1: Write the failing tests** — `model.rs` with the types above and `parse`/`resolve` bodied `todo!()`, plus:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const PETS: &str = r##"openapi: 3.0.3
info:
  title: Petstore
  version: 1.0.0
  description: All **the** pets.
servers:
  - url: https://api.example.com/v1
tags:
  - name: store
  - name: pets
    description: Pets and more
paths:
  /pets/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema: {type: string}
      - name: trace
        in: header
        schema: {type: string}
    get:
      tags: [pets]
      summary: Info for a pet
      operationId: showPet
      parameters:
        - name: trace
          in: header
          description: overridden
          schema: {type: integer}
      responses:
        200:
          description: A pet
          content:
            application/json:
              schema: {$ref: '#/components/schemas/Pet'}
        default:
          $ref: '#/components/responses/Error'
    delete:
      tags: [pets, store]
      deprecated: true
      responses:
        '204': {description: gone}
  /health:
    get:
      summary: Up?
      responses:
        '200': {description: ok}
  /orders:
    post:
      tags: [store]
      requestBody:
        $ref: '#/components/requestBodies/Order'
      responses:
        '201': {description: made}
components:
  schemas:
    Pet:
      type: object
      properties: {id: {type: integer}}
  responses:
    Error:
      description: Something broke
  requestBodies:
    Order:
      description: An order
      content:
        application/json:
          schema: {type: object}
"##;

    #[test]
    fn header_and_servers() {
        let api = parse(PETS, false).unwrap();
        assert_eq!((api.title.as_str(), api.version.as_str()), ("Petstore", "1.0.0"));
        assert_eq!(api.description.as_deref(), Some("All **the** pets."));
        assert_eq!(api.servers, ["https://api.example.com/v1"]);
        assert_eq!(api.schemas, ["Pet"]);
        assert!(api.missing.is_empty());
    }

    #[test]
    fn operations_group_by_first_tag_in_tag_order() {
        let api = parse(PETS, false).unwrap();
        let groups: Vec<(&str, Vec<String>)> = api
            .groups
            .iter()
            .map(|g| (g.name.as_str(), g.operations.iter().map(|o| o.key()).collect()))
            .collect();
        assert_eq!(
            groups,
            [
                ("store", vec!["POST /orders".to_string()]),
                ("pets", vec!["GET /pets/{id}".into(), "DELETE /pets/{id}".into()]),
                ("default", vec!["GET /health".into()]),
            ]
        );
        assert_eq!(api.groups[1].description.as_deref(), Some("Pets and more"));
        assert!(api.groups[1].operations[1].deprecated);
    }

    #[test]
    fn path_parameters_apply_and_the_operation_wins() {
        let api = parse(PETS, false).unwrap();
        let get = &api.groups[1].operations[0];
        let params: Vec<(&str, &str, bool, Option<&str>)> = get
            .parameters
            .iter()
            .map(|p| (p.name.as_str(), p.location.as_str(), p.required, p.description.as_deref()))
            .collect();
        assert_eq!(
            params,
            [("id", "path", true, None), ("trace", "header", false, Some("overridden"))]
        );
        assert_eq!(get.parameters[1].schema["type"], "integer");
        assert_eq!(get.operation_id.as_deref(), Some("showPet"));
    }

    #[test]
    fn unquoted_status_codes_are_keys() {
        let api = parse(PETS, false).unwrap();
        let statuses: Vec<&str> = api.groups[1].operations[0]
            .responses
            .iter()
            .map(|r| r.status.as_str())
            .collect();
        assert_eq!(statuses, ["200", "default"]);
    }

    #[test]
    fn referenced_responses_and_bodies_are_followed() {
        let api = parse(PETS, false).unwrap();
        let get = &api.groups[1].operations[0];
        assert_eq!(get.responses[1].description.as_deref(), Some("Something broke"));
        assert_eq!(
            get.responses[0].content[0],
            ("application/json".to_string(), serde_json::json!({"$ref": "#/components/schemas/Pet"})),
            "schemas stay as refs: followed when shown"
        );
        let post = &api.groups[0].operations[0];
        let body = post.request_body.as_ref().unwrap();
        assert_eq!(body.description.as_deref(), Some("An order"));
        assert_eq!(body.content[0].0, "application/json");
    }

    #[test]
    fn json_specs_read_the_same() {
        let json = r#"{"openapi": "3.1.0", "info": {"title": "J", "version": "2"},
            "paths": {"/a": {"get": {"responses": {"200": {"description": "ok"}}}}}}"#;
        let api = parse(json, true).unwrap();
        assert_eq!(api.title, "J");
        assert_eq!(api.groups[0].operations[0].key(), "GET /a");
    }

    #[test]
    fn errors_carry_their_line() {
        let bad = "openapi: 3.0.0\ninfo:\n  title: x\n   version: [\n";
        let err = parse(bad, false).unwrap_err();
        assert!(err.line.is_some_and(|l| (3..=5).contains(&l)), "{err:?}");
        let err = parse("{\n\"openapi\": \"3.0.0\",\n,}", true).unwrap_err();
        assert_eq!(err.line, Some(3), "{err:?}");
    }

    #[test]
    fn a_spec_missing_info_or_paths_renders_what_it_has() {
        let api = parse("openapi: 3.0.0\ncomponents:\n  schemas:\n    A: {type: string}\n", false)
            .unwrap();
        assert_eq!(api.missing, ["info", "paths"]);
        assert_eq!(api.schemas, ["A"]);
        assert!(api.groups.is_empty());
    }

    #[test]
    fn pointers_unescape() {
        let root = serde_json::json!({"paths": {"/a/b": {"x~y": 1}}});
        assert_eq!(resolve(&root, "#/paths/~1a~1b/x~0y"), Some(&serde_json::json!(1)));
        assert_eq!(resolve(&root, "#/nope"), None);
        assert_eq!(resolve(&root, "other.yaml#/x"), None, "not a local ref");
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib -- openapi::model`
Expected: FAIL — `not yet implemented` in each.

- [ ] **Step 3: Implement `model.rs`**

```rust
//! An OpenAPI 3.x spec as the view needs it (spec §7.8): the header, operations grouped by
//! their first tag, and the schema names. Schemas stay as `serde_json::Value`s and are read
//! a level at a time when shown (see `schema`), so recursive ones cost nothing here.
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    /// 1-based, when the parser knows it.
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Api {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
    pub servers: Vec<String>,
    pub groups: Vec<TagGroup>,
    pub schemas: Vec<String>,
    /// Top-level sections the spec lacks (`info`, `paths`), for the banner.
    pub missing: Vec<&'static str>,
    pub root: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TagGroup {
    pub name: String,
    pub description: Option<String>,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub operation_id: Option<String>,
    pub deprecated: bool,
    pub parameters: Vec<Parameter>,
    pub request_body: Option<Body>,
    pub responses: Vec<Response>,
}

impl Operation {
    /// `GET /pets`: what keeps a row open across reparses.
    pub fn key(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub location: String,
    pub required: bool,
    pub description: Option<String>,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub description: Option<String>,
    pub content: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: String,
    pub description: Option<String>,
    pub content: Vec<(String, Value)>,
}

const METHODS: [&str; 8] = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

/// A local JSON pointer (`#/a/b`), with `~1` and `~0` unescaped.
pub fn resolve<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let path = pointer.strip_prefix('#')?;
    if path.is_empty() {
        return Some(root);
    }
    path.strip_prefix('/')?
        .split('/')
        .map(|p| p.replace("~1", "/").replace("~0", "~"))
        .try_fold(root, |v, key| match v {
            Value::Object(m) => m.get(&key),
            Value::Array(a) => a.get(key.parse::<usize>().ok()?),
            _ => None,
        })
}

/// An object that may be a `$ref` to one (a parameter, response or request body): the
/// object itself, followed locally; an unresolvable ref gives an empty object.
fn deref<'a>(root: &'a Value, v: &'a Value) -> &'a Value {
    static EMPTY: Value = Value::Null;
    let mut v = v;
    for _ in 0..8 {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => v = resolve(root, r).unwrap_or(&EMPTY),
            None => break,
        }
    }
    v
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn content(v: &Value) -> Vec<(String, Value)> {
    v.get("content")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(media, c)| (media.clone(), c.get("schema").cloned().unwrap_or(Value::Null)))
                .collect()
        })
        .unwrap_or_default()
}

fn parameters(root: &Value, list: Option<&Value>) -> Vec<Parameter> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|p| deref(root, p))
        .filter_map(|p| {
            Some(Parameter {
                name: text(p, "name")?,
                location: text(p, "in").unwrap_or_default(),
                required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
                description: text(p, "description"),
                schema: p.get("schema").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

pub fn parse(text_in: &str, json: bool) -> Result<Api, ParseError> {
    let src = text_in.trim_start_matches('\u{feff}');
    let root: Value = if json {
        serde_json::from_str(src).map_err(|e| ParseError {
            message: e.to_string(),
            line: Some(e.line()),
        })?
    } else {
        serde_saphyr::from_str(src).map_err(|e| ParseError {
            message: e.to_string(),
            line: e.location().map(|l| l.line() as usize),
        })?
    };
    let empty = Map::new();
    let info = root.get("info");
    let mut missing = Vec::new();
    if info.is_none() {
        missing.push("info");
    }
    if root.get("paths").is_none() {
        missing.push("paths");
    }
    let servers = root
        .get("servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| text(s, "url"))
        .collect();
    // Groups: the `tags` list's order first, then tags as operations name them.
    let mut groups: Vec<TagGroup> = root
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(TagGroup {
                name: text(t, "name")?,
                description: text(t, "description"),
                operations: Vec::new(),
            })
        })
        .collect();
    let mut untagged = Vec::new();
    let paths = root.get("paths").and_then(Value::as_object).unwrap_or(&empty);
    for (path, item) in paths {
        let item = deref(&root, item);
        let shared = parameters(&root, item.get("parameters"));
        for method in METHODS {
            let Some(op) = item.get(method) else { continue };
            let mut params = shared.clone();
            for own in parameters(&root, op.get("parameters")) {
                params.retain(|p| !(p.name == own.name && p.location == own.location));
                params.push(own);
            }
            // Keep path-level ones first, in their order, with overrides in place.
            params.sort_by_key(|p| {
                shared
                    .iter()
                    .position(|s| s.name == p.name && s.location == p.location)
                    .unwrap_or(usize::MAX)
            });
            let responses = op
                .get("responses")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(status, r)| {
                            let r = deref(&root, r);
                            Response {
                                status: status.clone(),
                                description: text(r, "description"),
                                content: content(r),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let request_body = op.get("requestBody").map(|b| {
                let b = deref(&root, b);
                Body {
                    description: text(b, "description"),
                    content: content(b),
                }
            });
            let operation = Operation {
                method: method.to_uppercase(),
                path: path.clone(),
                summary: text(op, "summary"),
                description: text(op, "description"),
                operation_id: text(op, "operationId"),
                deprecated: op.get("deprecated").and_then(Value::as_bool).unwrap_or(false),
                parameters: params,
                request_body,
                responses,
            };
            let tag = op
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|t| t.first())
                .and_then(Value::as_str);
            match tag {
                None => untagged.push(operation),
                Some(tag) => match groups.iter_mut().find(|g| g.name == tag) {
                    Some(g) => g.operations.push(operation),
                    None => groups.push(TagGroup {
                        name: tag.to_string(),
                        description: None,
                        operations: vec![operation],
                    }),
                },
            }
        }
    }
    groups.retain(|g| !g.operations.is_empty());
    if !untagged.is_empty() {
        groups.push(TagGroup {
            name: "default".into(),
            description: None,
            operations: untagged,
        });
    }
    let schemas = root
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    Ok(Api {
        title: info.and_then(|i| text(i, "title")).unwrap_or_default(),
        version: info.and_then(|i| text(i, "version")).unwrap_or_default(),
        description: info.and_then(|i| text(i, "description")),
        servers,
        groups,
        schemas,
        missing,
        root,
    })
}
```

Note for the executor: `groups.retain(|g| !g.operations.is_empty())` drops tags listed in `tags:` that no operation uses — correct for the view. If `serde_saphyr::from_str::<serde_json::Value>` rejects unquoted integer keys (`unquoted_status_codes_are_keys` fails with a type error), parse into `serde_saphyr`'s own value type or a custom `Deserialize` that stringifies scalar keys, and ledger the ruling. If `Error::location()` has a different name in 1.3.0, use the crate source (`src/de/error.rs`).

In `mod.rs`: `pub mod model;` and `pub use model::{Api, Body, Operation, Parameter, ParseError, Response, TagGroup, parse, resolve};`. `Cargo.toml` (hydraui): `serde-saphyr = "1.3"` and `serde_json.workspace = true`.

- [ ] **Step 4: Run them**

Run: `cargo test -p hydraui --lib -- openapi::model`
Expected: PASS (9).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add Cargo.lock crates/hydraui
git commit -m "feat: hydraui reads OpenAPI specs into tag groups and operations"
```

---

### Task 3: Schemas, one level at a time; the filter

**Files:**
- Create: `crates/hydraui/src/openapi/schema.rs`
- Modify: `crates/hydraui/src/openapi/mod.rs` (`pub mod schema;` + re-exports; `pub fn matches`)
- Test: in `schema.rs`, `mod.rs`

**Interfaces:**
- Consumes: Task 2 (`resolve`, `Operation`).
- Produces:
  - `pub struct Level { pub label: String, pub kind: Kind }` (`Debug, Clone, PartialEq`)
  - `pub enum Kind { Leaf, Object(Vec<Prop>), OneOf(Vec<(String, serde_json::Value)>), Cycle(String), Missing(String), External { file: String, pointer: String } }`
  - `pub struct Prop { pub name: String, pub required: bool, pub schema: serde_json::Value }`
  - `pub fn level(root: &serde_json::Value, schema: &serde_json::Value, above: &[String]) -> Level` — `above` is the chain of `$ref`s already open above (for cycles); the label is the type text (`Pet`, `Pet[]`, `string?`, `string (date-time)`, `"a" | "b"`, `object`)
  - `pub fn ref_name(r: &str) -> &str` — last pointer segment (`#/components/schemas/Pet` → `Pet`)
  - `mod.rs`: `pub fn matches(op: &Operation, filter: &str) -> bool` — case-insensitive substring of method, path, summary or operationId; empty filter matches

- [ ] **Step 1: Write the failing tests** — `schema.rs` with the types and `level`/`ref_name` bodied `todo!()`, and `matches` in `mod.rs` bodied `todo!()`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root() -> Value {
        json!({"components": {"schemas": {
            "Pet": {"type": "object", "required": ["id"],
                    "properties": {"id": {"type": "integer", "format": "int64"},
                                   "tag": {"type": "string"},
                                   "owner": {"$ref": "#/components/schemas/Owner"}}},
            "Owner": {"type": "object", "properties": {"name": {"type": "string"}}},
            "Node": {"type": "object", "properties": {
                "children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}}},
            "Named": {"allOf": [{"$ref": "#/components/schemas/Owner"},
                                {"type": "object", "required": ["nick"],
                                 "properties": {"nick": {"type": "string"}}}]}
        }}})
    }

    fn names(l: &Level) -> Vec<(String, bool)> {
        match &l.kind {
            Kind::Object(props) => props.iter().map(|p| (p.name.clone(), p.required)).collect(),
            other => panic!("not an object: {other:?}"),
        }
    }

    #[test]
    fn a_ref_reads_as_its_name_and_opens_to_its_properties() {
        let r = root();
        let l = level(&r, &json!({"$ref": "#/components/schemas/Pet"}), &[]);
        assert_eq!(l.label, "Pet");
        assert_eq!(names(&l), [("id".into(), true), ("tag".into(), false), ("owner".into(), false)]);
        let Kind::Object(props) = &l.kind else { unreachable!() };
        let id = level(&r, &props[0].schema, &[]);
        assert_eq!((id.label.as_str(), &id.kind), ("integer (int64)", &Kind::Leaf));
    }

    #[test]
    fn arrays_enums_and_nullable_types() {
        let r = root();
        assert_eq!(
            level(&r, &json!({"type": "array", "items": {"$ref": "#/components/schemas/Pet"}}), &[]).label,
            "Pet[]"
        );
        assert_eq!(level(&r, &json!({"type": ["string", "null"]}), &[]).label, "string?");
        assert_eq!(level(&r, &json!({"type": "string", "nullable": true}), &[]).label, "string?");
        assert_eq!(level(&r, &json!({"enum": ["a", "b"]}), &[]).label, r#""a" | "b""#);
        assert_eq!(level(&r, &json!({}), &[]).label, "any");
    }

    #[test]
    fn a_schema_open_above_shows_a_cycle() {
        let r = root();
        let node = json!({"$ref": "#/components/schemas/Node"});
        let top = level(&r, &node, &[]);
        let Kind::Object(props) = &top.kind else { panic!() };
        // children: Node[] — the array's items ref Node, which is open above.
        let above = vec!["#/components/schemas/Node".to_string()];
        let child = level(&r, &props[0].schema, &above);
        assert_eq!(child.label, "Node[]");
        assert_eq!(child.kind, Kind::Cycle("Node".into()));
    }

    #[test]
    fn all_of_merges_into_one_object() {
        let r = root();
        let l = level(&r, &json!({"$ref": "#/components/schemas/Named"}), &[]);
        assert_eq!(l.label, "Named");
        assert_eq!(names(&l), [("name".into(), false), ("nick".into(), true)]);
    }

    #[test]
    fn one_of_lists_its_choices() {
        let r = root();
        let l = level(
            &r,
            &json!({"oneOf": [{"$ref": "#/components/schemas/Pet"}, {"type": "string"}]}),
            &[],
        );
        assert_eq!(l.label, "one of: Pet | string");
        let Kind::OneOf(choices) = &l.kind else { panic!("{l:?}") };
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].0, "Pet");
    }

    #[test]
    fn missing_and_external_refs() {
        let r = root();
        let l = level(&r, &json!({"$ref": "#/components/schemas/Nope"}), &[]);
        assert_eq!(l.kind, Kind::Missing("#/components/schemas/Nope".into()));
        let l = level(&r, &json!({"$ref": "common.yaml#/components/schemas/Id"}), &[]);
        assert_eq!(
            l.kind,
            Kind::External { file: "common.yaml".into(), pointer: "#/components/schemas/Id".into() }
        );
        assert_eq!(l.label, "Id");
        assert_eq!(ref_name("#/components/schemas/Pet"), "Pet");
    }
}
```

In `mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_matches_method_path_summary_and_id() {
        let op = Operation {
            method: "GET".into(),
            path: "/pets/{id}".into(),
            summary: Some("Info for a pet".into()),
            description: None,
            operation_id: Some("showPetById".into()),
            deprecated: false,
            parameters: vec![],
            request_body: None,
            responses: vec![],
        };
        for f in ["", "get", "PETS", "info for", "showpet"] {
            assert!(matches(&op, f), "{f}");
        }
        assert!(!matches(&op, "orders"));
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib -- openapi::schema openapi::tests`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Implement `schema.rs` and `matches`**

```rust
//! A schema one level at a time (spec §7.8): what the view shows for it (its label) and what
//! opening it reveals (properties, choices, a cycle, a missing or external ref).
use serde_json::{Map, Value};

use super::model::resolve;

#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub label: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Leaf,
    Object(Vec<Prop>),
    OneOf(Vec<(String, Value)>),
    /// A schema already open above: shows `↻ name`, never expands.
    Cycle(String),
    /// A local ref that resolves to nothing.
    Missing(String),
    /// A ref into another file: a label that opens it.
    External { file: String, pointer: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub name: String,
    pub required: bool,
    pub schema: Value,
}

pub fn ref_name(r: &str) -> &str {
    r.rsplit('/').next().unwrap_or(r)
}

fn type_text(t: &Value, schema: &Value) -> String {
    let base = match t {
        Value::String(s) => s.clone(),
        Value::Array(list) => {
            let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            let null = names.contains(&"null");
            let rest: Vec<&str> = names.into_iter().filter(|n| *n != "null").collect();
            let joined = if rest.is_empty() { "null".to_string() } else { rest.join(" | ") };
            return if null && !rest.is_empty() { format!("{joined}?") } else { joined };
        }
        _ => "any".into(),
    };
    let base = match schema.get("format").and_then(Value::as_str) {
        Some(f) => format!("{base} ({f})"),
        None => base,
    };
    if schema.get("nullable").and_then(Value::as_bool) == Some(true) {
        format!("{base}?")
    } else {
        base
    }
}

/// The text a schema reads as, without opening it.
fn label(root: &Value, schema: &Value) -> String {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        return ref_name(r).to_string();
    }
    if let Some(choices) = schema.get("oneOf").or_else(|| schema.get("anyOf")).and_then(Value::as_array) {
        let names: Vec<String> = choices.iter().map(|c| label(root, c)).collect();
        return format!("one of: {}", names.join(" | "));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" | ");
    }
    match schema.get("type") {
        Some(Value::String(t)) if t == "array" => {
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            format!("{}[]", label(root, &items))
        }
        Some(t) => type_text(t, schema),
        None if schema.get("properties").is_some() || schema.get("allOf").is_some() => "object".into(),
        None => "any".into(),
    }
}

/// `allOf` parts (refs followed) merged into one object's properties and required list.
fn merged(root: &Value, schema: &Value, out: &mut Map<String, Value>, required: &mut Vec<String>, depth: u8) {
    if depth > 16 {
        return;
    }
    let schema = match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => match resolve(root, r) {
            Some(s) => s,
            None => return,
        },
        None => schema,
    };
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (k, v) in props {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Some(req) = schema.get("required").and_then(Value::as_array) {
        required.extend(req.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
        for p in parts {
            merged(root, p, out, required, depth + 1);
        }
    }
}

pub fn level(root: &Value, schema: &Value, above: &[String]) -> Level {
    let text = label(root, schema);
    // Arrays open to their items.
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        let items = schema.get("items").cloned().unwrap_or(Value::Null);
        return Level { label: text, kind: level(root, &items, above).kind };
    }
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        if let Some((file, pointer)) = r.split_once('#').filter(|(f, _)| !f.is_empty()) {
            return Level {
                label: text,
                kind: Kind::External { file: file.to_string(), pointer: format!("#{pointer}") },
            };
        }
        if above.iter().any(|a| a == r) {
            return Level { label: text, kind: Kind::Cycle(ref_name(r).to_string()) };
        }
        return match resolve(root, r) {
            Some(target) => Level { label: text, kind: level(root, target, above).kind },
            None => Level { label: text, kind: Kind::Missing(r.to_string()) },
        };
    }
    if let Some(choices) = schema.get("oneOf").or_else(|| schema.get("anyOf")).and_then(Value::as_array) {
        let kind = Kind::OneOf(choices.iter().map(|c| (label(root, c), c.clone())).collect());
        return Level { label: text, kind };
    }
    let mut props = Map::new();
    let mut required = Vec::new();
    merged(root, schema, &mut props, &mut required, 0);
    if props.is_empty() {
        return Level { label: text, kind: Kind::Leaf };
    }
    let props = props
        .into_iter()
        .map(|(name, schema)| Prop { required: required.contains(&name), name, schema })
        .collect();
    Level { label: text, kind: Kind::Object(props) }
}
```

In `mod.rs`:

```rust
pub mod schema;

pub use schema::{Kind, Level, Prop, level, ref_name};

/// The filter box: method, path, summary or operationId, any case; empty matches all.
pub fn matches(op: &Operation, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty()
        || [Some(&op.method), Some(&op.path), op.summary.as_ref(), op.operation_id.as_ref()]
            .into_iter()
            .flatten()
            .any(|s| s.to_lowercase().contains(&f))
}
```

- [ ] **Step 4: Run them**

Run: `cargo test -p hydraui --lib -- openapi`
Expected: PASS (all openapi tests: 4 + 9 + 6 + 1).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: hydraui reads OpenAPI schemas a level at a time, with refs and cycles"
```

---

### Task 4: The API view in the file pane

**Files:**
- Create: `crates/hydraui/src/openapi_view.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod openapi_view;`), `crates/hydraui/src/file_pane.rs` (Body::Text gains `api`, header switch and Swagger note, Rendered body, events)
- Test: in `openapi_view.rs` (the non-UI state: `ViewState`)

**Interfaces:**
- Consumes: Tasks 1–3 (`spec_kind`, `SpecKind`, `parse`, `Api`, `ParseError`, `level`, `Kind`, `matches`, `Operation::key`).
- Produces:
  - `pub struct ViewState { pub api: Option<Api>, pub error: Option<ParseError>, pub open: HashSet<String> }` with `pub fn apply(&mut self, result: Result<Api, ParseError>)` — Ok replaces `api` and clears `error` (keeps `open`); Err sets `error` and keeps the last `api`
  - `pub enum ApiViewEvent { GoToLine(usize), OpenFile(PathBuf) }`
  - `pub struct ApiView` (entity; `EventEmitter<ApiViewEvent>`), `ApiView::new(text: String, json: bool, dir: PathBuf, window, cx)`, `ApiView::set_text(&mut self, text: String, cx)` (debounced 300 ms, parse in `background_spawn`)
  - Open keys: `"tag:<name>"` (stored when **collapsed**, so groups start open), `"op:<METHOD path>"`, `"schemas"`, `"s:<owner key>/<prop path>"` for schema nodes

- [ ] **Step 1: Write the failing test** — `openapi_view.rs` with `ViewState` and `apply` bodied `todo!()`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::parse;

    const ONE: &str = "openapi: 3.0.0\ninfo: {title: A, version: '1'}\npaths:\n  /a:\n    get:\n      responses: {'200': {description: ok}}\n";

    #[test]
    fn a_failed_parse_keeps_the_last_good_api() {
        let mut s = ViewState::default();
        s.apply(parse(ONE, false));
        s.open.insert("op:GET /a".into());
        s.apply(parse("openapi: 3.0.0\ninfo: [\n", false));
        assert!(s.error.is_some());
        assert_eq!(s.api.as_ref().unwrap().title, "A", "last good kept");
        s.apply(parse(&ONE.replace("title: A", "title: B"), false));
        assert!(s.error.is_none());
        assert_eq!(s.api.as_ref().unwrap().title, "B");
        assert!(s.open.contains("op:GET /a"), "open rows stay open");
    }
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p hydraui --lib -- openapi_view`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Implement `ViewState::apply`**

```rust
#[derive(Default)]
pub struct ViewState {
    pub api: Option<Api>,
    pub error: Option<ParseError>,
    pub open: HashSet<String>,
}

impl ViewState {
    /// A parse result: a good one replaces the API; a failed one keeps the last good API
    /// (shown greyed) beside the error. What is open survives either way.
    pub fn apply(&mut self, result: Result<Api, ParseError>) {
        match result {
            Ok(api) => {
                self.api = Some(api);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}
```

Run: `cargo test -p hydraui --lib -- openapi_view` — Expected: PASS.

- [ ] **Step 4: The view** — the rest of `openapi_view.rs`:

```rust
//! OpenAPI specs as API docs (spec §7.8): a read-only view of an `Api` in the file pane's
//! Rendered mode. What is open is kept here; schema trees are built only when opened.
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, EventEmitter, Hsla, SharedString, Task, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;

use crate::openapi::{Api, Kind, Operation, ParseError, level, matches, parse};

pub enum ApiViewEvent {
    GoToLine(usize),
    OpenFile(PathBuf),
}

pub struct ApiView {
    state: ViewState,
    json: bool,
    /// The spec's folder, for refs into other files.
    dir: PathBuf,
    filter: Entity<InputState>,
    _filter_sub: gpui::Subscription,
    pending: Option<Task<()>>,
}

impl EventEmitter<ApiViewEvent> for ApiView {}

impl ApiView {
    pub fn new(text: String, json: bool, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter operations"));
        let sub = cx.subscribe(&filter, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut view = Self {
            state: ViewState::default(),
            json,
            dir,
            filter,
            _filter_sub: sub,
            pending: None,
        };
        view.parse_now(text, cx);
        view
    }

    fn parse_now(&mut self, text: String, cx: &mut Context<Self>) {
        let json = self.json;
        self.pending = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { parse(&text, json) }).await;
            let _ = this.update(cx, |v, cx| {
                v.state.apply(result);
                cx.notify();
            });
        }));
    }

    /// The text changed (unsaved edits): parse again once typing stops for 300 ms.
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        let json = self.json;
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let result = cx.background_spawn(async move { parse(&text, json) }).await;
            let _ = this.update(cx, |v, cx| {
                v.state.apply(result);
                cx.notify();
            });
        }));
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.state.open.remove(&key) {
            self.state.open.insert(key);
        }
        cx.notify();
    }

    fn is_open(&self, key: &str) -> bool {
        self.state.open.contains(key)
    }
}
```

Rendering — methods on `ApiView` (keep each small; all colours from `cx.theme()`):

```rust
fn method_colour(method: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    match method {
        "GET" => t.info,
        "POST" => t.success,
        "PUT" => t.warning,
        "PATCH" => gpui::hsla(174. / 360., 0.6, 0.45, 1.),
        "DELETE" => t.danger,
        _ => t.muted_foreground,
    }
}

fn status_colour(status: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    match status.as_bytes().first() {
        Some(b'2') => t.success,
        Some(b'4') => t.warning,
        Some(b'5') => t.danger,
        _ => t.muted_foreground,
    }
}

impl ApiView {
    /// A schema node: its label, and when opened its next level. `key` names it for `open`;
    /// `above` is the refs open above it (cycles).
    fn render_schema(&self, key: String, name: Option<(String, bool)>, schema: &Value, above: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        let Some(api) = &self.state.api else { return div().into_any_element() };
        let lvl = level(&api.root, schema, &above);
        let muted = cx.theme().muted_foreground;
        let open = self.is_open(&key);
        let expandable = matches!(lvl.kind, Kind::Object(_) | Kind::OneOf(_));
        let head = h_flex()
            .id(SharedString::from(key.clone()))
            .gap_2()
            .when(expandable, |d| d.cursor_pointer())
            .child(div().w(px(12.)).text_color(muted).child(if !expandable { " " } else if open { "▾" } else { "▸" }))
            .when_some(name, |d, (n, required)| {
                d.child(div().font_family("monospace").child(n))
                    .when(required, |d| d.child(div().text_color(cx.theme().danger).child("*")))
            })
            .child(div().text_color(muted).child(match &lvl.kind {
                Kind::Cycle(n) => format!("↻ {n}"),
                Kind::Missing(r) => format!("⚠ {r} (missing)"),
                _ => lvl.label.clone(),
            }));
        let head = match &lvl.kind {
            Kind::External { file, .. } => {
                let path = self.dir.join(file);
                head.cursor_pointer()
                    .text_decoration_1()
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ApiViewEvent::OpenFile(path.clone()))))
            }
            _ if expandable => {
                let k = key.clone();
                head.on_click(cx.listener(move |v, _, _, cx| v.toggle(k.clone(), cx)))
            }
            _ => head,
        };
        let mut node = v_flex().child(head);
        if open {
            let mut above = above;
            if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
                above.push(r.to_string());
            } else if let Some(r) = schema.pointer("/items/$ref").and_then(Value::as_str) {
                above.push(r.to_string());
            }
            let children: Vec<AnyElement> = match &lvl.kind {
                Kind::Object(props) => props
                    .iter()
                    .map(|p| self.render_schema(format!("{key}/{}", p.name), Some((p.name.clone(), p.required)), &p.schema, above.clone(), cx))
                    .collect(),
                Kind::OneOf(choices) => choices
                    .iter()
                    .enumerate()
                    .map(|(i, (_, s))| self.render_schema(format!("{key}/{i}"), None, s, above.clone(), cx))
                    .collect(),
                _ => Vec::new(),
            };
            node = node.child(v_flex().pl(px(16.)).children(children));
        }
        node.into_any_element()
    }

    fn render_operation(&self, op: &Operation, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("op:{}", op.key());
        let open = self.is_open(&key);
        let muted = cx.theme().muted_foreground;
        let row = h_flex()
            .id(SharedString::from(key.clone()))
            .gap_3()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(cx.theme().accent))
            .on_click(cx.listener({ let k = key.clone(); move |v, _, _, cx| v.toggle(k.clone(), cx) }))
            .child(
                div()
                    .w(px(64.))
                    .flex_none()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(method_colour(&op.method, cx))
                    .child(op.method.clone()),
            )
            .child(div().font_family("monospace").when(op.deprecated, |d| d.line_through()).child(op.path.clone()))
            .child(div().text_color(muted).truncate().child(op.summary.clone().unwrap_or_default()));
        let mut block = v_flex().child(row);
        if open {
            let mut detail = v_flex().pl(px(76.)).pb_2().gap_2();
            if let Some(d) = &op.description {
                detail = detail.child(div().text_sm().child(d.clone()));
            }
            if !op.parameters.is_empty() {
                detail = detail.child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Parameters"));
                for p in &op.parameters {
                    let pkey = format!("{key}/param/{}/{}", p.location, p.name);
                    detail = detail.child(
                        h_flex()
                            .gap_3()
                            .child(div().w(px(70.)).text_color(muted).child(p.location.clone()))
                            .child(self.render_schema(pkey, Some((p.name.clone(), p.required)), &p.schema, Vec::new(), cx))
                            .child(div().text_color(muted).child(p.description.clone().unwrap_or_default())),
                    );
                }
            }
            if let Some(body) = &op.request_body {
                detail = detail.child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Request body"));
                if let Some(d) = &body.description {
                    detail = detail.child(div().text_color(muted).child(d.clone()));
                }
                for (media, schema) in &body.content {
                    detail = detail
                        .child(div().text_sm().text_color(muted).child(media.clone()))
                        .child(self.render_schema(format!("{key}/body/{media}"), None, schema, Vec::new(), cx));
                }
            }
            if !op.responses.is_empty() {
                detail = detail.child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Responses"));
                for r in &op.responses {
                    detail = detail.child(
                        h_flex()
                            .gap_3()
                            .child(div().w(px(56.)).text_color(status_colour(&r.status, cx)).child(r.status.clone()))
                            .child(div().text_color(muted).child(r.description.clone().unwrap_or_default())),
                    );
                    for (media, schema) in &r.content {
                        detail = detail.child(
                            v_flex()
                                .pl(px(68.))
                                .child(div().text_sm().text_color(muted).child(media.clone()))
                                .child(self.render_schema(format!("{key}/resp/{}/{media}", r.status), None, schema, Vec::new(), cx)),
                        );
                    }
                }
            }
            block = block.child(detail);
        }
        block.into_any_element()
    }
}

impl Render for ApiView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, warning) = (theme.muted_foreground, theme.border, theme.warning);
        let mut page = v_flex().id("api").size_full().overflow_y_scroll().p_3().gap_3();
        if let Some(e) = &self.state.error {
            let line = e.line;
            page = page.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(warning)
                    .child(match line {
                        Some(l) => format!("Line {l}: {}", e.message),
                        None => e.message.clone(),
                    })
                    .when_some(line, |d, l| {
                        d.child(
                            Button::new("go-to-line")
                                .label("Go to line")
                                .xsmall()
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ApiViewEvent::GoToLine(l)))),
                        )
                    }),
            );
        }
        let Some(api) = self.state.api.clone() else { return page };
        if !api.missing.is_empty() {
            page = page.child(div().text_sm().text_color(warning).child(format!("This spec has no {}", api.missing.join(" or "))));
        }
        let greyed = self.state.error.is_some();
        let filter = self.filter.read(cx).value().to_string();
        let mut content = v_flex().gap_3().when(greyed, |d| d.opacity(0.5));
        content = content.child(
            h_flex()
                .gap_2()
                .items_baseline()
                .child(div().text_xl().child(api.title.clone()))
                .child(div().text_color(muted).child(api.version.clone())),
        );
        if let Some(d) = &api.description {
            content = content.child(
                gpui_base::TextView::markdown("api-description", d.clone())
                    .selectable(true)
                    .on_link_click(|url, _, _, cx| {
                        if crate::file_doc::link_opens(url) {
                            cx.open_url(url);
                        }
                    }),
            );
        }
        for s in &api.servers {
            content = content.child(div().text_sm().text_color(muted).font_family("monospace").child(s.clone()));
        }
        content = content.child(Input::new(&self.filter).small());
        for g in &api.groups {
            let ops: Vec<&Operation> = g.operations.iter().filter(|o| matches(o, &filter)).collect();
            if ops.is_empty() {
                continue;
            }
            let tkey = format!("tag:{}", g.name);
            let collapsed = self.is_open(&tkey);
            let header = h_flex()
                .id(SharedString::from(tkey.clone()))
                .gap_2()
                .cursor_pointer()
                .border_b_1()
                .border_color(border)
                .on_click(cx.listener({ let k = tkey.clone(); move |v, _, _, cx| v.toggle(k.clone(), cx) }))
                .child(div().text_color(muted).child(if collapsed { "▸" } else { "▾" }))
                .child(div().text_lg().child(g.name.clone()))
                .child(div().text_sm().text_color(muted).truncate().child(g.description.clone().unwrap_or_default()));
            let mut group = v_flex().child(header);
            if !collapsed {
                for op in ops {
                    group = group.child(self.render_operation(op, cx));
                }
            }
            content = content.child(group);
        }
        if !api.schemas.is_empty() {
            let open = self.is_open("schemas");
            content = content.child(
                h_flex()
                    .id("schemas")
                    .gap_2()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(border)
                    .on_click(cx.listener(|v, _, _, cx| v.toggle("schemas".into(), cx)))
                    .child(div().text_color(muted).child(if open { "▾" } else { "▸" }))
                    .child(div().text_lg().child("Schemas")),
            );
            if open {
                for name in &api.schemas {
                    let r = serde_json::json!({ "$ref": format!("#/components/schemas/{}", name.replace('~', "~0").replace('/', "~1")) });
                    content = content.child(self.render_schema(format!("schema:{name}"), Some((name.clone(), false)), &r, Vec::new(), cx));
                }
            }
        }
        page.child(content)
    }
}
```

Executor notes: gpui-component / gpui method names that may differ in this version — `line_through()`, `text_decoration_1()`, `font_weight`, `items_baseline`, `opacity`, `hover`, `truncate` — check against existing uses in this crate (`grep -rn` under `crates/hydraui/src`) or the crate source, and adjust; no ruling needed for a renamed styling call. `TextView::markdown` takes an id and text as in `file_pane.rs`.

- [ ] **Step 5: Wire it into the file pane** (`file_pane.rs`):

1. `Body::Text` gains `api: Option<Entity<ApiView>>` and `swagger: bool`; `_api_sub: Option<Subscription>`.
2. `text_body`: after building the editor,

```rust
        let kind = crate::openapi::spec_kind(path, &doc.text);
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let json = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        let (api, api_sub) = match kind {
            Some(crate::openapi::SpecKind::OpenApi3) => {
                let text = doc.text.clone();
                let view = cx.new(|cx| ApiView::new(text, json, dir, window, cx));
                let key = path.to_path_buf();
                let sub = cx.subscribe_in(&view, window, move |pane, _, ev: &ApiViewEvent, window, cx| match ev {
                    ApiViewEvent::GoToLine(line) => pane.open_at(key.clone(), *line, 1, window, cx),
                    ApiViewEvent::OpenFile(p) => pane.open(p.clone(), window, cx),
                });
                (Some(view), Some(sub))
            }
            _ => (None, None),
        };
        let view = if file_doc::is_markdown(path) || api.is_some() { View::Rendered } else { View::Raw };
```

   and `swagger: kind == Some(SpecKind::Swagger2)`.
3. `edited`: after recomputing `dirty`, if the tab has an `api`, `api.update(cx, |v, cx| v.set_text(editor.read(cx).value().to_string(), cx))`.
4. `reload` (changed on disk, no edits): also `set_text` the reloaded text (find where `reload` sets the editor value and add the same call).
5. Header: the Raw / Rendered button shows when `file_doc::is_markdown(&t.path) || api.is_some()`; when `swagger`, add `div().text_sm().text_color(muted).child("Swagger 2.0 isn't rendered")` before Open in editor.
6. Rendered body: if `api` is `Some(view)`, the body is `div().size_full().child(view.clone())`; otherwise the existing Markdown.
7. `open_at` already switches to Raw; keep it.

- [ ] **Step 6: Build, gate, commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: hydraui shows OpenAPI specs as API docs in the file pane"
```

---

### Task 5: Smoke, the big-spec check, docs

**Files:**
- Modify: `site/app.html` (the file pane section: one sentence on OpenAPI)
- Create (scratchpad only, not committed): `petstore.yaml`, a generated `big.yaml` (~1 MB), a smoke script

- [ ] **Step 1: Petstore smoke** — with the scratch `HYDRA_HOME` (`layout-home` from the layouts smoke) open hydraui on a layout whose Loose tab's file pane has `files = ["<scratch>\\petstore.yaml"]` (write that `.hydraui` by hand). petstore.yaml: the `PETS` text from Task 2's tests plus `components.schemas.Node` (self-referencing) and a `common.yaml#/components/schemas/Id` ref with `common.yaml` beside it. Screenshot (PrintWindow) and check:
  1. It opens in Rendered: title, version, servers, filter, groups store/pets/default, method badges coloured.
  2. Click `GET /pets/{id}`: parameters (id path *, trace header), responses 200 (green) with `Pet ▸`, default.
  3. Expand `Pet`: properties; Schemas ▸ → Node → children shows `Node[]` then `↻ Node` one level down.
  4. Click the `Id` external label: `common.yaml` opens as a new file tab.
  5. Type `orders` in the filter: only `store` / `POST /orders` remain.
  6. Raw, break the YAML (type `: [` on a line), back to Rendered: banner with the line, greyed docs; Go to line lands in Raw at that line; undo → docs back, `GET /pets/{id}` still open.
  Ledger each result; fix failures with systematic-debugging (a unit test where the failing piece is pure).

- [ ] **Step 2: Big spec** — generate `big.yaml` with a PowerShell loop: 2,500 paths × `get`+`post` under 25 tags, each with 3 parameters and a 200 response referencing one of 200 schemas of 8 properties (≈1 MB). Time from opening the file tab to the first Rendered paint (stopwatch around a PrintWindow poll until the title text appears), and scroll with 20 `WM_MOUSEWHEEL` posts. Expected: first paint under 2 s on this machine; scrolling responsive (each PrintWindow after a wheel post differs). If it is slow, collapse all tag groups by default for specs with more than 500 operations (store each `tag:` key as collapsed) and ledger that as a ruling; re-measure.

- [ ] **Step 3: Docs** — in `site/app.html`'s file pane section, after the Markdown sentence: "OpenAPI 3 specs (`.yaml`, `.yml`, `.json`) open as API docs: endpoints grouped by tag, each with its parameters, request body and responses, and schemas you can unfold. **Raw** shows the YAML; edits show up in the docs as you type."

- [ ] **Step 4: Commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add site/app.html crates/hydraui
git commit -m "docs: site mentions OpenAPI docs in the file pane"
```
