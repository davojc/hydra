//! An OpenAPI 3.x spec as the view needs it (spec §7.8).
use serde_json::Value;

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
    /// A path item that is a `$ref` this view can't follow (another file, or missing).
    pub reference: Option<String>,
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

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// A local JSON pointer (`#/a/b`), with `~1` and `~0` unescaped; `None` for refs into
/// other files and for pointers that lead nowhere.
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

/// An object that may be a `$ref` to one (a parameter, response or request body), followed
/// locally; an unresolvable ref gives `null`, which reads as empty.
fn deref<'a>(root: &'a Value, v: &'a Value) -> &'a Value {
    static NULL: Value = Value::Null;
    let mut v = v;
    for _ in 0..8 {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => v = resolve(root, r).unwrap_or(&NULL),
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
                .map(|(media, c)| {
                    (
                        media.clone(),
                        c.get("schema").cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `$ref` of an object this view can't follow (into another file, or to nothing), so it
/// shows as a ref instead of vanishing.
fn unresolved<'a>(root: &Value, v: &'a Value) -> Option<&'a str> {
    let r = v.get("$ref").and_then(Value::as_str)?;
    deref(root, v).is_null().then_some(r)
}

fn ref_schema(r: &str) -> Value {
    serde_json::json!({ "$ref": r })
}

fn parameters(root: &Value, list: Option<&Value>) -> Vec<Parameter> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            if let Some(r) = unresolved(root, p) {
                return Some(Parameter {
                    name: super::schema::ref_name(r).to_string(),
                    location: String::new(),
                    required: false,
                    description: None,
                    schema: ref_schema(r),
                });
            }
            let p = deref(root, p);
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

fn operation(
    root: &Value,
    method: &str,
    path: &str,
    op: &Value,
    shared: &[Parameter],
) -> Operation {
    let mut params = shared.to_vec();
    for own in parameters(root, op.get("parameters")) {
        match params
            .iter_mut()
            .find(|p| p.name == own.name && p.location == own.location)
        {
            Some(p) => *p = own,
            None => params.push(own),
        }
    }
    let responses = op
        .get("responses")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(status, r)| {
                    if let Some(rf) = unresolved(root, r) {
                        return Response {
                            status: status.clone(),
                            description: None,
                            content: vec![(String::new(), ref_schema(rf))],
                        };
                    }
                    let r = deref(root, r);
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
        let b = deref(root, b);
        Body {
            description: text(b, "description"),
            content: content(b),
        }
    });
    Operation {
        method: method.to_uppercase(),
        path: path.to_string(),
        summary: text(op, "summary"),
        description: text(op, "description"),
        operation_id: text(op, "operationId"),
        deprecated: op
            .get("deprecated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        parameters: params,
        request_body,
        responses,
        reference: None,
    }
}

/// The YAML library's limits, with room for big specs: its default of 250,000 nodes turns
/// away real APIs of a few MB. The file pane never reads more than 5 MB, and a node takes
/// at least a couple of bytes, so these bound the work as well; the alias and anchor caps
/// that stop expansion bombs keep their defaults.
fn yaml_options() -> serde_saphyr::Options {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_nodes = 5_000_000;
    budget.max_events = 10_000_000;
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    options
}

/// A parser's message as one plain line for the banner (which shows the line itself): the
/// first line only, without `error:`, `line N column M:` or a trailing `at line N column M`.
pub fn short_message(message: &str) -> String {
    let first = message.lines().next().unwrap_or("").trim();
    let mut s = first.strip_prefix("error:").unwrap_or(first).trim_start();
    if let Some(rest) = s.strip_prefix("line ")
        && let Some((_, after)) = rest.split_once(": ")
    {
        s = after;
    }
    let s = match s.rfind(" at line ") {
        Some(i) => &s[..i],
        None => s,
    };
    // serde's wording about Rust types means nothing to someone editing YAML.
    if s.contains("deserialize") || s.contains("Option<") {
        return "this isn't valid YAML here".into();
    }
    s.trim().to_string()
}

/// Reads a spec: YAML, or JSON when `json`. Operations are grouped by their first tag,
/// groups in the `tags` list's order then as operations name them, untagged ones last
/// under "default".
pub fn parse(text_in: &str, json: bool) -> Result<Api, ParseError> {
    let src = text_in.trim_start_matches('\u{feff}');
    let root: Value = if json {
        serde_json::from_str(src).map_err(|e| ParseError {
            message: short_message(&e.to_string()),
            line: Some(e.line()),
        })?
    } else {
        serde_saphyr::from_str_with_options(src, yaml_options()).map_err(|e| ParseError {
            message: short_message(&e.to_string()),
            line: e.location().map(|l| l.line() as usize),
        })?
    };
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
    if let Some(paths) = root.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            // A path item in another file (split specs) or missing: one row that links to it.
            if let Some(r) = unresolved(&root, item) {
                untagged.push(Operation {
                    method: "REF".into(),
                    path: path.clone(),
                    summary: None,
                    description: None,
                    operation_id: None,
                    deprecated: false,
                    parameters: Vec::new(),
                    request_body: None,
                    responses: Vec::new(),
                    reference: Some(r.to_string()),
                });
                continue;
            }
            let item = deref(&root, item);
            let shared = parameters(&root, item.get("parameters"));
            for method in METHODS {
                let Some(op) = item.get(method) else { continue };
                let op_model = operation(&root, method, path, op, &shared);
                let tag = op
                    .get("tags")
                    .and_then(Value::as_array)
                    .and_then(|t| t.first())
                    .and_then(Value::as_str);
                match tag {
                    None => untagged.push(op_model),
                    Some(tag) => match groups.iter_mut().find(|g| g.name == tag) {
                        Some(g) => g.operations.push(op_model),
                        None => groups.push(TagGroup {
                            name: tag.to_string(),
                            description: None,
                            operations: vec![op_model],
                        }),
                    },
                }
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
        assert_eq!(
            (api.title.as_str(), api.version.as_str()),
            ("Petstore", "1.0.0")
        );
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
            .map(|g| {
                (
                    g.name.as_str(),
                    g.operations.iter().map(|o| o.key()).collect(),
                )
            })
            .collect();
        assert_eq!(
            groups,
            [
                ("store", vec!["POST /orders".to_string()]),
                (
                    "pets",
                    vec!["GET /pets/{id}".into(), "DELETE /pets/{id}".into()]
                ),
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
            .map(|p| {
                (
                    p.name.as_str(),
                    p.location.as_str(),
                    p.required,
                    p.description.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            params,
            [
                ("id", "path", true, None),
                ("trace", "header", false, Some("overridden"))
            ]
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
        assert_eq!(
            get.responses[1].description.as_deref(),
            Some("Something broke")
        );
        assert_eq!(
            get.responses[0].content[0],
            (
                "application/json".to_string(),
                serde_json::json!({"$ref": "#/components/schemas/Pet"})
            ),
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
        let api = parse(
            "openapi: 3.0.0\ncomponents:\n  schemas:\n    A: {type: string}\n",
            false,
        )
        .unwrap();
        assert_eq!(api.missing, ["info", "paths"]);
        assert_eq!(api.schemas, ["A"]);
        assert!(api.groups.is_empty());
    }

    #[test]
    fn a_big_spec_is_not_refused_for_its_size() {
        // ~300,000 nodes: over the YAML library's default budget of 250,000, yet a few MB,
        // the size of a real large API.
        let mut text = String::from(
            "openapi: 3.0.0\ninfo: {title: Big, version: '1'}\npaths: {}\ncomponents:\n  schemas:\n    X:\n      enum:\n",
        );
        for i in 0..300_000 {
            text.push_str(&format!("        - v{i}\n"));
        }
        let api = parse(&text, false).unwrap();
        assert_eq!(api.schemas, ["X"]);
    }

    #[test]
    fn error_messages_are_one_plain_line() {
        let err = parse("openapi: 3.0.0\ninfo:\n  title: [\n", false).unwrap_err();
        assert!(!err.message.contains('\n'), "{:?}", err.message);
        assert!(!err.message.starts_with("error:"), "{:?}", err.message);
        assert!(
            !err.message.contains("line "),
            "the banner says the line: {:?}",
            err.message
        );
        assert!(!err.message.is_empty());
        assert_eq!(
            short_message("error: line 1 column 1: cannot read this\n --> <input>:1:1\n |"),
            "cannot read this"
        );
        assert_eq!(
            short_message("expected value at line 3 column 1"),
            "expected value"
        );
        // The YAML library's Rust wording becomes plain words.
        let err = parse(": [openapi: 3.0.3\ninfo: {}\n", false).unwrap_err();
        assert_eq!(err.message, "this isn't valid YAML here", "{err:?}");
    }

    #[test]
    fn refs_that_cannot_be_followed_stay_visible() {
        let text = r##"openapi: 3.0.0
info: {title: Split, version: '1'}
paths:
  /pets:
    $ref: paths/pets.yaml
  /orders:
    get:
      parameters:
        - $ref: '../common.yaml#/components/parameters/PageSize'
        - $ref: '#/components/parameters/Nope'
      responses:
        '404':
          $ref: 'errors.yaml#/NotFound'
"##;
        let api = parse(text, false).unwrap();
        let ops: Vec<&Operation> = api.groups.iter().flat_map(|g| &g.operations).collect();
        let pets = ops
            .iter()
            .find(|o| o.path == "/pets")
            .expect("the path item shows");
        assert_eq!(pets.method, "REF");
        assert_eq!(pets.reference.as_deref(), Some("paths/pets.yaml"));
        let get = ops.iter().find(|o| o.path == "/orders").unwrap();
        let params: Vec<(&str, &Value)> = get
            .parameters
            .iter()
            .map(|p| (p.name.as_str(), &p.schema))
            .collect();
        assert_eq!(
            params,
            [
                (
                    "PageSize",
                    &serde_json::json!({"$ref": "../common.yaml#/components/parameters/PageSize"})
                ),
                (
                    "Nope",
                    &serde_json::json!({"$ref": "#/components/parameters/Nope"})
                ),
            ]
        );
        assert_eq!(get.responses[0].status, "404");
        assert_eq!(
            get.responses[0].content,
            [(
                "".to_string(),
                serde_json::json!({"$ref": "errors.yaml#/NotFound"})
            )]
        );
    }

    #[test]
    fn pointers_unescape() {
        let root = serde_json::json!({"paths": {"/a/b": {"x~y": 1}}});
        assert_eq!(
            resolve(&root, "#/paths/~1a~1b/x~0y"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(resolve(&root, "#/nope"), None);
        assert_eq!(resolve(&root, "other.yaml#/x"), None, "not a local ref");
    }
}
