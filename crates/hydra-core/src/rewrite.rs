use toml_edit::{DocumentMut, Formatted, Item, Table, TomlError, Value};

/// Replaces a string value while keeping its surrounding whitespace and comments.
fn replace_string(s: &mut Formatted<String>, new: String) {
    let mut f = Formatted::new(new);
    *f.decor_mut() = s.decor().clone();
    *s = f;
}

/// `[bindings]` entries whose value is `old` get `new`. Returns the changed patterns.
pub fn rename_bindings(
    text: &str,
    old: &str,
    new: &str,
) -> Result<(String, Vec<String>), TomlError> {
    let mut doc: DocumentMut = text.parse()?;
    let mut changed = Vec::new();
    if let Some(table) = doc.get_mut("bindings").and_then(Item::as_table_like_mut) {
        for (key, item) in table.iter_mut() {
            if let Some(Value::String(s)) = item.as_value_mut()
                && s.value() == old
            {
                replace_string(s, new.to_string());
                changed.push(key.get().to_string());
            }
        }
    }
    Ok((doc.to_string(), changed))
}

/// Rewrites every `secret:<old>/...` string value to `secret:<new>/...`. Returns the count.
pub fn rename_secret_refs(text: &str, old: &str, new: &str) -> Result<(String, usize), TomlError> {
    let mut doc: DocumentMut = text.parse()?;
    let (from, to) = (format!("secret:{old}/"), format!("secret:{new}/"));
    let mut count = 0;
    walk_table(doc.as_table_mut(), &from, &to, &mut count);
    Ok((doc.to_string(), count))
}

fn walk_table(t: &mut Table, from: &str, to: &str, count: &mut usize) {
    for (_, item) in t.iter_mut() {
        walk_item(item, from, to, count);
    }
}

fn walk_item(item: &mut Item, from: &str, to: &str, count: &mut usize) {
    match item {
        Item::Value(v) => walk_value(v, from, to, count),
        Item::Table(t) => walk_table(t, from, to, count),
        Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                walk_table(t, from, to, count);
            }
        }
        Item::None => {}
    }
}

fn walk_value(v: &mut Value, from: &str, to: &str, count: &mut usize) {
    match v {
        Value::String(s) => {
            if let Some(rest) = s.value().strip_prefix(from) {
                let new = format!("{to}{rest}");
                replace_string(s, new);
                *count += 1;
            }
        }
        Value::Array(a) => {
            for x in a.iter_mut() {
                walk_value(x, from, to, count);
            }
        }
        Value::InlineTable(t) => {
            for (_, x) in t.iter_mut() {
                walk_value(x, from, to, count);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_only_matching_bindings_and_keeps_comments() {
        let src = "default_shell = \"pwsh\"\n\n[bindings]\n# work repos\n\"E:/acme/**\" = \"work\" # main\n\"E:/AI/**\" = \"personal\"\n";
        let (out, changed) = rename_bindings(src, "work", "acme").unwrap();
        assert_eq!(changed, vec!["E:/acme/**"]);
        assert_eq!(
            out,
            "default_shell = \"pwsh\"\n\n[bindings]\n# work repos\n\"E:/acme/**\" = \"acme\" # main\n\"E:/AI/**\" = \"personal\"\n"
        );
    }

    #[test]
    fn renames_secret_references_everywhere() {
        let src = "[env]\nA = \"secret:work/a\" # keep\nB = \"secret:workshop/b\"\nC = \"plain\"\n\n[gws]\ncredentials = \"secret:work/gws-creds\"\n";
        let (out, n) = rename_secret_refs(src, "work", "acme").unwrap();
        assert_eq!(n, 2);
        assert!(out.contains("A = \"secret:acme/a\" # keep"), "{out}");
        assert!(out.contains("B = \"secret:workshop/b\""), "{out}");
        assert!(
            out.contains("credentials = \"secret:acme/gws-creds\""),
            "{out}"
        );
    }

    #[test]
    fn reports_parse_errors() {
        assert!(rename_bindings("[bindings\n", "a", "b").is_err());
    }
}
