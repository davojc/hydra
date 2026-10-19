//! hydraui's layout files: environments and their terminals (`*.hydraui`), the recent list. No UI.
pub mod layout;
mod layouts;
mod recent;

pub use layout::{Context, FilesSpec, PaneSpec};
pub use layouts::{LAYOUT_EXT, free_path, layout_name, layouts_dir};
pub use recent::{
    MAX_RECENT, RecentEntry, forget_recent, read_recent, rename_recent, same_path, touch_recent,
};
