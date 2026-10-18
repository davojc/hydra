//! hydra-setup's core (spec §7.7): the install itself is UI-free and tested with a fake
//! System; `dialog` is the Windows UI over it.
pub mod args;
#[cfg(windows)]
pub mod dialog;
pub mod install;
pub mod paths;
pub mod system;

/// `HYDRA_VERSION` at compile time (release builds), else `<Cargo version>-dev`.
pub const VERSION: &str = match option_env!("HYDRA_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};
