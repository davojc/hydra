//! OS-specific pieces: credential store, shells, processes, private files.
pub mod fs;
pub mod keyring_store;
pub mod links;
#[cfg(target_os = "macos")]
pub mod macos_keychain;
pub mod process;
pub mod shell;
