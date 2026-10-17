//! hydraui's terminal sets: `<HYDRA_HOME>/hydraui/sets/<name>.toml` files resolved into panes,
//! plus which sets are loaded and the session file. No UI.
pub mod file;
mod resolve;
mod spec;

pub use resolve::{Context, SetError, list_sets, load_set, parse_set, set_file, sets_dir};
pub use spec::{Node, PaneSpec, SetSpec, SplitDir, TabSpec};
