//! hydraui's terminal core: a shell in a ConPTY running in a hydra environment, its
//! `alacritty_terminal` grid, and the shell-integration events. Windows only; no UI.
pub mod command;
pub mod init;
pub mod osc;

pub use init::clear_pane_inits;
pub use osc::PromptState;
