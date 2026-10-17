//! hydraui's terminal sets: `<HYDRA_HOME>/hydraui/sets/<name>.toml` files resolved into panes,
//! plus which sets are loaded and the session file. No UI.
pub mod file;
mod loaded;
mod resolve;
mod session;
mod spec;

pub use loaded::{LoadedSet, LoadedSets, TabId, file_modified};
pub use resolve::{Context, SetError, list_sets, load_set, parse_set, set_file, sets_dir};
pub use session::{Session, read_session, session_file, write_session};
pub use spec::{Node, PaneSpec, SetSpec, SplitDir, TabSpec};
