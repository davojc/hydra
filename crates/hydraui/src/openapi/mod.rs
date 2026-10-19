//! OpenAPI specs as API docs (spec §7.8): detection, the model, and schema trees. No UI.
pub mod detect;
pub mod model;

pub use detect::{SpecKind, spec_kind};
pub use model::{Api, Body, Operation, Parameter, ParseError, Response, TagGroup, parse, resolve};
