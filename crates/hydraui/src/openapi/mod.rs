//! OpenAPI specs as API docs (spec §7.8): detection, the model, and schema trees. No UI.
pub mod detect;

pub use detect::{SpecKind, spec_kind};
