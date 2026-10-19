//! Save As for layout files (spec §7.9). The dialog knows the file type, so Windows
//! adds the extension when it's left off, and its "replace?" question is about the file that
//! will really be written.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    Layout,
    Group,
}

impl SaveKind {
    /// The dialog's file type: its label, its pattern, and the extension added when none is
    /// typed.
    pub fn file_type(self) -> (&'static str, &'static str, &'static str) {
        match self {
            SaveKind::Layout => ("hydraui layout (*.hydraui)", "*.hydraui", "hydraui"),
            SaveKind::Group => ("Group file (*.toml)", "*.toml", "toml"),
        }
    }
}

/// Shows the Save As dialog in `dir` with `name` filled in, owned by the window `owner` (an
/// HWND, 0 for none). `None` when cancelled. It blocks until closed: call it off the UI thread.
pub fn prompt(
    kind: SaveKind,
    dir: &Path,
    name: &str,
    owner: isize,
) -> Result<Option<PathBuf>, String> {
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    use windows::core::{HRESULT, HSTRING, PCWSTR};

    let (label, pattern, ext) = kind.file_type();
    let (label, pattern) = (HSTRING::from(label), HSTRING::from(pattern));
    let failed = |e: windows::core::Error| format!("the Save As dialog failed: {e}");
    // SAFETY: plain COM calls on this thread; the strings outlive the calls that use them.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).map_err(failed)?;
        dialog
            .SetFileTypes(&[COMDLG_FILTERSPEC {
                pszName: PCWSTR(label.as_ptr()),
                pszSpec: PCWSTR(pattern.as_ptr()),
            }])
            .map_err(failed)?;
        dialog
            .SetDefaultExtension(&HSTRING::from(ext))
            .map_err(failed)?;
        if let Ok(folder) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir), None)
        {
            let _ = dialog.SetFolder(&folder);
        }
        dialog.SetFileName(&HSTRING::from(name)).map_err(failed)?;
        let owner = (owner != 0).then_some(HWND(owner as *mut _));
        if let Err(e) = dialog.Show(owner) {
            if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(failed(e));
        }
        let item = dialog.GetResult().map_err(failed)?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(failed)?;
        let path = raw.to_string();
        CoTaskMemFree(Some(raw.0 as _));
        Ok(Some(PathBuf::from(path.map_err(|e| failed(e.into()))?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_names_its_extension() {
        assert_eq!(SaveKind::Layout.file_type().2, "hydraui");
        assert_eq!(SaveKind::Group.file_type().2, "toml");
        for kind in [SaveKind::Layout, SaveKind::Group] {
            let (label, pattern, ext) = kind.file_type();
            assert_eq!(pattern, format!("*.{ext}"));
            assert!(label.contains(pattern), "{label}");
        }
    }
}
