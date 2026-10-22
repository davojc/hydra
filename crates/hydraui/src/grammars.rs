//! Highlighting grammars gpui-component doesn't ship, built from the C sources vendored
//! under `grammars/` (see build.rs) and registered under the names `file_doc::language`
//! gives.
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_typespec() -> *const ();
}

/// TypeSpec (`.tsp`), from github.com/happenslol/tree-sitter-typespec (MIT).
const TYPESPEC: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_typespec) };

/// The TypeSpec colour rules. Namespaces and `using` targets are captured as `@module`,
/// which the theme doesn't colour, so they're shown as types.
fn typespec_highlights() -> String {
    include_str!("../grammars/typespec/queries/highlights.scm").replace("@module", "@type")
}

pub fn typespec() -> LanguageConfig {
    LanguageConfig::new(
        "typespec",
        TYPESPEC.into(),
        vec![],
        &typespec_highlights(),
        "",
        "",
    )
}

/// Adds the grammars to the editor's highlighter; call once at startup.
pub fn register() {
    LanguageRegistry::singleton().register("typespec", &typespec());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    const SAMPLE: &str = r#"import "@typespec/http";
using Http;

/// A pet.
@doc("A pet")
model Pet {
  id: int32;
  name?: string = "rex";
}

@route("/pets")
op list(@query limit: int32): Pet[];
"#;

    #[test]
    fn typespec_parses_and_its_colour_rules_load() {
        let lang: tree_sitter::Language = TYPESPEC.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(SAMPLE, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let query = Query::new(&lang, &typespec_highlights()).unwrap();
        let mut cursor = QueryCursor::new();
        let mut caught = std::collections::HashSet::new();
        let mut matches = cursor.matches(&query, tree.root_node(), SAMPLE.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures {
                let name = query.capture_names()[c.index as usize];
                caught.insert((name, c.node.utf8_text(SAMPLE.as_bytes()).unwrap()));
            }
        }
        for want in [
            ("keyword.type", "model"),
            ("keyword.function", "op"),
            ("type", "Pet"),
            ("type", "Http"),
            ("string", "\"rex\""),
            ("comment", "/// A pet."),
            ("function", "list"),
            ("variable.member", "name"),
        ] {
            assert!(caught.contains(&want), "{want:?} missing from {caught:?}");
        }
    }
}
