//! hydraui's Projects (docs/superpowers/specs/2026-11-03-hydraui-projects-design.md): the
//! projects repo's files, milestone progress, numbering and the local check's rules. No UI.
pub mod front_matter;
pub mod model;
pub mod progress;
pub mod repo;

use std::path::Path;

pub use model::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Toml(String),
    #[error("{file}: {message}")]
    InFile { file: String, message: String },
    #[error("{name} is {bytes} bytes; resources can be at most 10 MB")]
    TooLarge { name: String, bytes: u64 },
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    fn in_file(path: &Path, e: impl std::fmt::Display) -> Error {
        Error::InFile {
            file: path.display().to_string(),
            message: e.to_string(),
        }
    }

    fn with_file(self, path: &Path) -> Error {
        match self {
            Error::Toml(message) => Error::InFile {
                file: path.display().to_string(),
                message,
            },
            other => other,
        }
    }
}

/// The number in an id like `PG-12` or `D-7`.
pub fn number_of(id: &str) -> Option<u32> {
    id.rsplit_once('-')?.1.parse().ok()
}

/// The next free id with `prefix` (spec §2), given every id in use: on the default branch and in
/// open changes.
pub fn next_id<'a>(prefix: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let top = taken
        .into_iter()
        .filter(|id| {
            id.rsplit_once('-')
                .is_some_and(|(p, _)| p.eq_ignore_ascii_case(prefix))
        })
        .filter_map(number_of)
        .max()
        .unwrap_or(0);
    format!("{prefix}-{}", top + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_follows_the_highest_number_with_that_prefix() {
        assert_eq!(next_id("PG", []), "PG-1");
        assert_eq!(next_id("PG", ["PG-2", "PG-10", "PG-3", "XY-40"]), "PG-11");
        assert_eq!(next_id("D", ["D-1", "pg-9"]), "D-2");
    }
}
