//! hydraui's terminal sets: `<HYDRA_HOME>/hydraui/sets/<name>.toml` files resolved into panes,
//! plus which sets are loaded and the session file. No UI.
pub mod file;
mod files;
mod loaded;
mod resolve;
mod session;
mod spec;

pub use files::{
    FileKind, SetRef, file_kind, group_name, load_set_file, read_layout, set_ref, write_atomic,
    write_layout, write_set_file,
};
pub use loaded::{LoadedSet, LoadedSets, TabId, file_modified};
pub use resolve::{
    Context, SetError, list_sets, load_set, parse_set, resolve_loose, set_file, sets_dir,
};
pub use session::{Session, read_session, session_file, write_session};
pub use spec::{FilesSpec, Node, PaneSpec, SetSpec, SplitDir, TabSpec};
