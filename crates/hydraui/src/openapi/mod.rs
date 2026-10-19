//! OpenAPI specs as API docs (spec §7.8): detection, the model, and schema trees. No UI.
pub mod detect;
pub mod model;
pub mod schema;

pub use detect::{SpecKind, spec_kind};
pub use model::{Api, Body, Operation, Parameter, ParseError, Response, TagGroup, parse, resolve};
pub use schema::{Kind, Level, Prop, level, ref_name};

/// The filter box: method, path, summary or operationId, any case; empty matches all.
pub fn matches(op: &Operation, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty()
        || [
            Some(&op.method),
            Some(&op.path),
            op.summary.as_ref(),
            op.operation_id.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|s| s.to_lowercase().contains(&f))
}

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
