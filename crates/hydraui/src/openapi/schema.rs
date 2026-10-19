//! A schema one level at a time (spec §7.8): what the view shows for it (its label) and what
//! opening it reveals (properties, choices, a cycle, a missing or external ref).
use serde_json::Value;

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
    External {
        file: String,
        pointer: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub name: String,
    pub required: bool,
    pub schema: Value,
}

/// The last segment of a ref: `#/components/schemas/Pet` → `Pet`.
pub fn ref_name(r: &str) -> &str {
    r.rsplit('/').next().unwrap_or(r)
}

/// `string`, `integer (int64)`, `string?` (3.0 `nullable` or a 3.1 `type` list with null).
fn type_text(t: &Value, schema: &Value) -> String {
    let base = match t {
        Value::String(s) => s.clone(),
        Value::Array(list) => {
            let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            let null = names.contains(&"null");
            let rest: Vec<&str> = names.into_iter().filter(|n| *n != "null").collect();
            return match (rest.is_empty(), null) {
                (true, _) => "null".into(),
                (false, true) => format!("{}?", rest.join(" | ")),
                (false, false) => rest.join(" | "),
            };
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

fn choices(schema: &Value) -> Option<&Vec<Value>> {
    schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
}

/// The text a schema reads as, without opening it.
fn label(schema: &Value) -> String {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        return ref_name(r).to_string();
    }
    if let Some(list) = choices(schema) {
        let names: Vec<String> = list.iter().map(label).collect();
        return format!("one of: {}", names.join(" | "));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type") {
        Some(Value::String(t)) if t == "array" => {
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            format!("{}[]", label(&items))
        }
        Some(t) => type_text(t, schema),
        None if schema.get("properties").is_some() || schema.get("allOf").is_some() => {
            "object".into()
        }
        None => "any".into(),
    }
}

/// `properties` and `required`, with `allOf` parts (refs followed) merged in.
fn merged(
    root: &Value,
    schema: &Value,
    props: &mut serde_json::Map<String, Value>,
    required: &mut Vec<String>,
    depth: u8,
) {
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
    if let Some(p) = schema.get("properties").and_then(Value::as_object) {
        for (k, v) in p {
            props.insert(k.clone(), v.clone());
        }
    }
    if let Some(req) = schema.get("required").and_then(Value::as_array) {
        required.extend(req.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
        for part in parts {
            merged(root, part, props, required, depth + 1);
        }
    }
}

/// One level of `schema`: its label, and what opening it shows. `above` holds the refs
/// already open above it, so a recursive schema stops at `↻`.
pub fn level(root: &Value, schema: &Value, above: &[String]) -> Level {
    let text = label(schema);
    let kind = kind_of(root, schema, above, 0);
    Level { label: text, kind }
}

fn kind_of(root: &Value, schema: &Value, above: &[String], depth: u8) -> Kind {
    if depth > 16 {
        return Kind::Leaf;
    }
    // Arrays open to their items.
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        let items = schema.get("items").cloned().unwrap_or(Value::Null);
        return kind_of(root, &items, above, depth + 1);
    }
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        if let Some((file, pointer)) = r.split_once('#').filter(|(f, _)| !f.is_empty()) {
            return Kind::External {
                file: file.to_string(),
                pointer: format!("#{pointer}"),
            };
        }
        if above.iter().any(|a| a == r) {
            return Kind::Cycle(ref_name(r).to_string());
        }
        return match resolve(root, r) {
            Some(target) => kind_of(root, target, above, depth + 1),
            None => Kind::Missing(r.to_string()),
        };
    }
    if let Some(list) = choices(schema) {
        return Kind::OneOf(list.iter().map(|c| (label(c), c.clone())).collect());
    }
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    merged(root, schema, &mut props, &mut required, 0);
    if props.is_empty() {
        return Kind::Leaf;
    }
    Kind::Object(
        props
            .into_iter()
            .map(|(name, schema)| Prop {
                required: required.contains(&name),
                name,
                schema,
            })
            .collect(),
    )
}

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
        assert_eq!(
            names(&l),
            [
                ("id".into(), true),
                ("tag".into(), false),
                ("owner".into(), false)
            ]
        );
        let Kind::Object(props) = &l.kind else {
            unreachable!()
        };
        let id = level(&r, &props[0].schema, &[]);
        assert_eq!(
            (id.label.as_str(), &id.kind),
            ("integer (int64)", &Kind::Leaf)
        );
    }

    #[test]
    fn arrays_enums_and_nullable_types() {
        let r = root();
        assert_eq!(
            level(
                &r,
                &json!({"type": "array", "items": {"$ref": "#/components/schemas/Pet"}}),
                &[]
            )
            .label,
            "Pet[]"
        );
        assert_eq!(
            level(&r, &json!({"type": ["string", "null"]}), &[]).label,
            "string?"
        );
        assert_eq!(
            level(&r, &json!({"type": "string", "nullable": true}), &[]).label,
            "string?"
        );
        assert_eq!(
            level(&r, &json!({"enum": ["a", "b"]}), &[]).label,
            r#""a" | "b""#
        );
        assert_eq!(level(&r, &json!({}), &[]).label, "any");
    }

    #[test]
    fn a_schema_open_above_shows_a_cycle() {
        let r = root();
        let node = json!({"$ref": "#/components/schemas/Node"});
        let top = level(&r, &node, &[]);
        let Kind::Object(props) = &top.kind else {
            panic!()
        };
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
        let Kind::OneOf(choices) = &l.kind else {
            panic!("{l:?}")
        };
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].0, "Pet");
    }

    #[test]
    fn missing_and_external_refs() {
        let r = root();
        let l = level(&r, &json!({"$ref": "#/components/schemas/Nope"}), &[]);
        assert_eq!(l.kind, Kind::Missing("#/components/schemas/Nope".into()));
        let l = level(
            &r,
            &json!({"$ref": "common.yaml#/components/schemas/Id"}),
            &[],
        );
        assert_eq!(
            l.kind,
            Kind::External {
                file: "common.yaml".into(),
                pointer: "#/components/schemas/Id".into()
            }
        );
        assert_eq!(l.label, "Id");
        assert_eq!(ref_name("#/components/schemas/Pet"), "Pet");
    }
}
