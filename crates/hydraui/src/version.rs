//! hydraui's version (spec §7.11): the release's in release builds, else `<Cargo>-dev`.

/// `HYDRA_VERSION` at compile time (release builds), else `<Cargo version>-dev`.
pub const VERSION: &str = match option_env!("HYDRA_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};
