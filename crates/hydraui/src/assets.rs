//! The app's assets: gpui-component's default icons, plus the file tree's file-type icons,
//! which the default set leaves out.
use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

gpui_kit_assets::icon_assets!(
    TreeIcons,
    [
        Folder,
        FolderOpen,
        File,
        FileBraces,
        FileText,
        FileCog,
        FileCode,
        FileTerminal,
        FileImage,
        FileLock
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match TreeIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = gpui_kit_assets::Assets.list(path)?;
        all.extend(TreeIcons.list(path)?);
        all.sort();
        all.dedup();
        Ok(all)
    }
}
