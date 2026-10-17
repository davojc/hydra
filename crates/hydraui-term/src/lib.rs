//! hydraui's terminal core: a shell in a ConPTY running in a hydra environment, its
//! `alacritty_terminal` grid, and the shell-integration events. Windows only; no UI.
pub mod command;
mod event;
pub mod init;
pub mod job;
pub mod osc;
mod pane;

/// The emulator, re-exported so the app renders with the same version.
pub use alacritty_terminal;
pub use event::{Listener, TermEvent};
pub use init::clear_pane_inits;
pub use osc::PromptState;
pub use pane::{GridSize, Pane, SpawnError, SpawnSpec};
