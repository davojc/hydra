//! What the API view adds to a schema (spec §7.8): an example value made up from it, the
//! rules it sets, a curl command for an operation, and where the operation is in the file.
use serde_json::{Map, Value, json};

use super::model::{Api, Operation, resolve};

/// How deep an example goes before it stops (a deep or odd schema still ends).
const MAX_DEPTH: usize = 12;

/// An example value for `schema`: its own `example` (or first `examples` entry), `default`,
/// `const` or first `enum` value when it has one, else one made up from its type. A schema
/// that refers back to itself stops with `null`.
pub fn example(root: &Value, schema: &Value) -> Value {
    make(root, schema, &mut Vec::new(), 0)
}

fn make(root: &Value, schema: &Value, seen: &mut Vec<String>, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::Null;
    }
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        if seen.iter().any(|s| s == r) {
            return Value::Null;
        }
        let Some(target) = resolve(root, r) else {
            return Value::Null;
        };
        seen.push(r.to_string());
        let v = make(root, target, seen, depth + 1);
        seen.pop();
        return v;
    }
    for key in ["example", "default", "const"] {
        if let Some(v) = schema.get(key) {
            return v.clone();
        }
    }
    if let Some(v) = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
    {
        return v.clone();
    }
    if let Some(v) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
    {
        return v.clone();
    }
    if let Some(first) = ["oneOf", "anyOf"]
        .iter()
        .find_map(|k| schema.get(*k).and_then(Value::as_array))
        .and_then(|l| l.first())
    {
        return make(root, first, seen, depth + 1);
    }
    let kind = match schema.get("type") {
        Some(Value::String(t)) => t.as_str(),
        // 3.1: `type: [string, "null"]` reads as its first real type.
        Some(Value::Array(l)) => l
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null"),
        _ if schema.get("properties").is_some() || schema.get("allOf").is_some() => "object",
        _ if schema.get("items").is_some() => "array",
        _ => "",
    };
    match kind {
        "object" => {
            let mut out = Map::new();
            fill(root, schema, &mut out, seen, depth);
            Value::Object(out)
        }
        "array" => {
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            Value::Array(vec![make(root, &items, seen, depth + 1)])
        }
        "string" => Value::String(string_example(schema)),
        "integer" => json!(number_start(schema).map_or(0, |n| n.ceil() as i64)),
        "number" => json!(number_start(schema).unwrap_or(0.0)),
        "boolean" => json!(true),
        _ => Value::Null,
    }
}

/// An object's properties, `allOf` parts merged in, each with its example.
fn fill(
    root: &Value,
    schema: &Value,
    out: &mut Map<String, Value>,
    seen: &mut Vec<String>,
    depth: usize,
) {
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (name, s) in props {
            // A field only the server writes isn't sent; one only the client sends isn't
            // returned: an example shows both, as the spec lists them.
            out.insert(name.clone(), make(root, s, seen, depth + 1));
        }
    }
    if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
        for part in parts {
            if let Value::Object(m) = make(root, part, seen, depth + 1) {
                out.extend(m);
            }
        }
    }
}

fn number_start(schema: &Value) -> Option<f64> {
    schema
        .get("minimum")
        .or_else(|| schema.get("exclusiveMinimum"))
        .and_then(Value::as_f64)
}

fn string_example(schema: &Value) -> String {
    match schema.get("format").and_then(Value::as_str) {
        Some("date") => "2026-01-31".into(),
        Some("date-time") => "2026-01-31T09:30:00Z".into(),
        Some("time") => "09:30:00".into(),
        Some("uuid") => "3f2b8c1e-5a7d-4e0b-9c61-2d8f0a4b7e12".into(),
        Some("email") => "someone@example.com".into(),
        Some("uri" | "url") => "https://example.com".into(),
        Some("hostname") => "example.com".into(),
        Some("ipv4") => "192.0.2.1".into(),
        Some("byte") => "aGVsbG8=".into(),
        _ => "string".into(),
    }
}

/// The rules a schema sets, as short chips: lengths, ranges, sizes, a pattern, a default,
/// the allowed values, read-only or write-only.
pub fn rules(schema: &Value) -> Vec<String> {
    let num = |k: &str| schema.get(k).and_then(Value::as_f64);
    let fmt = |n: f64| {
        if n.fract() == 0.0 {
            format!("{}", n as i64)
        } else {
            n.to_string()
        }
    };
    let mut out = Vec::new();
    match (num("minLength"), num("maxLength")) {
        (Some(a), Some(b)) => out.push(format!("{}–{} chars", fmt(a), fmt(b))),
        (Some(a), None) => out.push(format!("at least {} chars", fmt(a))),
        (None, Some(b)) => out.push(format!("max {} chars", fmt(b))),
        _ => {}
    }
    if let Some(n) = num("minimum") {
        out.push(format!("≥ {}", fmt(n)));
    }
    if let Some(n) = num("exclusiveMinimum") {
        out.push(format!("> {}", fmt(n)));
    }
    if let Some(n) = num("maximum") {
        out.push(format!("≤ {}", fmt(n)));
    }
    if let Some(n) = num("exclusiveMaximum") {
        out.push(format!("< {}", fmt(n)));
    }
    if let Some(n) = num("multipleOf") {
        out.push(format!("multiple of {}", fmt(n)));
    }
    match (num("minItems"), num("maxItems")) {
        (Some(a), Some(b)) => out.push(format!("{}–{} items", fmt(a), fmt(b))),
        (Some(a), None) => out.push(format!("at least {} items", fmt(a))),
        (None, Some(b)) => out.push(format!("max {} items", fmt(b))),
        _ => {}
    }
    if schema.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
        out.push("unique".into());
    }
    if let Some(p) = schema.get("pattern").and_then(Value::as_str) {
        out.push(format!("pattern {p}"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let shown: Vec<String> = values.iter().map(plain).collect();
        out.push(format!("one of {}", shown.join(", ")));
    }
    if let Some(d) = schema.get("default") {
        out.push(format!("default {}", plain(d)));
    }
    if schema.get("readOnly").and_then(Value::as_bool) == Some(true) {
        out.push("read-only".into());
    }
    if schema.get("writeOnly").and_then(Value::as_bool) == Some(true) {
        out.push("write-only".into());
    }
    if schema.get("deprecated").and_then(Value::as_bool) == Some(true) {
        out.push("deprecated".into());
    }
    out
}

/// A JSON value as people read it: strings without quotes.
fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Text for a single-quoted shell argument.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A curl command for `op`: the first server, path parameters and required query
/// parameters filled with examples, required headers, and an example JSON body.
pub fn curl(api: &Api, op: &Operation) -> String {
    let base = api.servers.first().map(String::as_str).unwrap_or("");
    let mut path = op.path.clone();
    let mut query = Vec::new();
    let mut headers = Vec::new();
    for p in &op.parameters {
        let value = plain(&example(&api.root, &p.schema));
        match p.location.as_str() {
            "path" => path = path.replace(&format!("{{{}}}", p.name), &value),
            "query" if p.required => query.push(format!("{}={value}", p.name)),
            "header" if p.required => headers.push(format!("{}: {value}", p.name)),
            _ => {}
        }
    }
    let mut url = format!("{}{path}", base.trim_end_matches('/'));
    if !query.is_empty() {
        url = format!("{url}?{}", query.join("&"));
    }
    let mut out = format!("curl -X {} {}", op.method, quoted(&url));
    for h in headers {
        out.push_str(&format!(" \\\n  -H {}", quoted(&h)));
    }
    let json_body = op
        .request_body
        .as_ref()
        .and_then(|b| b.content.iter().find(|(m, _)| m.contains("json")));
    if let Some((media, schema)) = json_body {
        let body = serde_json::to_string(&example(&api.root, schema)).unwrap_or_default();
        out.push_str(&format!(
            " \\\n  -H {}",
            quoted(&format!("Content-Type: {media}"))
        ));
        out.push_str(&format!(" \\\n  -d {}", quoted(&body)));
    }
    out
}

/// The 1-based line of `method` under `path` in the spec's text: the path's key, then the
/// first `method` key after it. `None` when it can't be found (a path from another file).
pub fn operation_line(text: &str, path: &str, method: &str) -> Option<usize> {
    let method = method.to_ascii_lowercase();
    let is_key = |line: &str, key: &str| {
        let t = line.trim_start();
        [
            format!("{key}:"),
            format!("'{key}':"),
            format!("\"{key}\":"),
            format!("\"{key}\" :"),
        ]
        .iter()
        .any(|k| t.starts_with(k.as_str()))
    };
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| is_key(l, path))?;
    let indent = |l: &str| l.len() - l.trim_start().len();
    let path_indent = indent(lines[at]);
    for (i, l) in lines.iter().enumerate().skip(at + 1) {
        if l.trim().is_empty() {
            continue;
        }
        // Back out to the path's own level (YAML): the next path began.
        if !text.trim_start().starts_with('{') && indent(l) <= path_indent {
            return None;
        }
        if is_key(l, &method) {
            return Some(i + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::parse;

    fn root() -> Value {
        json!({"components": {"schemas": {
            "Line": {"type": "object", "properties": {
                "product": {"type": "string"},
                "quantity": {"type": "integer", "minimum": 1}}},
            "Order": {"type": "object", "required": ["id"], "properties": {
                "id": {"type": "string", "format": "uuid"},
                "status": {"type": "string", "enum": ["open", "paid"]},
                "total": {"type": "number", "example": 42.5},
                "lines": {"type": "array", "items": {"$ref": "#/components/schemas/Line"}}}},
            "Node": {"type": "object", "properties": {
                "next": {"$ref": "#/components/schemas/Node"}}}
        }}})
    }

    #[test]
    fn an_example_is_made_up_from_the_schema() {
        let r = root();
        assert_eq!(
            example(&r, &json!({"$ref": "#/components/schemas/Order"})),
            json!({
                "id": "3f2b8c1e-5a7d-4e0b-9c61-2d8f0a4b7e12",
                "status": "open",
                "total": 42.5,
                "lines": [{"product": "string", "quantity": 1}]
            })
        );
        assert_eq!(example(&r, &json!({"type": "boolean"})), json!(true));
        assert_eq!(example(&r, &json!({"type": ["integer", "null"]})), json!(0));
        assert_eq!(
            example(&r, &json!({"type": "string", "default": "x"})),
            json!("x")
        );
        assert_eq!(
            example(
                &r,
                &json!({"oneOf": [{"type": "string", "format": "date"}, {"type": "integer"}]})
            ),
            json!("2026-01-31")
        );
    }

    #[test]
    fn an_example_of_a_schema_that_refers_to_itself_ends() {
        let r = root();
        assert_eq!(
            example(&r, &json!({"$ref": "#/components/schemas/Node"})),
            json!({"next": null})
        );
        assert_eq!(
            example(&r, &json!({"$ref": "#/components/schemas/Nope"})),
            Value::Null
        );
    }

    #[test]
    fn all_of_parts_merge_into_one_example() {
        let r = root();
        let s = json!({"allOf": [{"$ref": "#/components/schemas/Line"},
                                 {"properties": {"note": {"type": "string"}}}]});
        assert_eq!(
            example(&r, &s),
            json!({"product": "string", "quantity": 1, "note": "string"})
        );
    }

    #[test]
    fn rules_read_as_short_chips() {
        assert_eq!(
            rules(&json!({"type": "string", "maxLength": 200})),
            ["max 200 chars"]
        );
        assert_eq!(
            rules(&json!({"type": "integer", "minimum": 1, "maximum": 100, "default": 20})),
            ["≥ 1", "≤ 100", "default 20"]
        );
        assert_eq!(
            rules(&json!({"enum": ["open", "paid"], "readOnly": true})),
            ["one of open, paid", "read-only"]
        );
        assert_eq!(
            rules(&json!({"type": "array", "minItems": 1, "maxItems": 5})),
            ["1–5 items"]
        );
        assert!(rules(&json!({"type": "string"})).is_empty());
    }

    const SPEC: &str = "openapi: 3.0.0
info: {title: Shop, version: '1'}
servers: [{url: 'https://api.shop.example/v1/'}]
paths:
  /orders:
    get:
      parameters:
        - {name: status, in: query, required: true, schema: {type: string, enum: [paid]}}
        - {name: limit, in: query, schema: {type: integer}}
      responses: {'200': {description: ok}}
    post:
      requestBody:
        content:
          application/json:
            schema: {type: object, properties: {note: {type: string, example: \"it's\"}}}
      responses: {'201': {description: ok}}
  /orders/{id}:
    get:
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer, minimum: 7}}
        - {name: X-Key, in: header, required: true, schema: {type: string}}
      responses: {'200': {description: ok}}
";

    fn op<'a>(api: &'a Api, method: &str, path: &str) -> &'a Operation {
        api.groups
            .iter()
            .flat_map(|g| &g.operations)
            .find(|o| o.method == method && o.path == path)
            .unwrap()
    }

    #[test]
    fn a_curl_command_fills_in_what_the_operation_needs() {
        let api = parse(SPEC, false).unwrap();
        assert_eq!(
            curl(&api, op(&api, "GET", "/orders")),
            "curl -X GET 'https://api.shop.example/v1/orders?status=paid'"
        );
        assert_eq!(
            curl(&api, op(&api, "GET", "/orders/{id}")),
            "curl -X GET 'https://api.shop.example/v1/orders/7' \\\n  -H 'X-Key: string'"
        );
        assert_eq!(
            curl(&api, op(&api, "POST", "/orders")),
            "curl -X POST 'https://api.shop.example/v1/orders' \\\n  -H 'Content-Type: application/json' \\\n  -d '{\"note\":\"it'\\''s\"}'"
        );
    }

    #[test]
    fn an_operation_is_found_by_its_line() {
        assert_eq!(operation_line(SPEC, "/orders", "GET"), Some(6));
        assert_eq!(operation_line(SPEC, "/orders", "POST"), Some(11));
        assert_eq!(operation_line(SPEC, "/orders/{id}", "GET"), Some(18));
        assert_eq!(
            operation_line(SPEC, "/orders/{id}", "DELETE"),
            None,
            "not under it"
        );
        assert_eq!(operation_line(SPEC, "/nope", "GET"), None);
        let json = "{\n  \"paths\": {\n    \"/a\": {\n      \"get\": {}\n    }\n  }\n}";
        assert_eq!(operation_line(json, "/a", "GET"), Some(4));
    }
}
