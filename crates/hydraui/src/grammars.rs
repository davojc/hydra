//! Highlighting grammars gpui-component doesn't ship, built from the C sources vendored
//! under `grammars/` (see build.rs) and registered under the names `file_doc::language`
//! gives.
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_typespec() -> *const ();
    fn tree_sitter_dockerfile() -> *const ();
    fn tree_sitter_hcl() -> *const ();
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

/// Dockerfiles, from github.com/camdencheek/tree-sitter-dockerfile (MIT).
const DOCKERFILE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_dockerfile) };

const DOCKERFILE_HIGHLIGHTS: &str = include_str!("../grammars/dockerfile/queries/highlights.scm");
const DOCKERFILE_INJECTIONS: &str = include_str!("../grammars/dockerfile/queries/injections.scm");

/// Shell-form commands (`RUN apt-get ...`) are highlighted with the bash grammar.
pub fn dockerfile() -> LanguageConfig {
    LanguageConfig::new(
        "dockerfile",
        DOCKERFILE.into(),
        vec!["bash".into()],
        DOCKERFILE_HIGHLIGHTS,
        DOCKERFILE_INJECTIONS,
        "",
    )
}

/// HCL and Terraform, from github.com/tree-sitter-grammars/tree-sitter-hcl (Apache-2.0),
/// with nvim-treesitter's colour rules (Apache-2.0).
const HCL: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_hcl) };

const HCL_HIGHLIGHTS: &str = include_str!("../grammars/hcl/queries/highlights.scm");

pub fn hcl() -> LanguageConfig {
    LanguageConfig::new("hcl", HCL.into(), vec![], HCL_HIGHLIGHTS, "", "")
}

/// GraphQL: gpui-component ships the grammar (github.com/joowani/tree-sitter-graphql, MIT)
/// with no colour rules, so these are nvim-treesitter's (Apache-2.0).
const GRAPHQL_HIGHLIGHTS: &str = include_str!("../grammars/graphql/queries/highlights.scm");

pub fn graphql() -> LanguageConfig {
    LanguageConfig::new(
        "graphql",
        tree_sitter_graphql::LANGUAGE.into(),
        vec![],
        GRAPHQL_HIGHLIGHTS,
        "",
        "",
    )
}

/// Protobuf: as for GraphQL, the grammar's own colour rules, which its crate leaves out.
const PROTO_HIGHLIGHTS: &str = include_str!("../grammars/proto/queries/highlights.scm");

pub fn proto() -> LanguageConfig {
    LanguageConfig::new(
        "proto",
        tree_sitter_proto::LANGUAGE.into(),
        vec![],
        PROTO_HIGHLIGHTS,
        "",
        "",
    )
}

/// C#: as for GraphQL, gpui-component ships the grammar with no colour rules, so these are
/// the grammar's own (MIT), adapted to the theme's names.
const CSHARP_HIGHLIGHTS: &str = include_str!("../grammars/csharp/queries/highlights.scm");

pub fn csharp() -> LanguageConfig {
    LanguageConfig::new(
        "csharp",
        tree_sitter_c_sharp::LANGUAGE.into(),
        vec![],
        CSHARP_HIGHLIGHTS,
        "",
        "",
    )
}

/// SQL: gpui-component's own rules, with the nvim-style names its theme doesn't know
/// renamed to ones it does, so columns, decimals, CASE/WHEN and parameters get colours.
/// Its number patterns are written in Lua's `%d`, which tree-sitter's regexes read as a
/// literal `%d`, so they're spelled `[0-9]` here; and they follow the catch-all string
/// pattern, which the highlighter lets win, so that one is kept off numbers.
fn sql_highlights() -> String {
    [
        (
            "(literal) @string\n",
            "((literal) @string (#not-match? @string \"^[-+]?[0-9]*[.]?[0-9]+$\"))\n",
        ),
        (r"%d*\.%d*", "[0-9]*[.][0-9]*"),
        ("%d", "[0-9]"),
        ("@field", "@property"),
        ("@float", "@number"),
        ("@conditional", "@keyword"),
        ("@storageclass", "@keyword"),
        ("@parameter", "@variable.special"),
    ]
    .iter()
    .fold(
        tree_sitter_sequel::HIGHLIGHTS_QUERY.to_string(),
        |q, (old, new)| q.replace(old, new),
    )
}

pub fn sql() -> LanguageConfig {
    LanguageConfig::new(
        "sql",
        tree_sitter_sequel::LANGUAGE.into(),
        vec![],
        &sql_highlights(),
        "",
        "",
    )
}

/// Adds the grammars to the editor's highlighter; call once at startup.
pub fn register() {
    LanguageRegistry::singleton().register("typespec", &typespec());
    LanguageRegistry::singleton().register("dockerfile", &dockerfile());
    LanguageRegistry::singleton().register("hcl", &hcl());
    // Replace gpui-component's colourless built-ins.
    LanguageRegistry::singleton().register("graphql", &graphql());
    LanguageRegistry::singleton().register("proto", &proto());
    LanguageRegistry::singleton().register("csharp", &csharp());
    LanguageRegistry::singleton().register("sql", &sql());
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

    /// Parses `src` (failing on a syntax error) and returns every (capture, text) pair
    /// `query` finds in it.
    fn captures<'a>(
        lang: LanguageFn,
        query: &str,
        src: &'a str,
    ) -> std::collections::HashSet<(String, &'a str)> {
        let lang: tree_sitter::Language = lang.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(src, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let query = Query::new(&lang, query).unwrap();
        let mut cursor = QueryCursor::new();
        let mut caught = std::collections::HashSet::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures {
                let name = query.capture_names()[c.index as usize].to_string();
                caught.insert((name, c.node.utf8_text(src.as_bytes()).unwrap()));
            }
        }
        caught
    }

    #[test]
    fn typespec_parses_and_its_colour_rules_load() {
        let caught = captures(TYPESPEC, &typespec_highlights(), SAMPLE);
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
            let want = (want.0.to_string(), want.1);
            assert!(caught.contains(&want), "{want:?} missing from {caught:?}");
        }
    }

    const DOCKERFILE_SAMPLE: &str = r#"# build stage
FROM rust:1.80 AS build
ARG PROFILE=release
ENV APP_HOME="/app"
WORKDIR $APP_HOME
COPY --from=deps /src /src
RUN apt-get update &&     apt-get install -y curl
EXPOSE 8080
CMD ["./server", "--port", "8080"]
"#;

    #[test]
    fn dockerfile_parses_and_its_colour_rules_load() {
        let caught = captures(DOCKERFILE, DOCKERFILE_HIGHLIGHTS, DOCKERFILE_SAMPLE);
        for want in [
            ("keyword", "FROM"),
            ("keyword", "AS"),
            ("keyword", "RUN"),
            ("comment", "# build stage"),
            ("string", "\"/app\""),
            ("string", "\"./server\""),
            ("operator", ":"),
        ] {
            let want = (want.0.to_string(), want.1);
            assert!(caught.contains(&want), "{want:?} missing from {caught:?}");
        }
    }

    #[test]
    fn dockerfile_shell_commands_are_injected_as_bash() {
        let caught = captures(DOCKERFILE, DOCKERFILE_INJECTIONS, DOCKERFILE_SAMPLE);
        assert!(
            caught.iter().any(|(name, text)| name == "injection.content"
                && text.starts_with("apt-get update")
                && text.contains("apt-get install -y curl")),
            "{caught:?}"
        );
    }

    /// What the editor shows for each node, following gpui-component's highlighter: captures
    /// in match order, where one on the same node as the capture just before it takes that
    /// capture's name, and a later capture of a node overrides an earlier one.
    fn shown<'a>(lang: LanguageFn, query: &str, src: &'a str) -> Vec<(&'a str, String)> {
        let lang: tree_sitter::Language = lang.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(src, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let query = Query::new(&lang, query).unwrap();
        let mut cursor = QueryCursor::new();
        let mut items: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures {
                let range = c.node.byte_range();
                let name = match items.last() {
                    Some((r, name)) if *r == range => name.clone(),
                    _ => query.capture_names()[c.index as usize].to_string(),
                };
                items.push((range, name));
            }
        }
        let mut shown: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for (range, name) in items {
            match shown.iter_mut().find(|(r, _)| *r == range) {
                Some(seen) => seen.1 = name,
                None => shown.push((range, name)),
            }
        }
        shown.into_iter().map(|(r, name)| (&src[r], name)).collect()
    }

    const HCL_SAMPLE: &str = r#"# A bucket.
resource "aws_s3_bucket" "logs" {
  bucket = "logs-${var.env}"
  count  = 2
  tags   = { team = "infra" }

  lifecycle {
    prevent_destroy = true
  }
}

output "name" {
  value = upper(aws_s3_bucket.logs.id)
  doc   = <<EOT
hello
EOT
}
"#;

    #[test]
    fn hcl_parses_and_shows_the_right_colours() {
        let shown = shown(HCL, HCL_HIGHLIGHTS, HCL_SAMPLE);
        for want in [
            ("resource", "keyword"),
            ("output", "keyword"),
            ("lifecycle", "type"),
            ("bucket", "property"),
            ("prevent_destroy", "property"),
            ("team", "property"),
            ("var", "variable.special"),
            ("env", "property"),
            ("upper", "function"),
            ("2", "number"),
            ("true", "boolean"),
            ("# A bucket.", "comment"),
            ("\"", "string"),
        ] {
            let want = (want.0, want.1.to_string());
            assert!(shown.contains(&want), "{want:?} missing from {shown:?}");
        }
    }

    const GRAPHQL_SAMPLE: &str = r#""""A user."""
type User implements Node @key(fields: "id") {
  id: ID!
  name(upper: Boolean = false): String
}

query Get($id: ID!) {
  user(id: $id) { ...Bits }
}
"#;

    #[test]
    fn graphql_parses_and_shows_the_right_colours() {
        let shown = shown(
            tree_sitter_graphql::LANGUAGE,
            GRAPHQL_HIGHLIGHTS,
            GRAPHQL_SAMPLE,
        );
        for want in [
            ("type", "keyword.type"),
            ("implements", "keyword"),
            ("query", "keyword"),
            ("User", "type"),
            ("ID", "type"),
            ("name", "property"),
            ("upper", "variable.parameter"),
            ("false", "boolean"),
            ("\"\"\"A user.\"\"\"", "comment.doc"),
            ("\"id\"", "string"),
            ("Get", "variable"),
        ] {
            let want = (want.0, want.1.to_string());
            assert!(shown.contains(&want), "{want:?} missing from {shown:?}");
        }
    }

    const PROTO_SAMPLE: &str = r#"syntax = "proto3";
package demo;

// A pet.
message Pet {
  string name = 1;
  repeated int32 ids = 2;
}

service Pets {
  rpc Get (Pet) returns (stream Pet);
}
"#;

    #[test]
    fn proto_parses_and_shows_the_right_colours() {
        let shown = shown(tree_sitter_proto::LANGUAGE, PROTO_HIGHLIGHTS, PROTO_SAMPLE);
        for want in [
            ("syntax", "keyword"),
            ("message", "keyword"),
            ("repeated", "keyword"),
            ("rpc", "keyword"),
            ("Pet", "type"),
            ("Pets", "type"),
            ("\"proto3\"", "string"),
            ("1", "number"),
            ("// A pet.", "comment"),
            ("Get", "function"),
            ("stream", "keyword"),
        ] {
            let want = (want.0, want.1.to_string());
            assert!(shown.contains(&want), "{want:?} missing from {shown:?}");
        }
    }

    const CSHARP_SAMPLE: &str = r#"using System.Text;

namespace Demo.Pets;

/// A pet.
[Serializable]
public sealed class Pet<T> : IAnimal where T : struct
{
    public string Name { get; set; } = "rex";

    // How old.
    public int Age(int years, double scale = 1.5) => years * 2;

    public void Speak()
    {
        var sb = new StringBuilder();
        if (sb.Length == 0 && true) { Console.WriteLine($"hi {Name}"); }
        var tab = "a\tb";
        object nothing = null;
    }
}

enum Colour { Red, Green }
"#;

    #[test]
    fn csharp_parses_and_shows_the_right_colours() {
        let shown = shown(
            tree_sitter_c_sharp::LANGUAGE,
            CSHARP_HIGHLIGHTS,
            CSHARP_SAMPLE,
        );
        for want in [
            ("using", "keyword"),
            ("namespace", "keyword"),
            ("Demo.Pets", "type"),
            ("public", "keyword"),
            ("class", "keyword"),
            ("Pet", "type"),
            ("T", "type"),
            ("IAnimal", "type"),
            ("Serializable", "attribute"),
            ("string", "type.builtin"),
            ("Name", "property"),
            ("Age", "function"),
            ("years", "variable.parameter"),
            ("1.5", "number"),
            ("var", "keyword"),
            ("StringBuilder", "type"),
            ("WriteLine", "function"),
            ("Length", "property"),
            ("true", "boolean"),
            ("null", "constant"),
            (r"\t", "string.escape"),
            ("Text", "type"),
            ("/// A pet.", "comment.doc"),
            ("// How old.", "comment"),
            ("Red", "property"),
        ] {
            let want = (want.0, want.1.to_string());
            assert!(shown.contains(&want), "{want:?} missing from {shown:?}");
        }
    }

    const SQL_SAMPLE: &str = "SELECT u.name, 1.5 AS ratio,
  CASE WHEN u.age > 18 THEN 'adult' ELSE 'minor' END
FROM users u
WHERE u.id = $1;
";

    #[test]
    fn sql_shows_the_names_gpui_component_left_uncoloured() {
        let shown = shown(tree_sitter_sequel::LANGUAGE, &sql_highlights(), SQL_SAMPLE);
        for want in [
            ("SELECT", "keyword"),
            ("name", "property"),
            ("1.5", "number"),
            ("CASE", "keyword"),
            ("WHEN", "keyword"),
            ("'adult'", "string"),
        ] {
            let want = (want.0, want.1.to_string());
            assert!(shown.contains(&want), "{want:?} missing from {shown:?}");
        }
        let theme = gpui_component::highlighter::HighlightTheme::default_dark();
        for want in ["18", "name", "1.5", "$1"] {
            let (_, name) = shown.iter().find(|(text, _)| *text == want).unwrap();
            assert!(
                theme.style.syntax.style(name).is_some(),
                "{want} is {name}, which has no colour"
            );
        }
        assert!(shown.contains(&("18", "number".to_string())), "{shown:?}");
    }

    /// The built-in this file relies on gpui-component for.
    #[test]
    fn java_comes_from_gpui_component() {
        assert!(
            gpui_component::highlighter::LanguageRegistry::singleton()
                .language("java")
                .is_some()
        );
    }
}
