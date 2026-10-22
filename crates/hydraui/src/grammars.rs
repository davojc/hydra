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

/// Adds the grammars to the editor's highlighter; call once at startup.
pub fn register() {
    LanguageRegistry::singleton().register("typespec", &typespec());
    LanguageRegistry::singleton().register("dockerfile", &dockerfile());
    LanguageRegistry::singleton().register("hcl", &hcl());
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
}
