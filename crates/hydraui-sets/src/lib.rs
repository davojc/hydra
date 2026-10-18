//! hydraui's terminal sets: `<HYDRA_HOME>/hydraui/sets/<name>.toml` files resolved into panes,
//! plus which sets are loaded and the session file. No UI.
pub mod file;
mod files;
mod layouts;
mod loaded;
mod recent;
mod resolve;
mod session;
mod spec;

pub use files::{
    FileKind, SetRef, file_kind, group_name, load_set_file, read_layout, set_ref, write_atomic,
    write_layout, write_set_file,
};
pub use layouts::{
    LAYOUT_EXT, Opened, converted_path, free_path, layout_name, layouts_dir, migrate_session,
    open_layout_file,
};
pub use loaded::{LoadedSet, LoadedSets, TabId, file_modified};
pub use recent::{
    MAX_RECENT, RecentEntry, forget_recent, read_recent, rename_recent, same_path, touch_recent,
};
pub use resolve::{
    Context, SetError, list_sets, load_set, parse_set, resolve_loose, set_file, sets_dir,
};
pub use session::{Session, read_session, session_file, write_session};
pub use spec::{FilesSpec, Node, PaneSpec, SetSpec, SplitDir, TabSpec};
