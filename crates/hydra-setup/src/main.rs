//! hydra-setup: installs hydraui and hydra for the current user, and uninstalls them
//! (spec §7.7). It is the Windows installer; other platforms install another way.
#![windows_subsystem = "windows"]

#[cfg(windows)]
mod installer;

#[cfg(windows)]
fn main() {
    installer::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "hydra-setup is the Windows installer; on this platform build hydra and hydraui with \
         `cargo build --release -p hydra-cli -p hydraui` instead."
    );
    std::process::exit(2);
}
