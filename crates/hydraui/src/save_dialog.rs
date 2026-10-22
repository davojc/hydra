//! Save As for layout files (spec §7.9). On Windows the dialog knows the file type, so it
//! adds the extension when it's left off, and its "replace?" question is about the file that
//! will really be written. Elsewhere gpui's own Save panel is used and `with_extension` adds
//! it afterwards.
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;

/// The dialog's file type: its label, its pattern, and the extension added when none is typed.
const FILE_TYPE: (&str, &str, &str) = ("hydraui layout (*.hydraui)", "*.hydraui", "hydraui");

/// Shows the Save As dialog in `dir` with `name` filled in, owned by the window `owner` (an
/// HWND, 0 for none). `None` when cancelled. It blocks until closed: call it off the UI thread.
#[cfg(windows)]
pub fn prompt(dir: &Path, name: &str, owner: isize) -> Result<Option<PathBuf>, String> {
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

    let (label, pattern, ext) = FILE_TYPE;
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

/// `path` with the layout extension added when it has none, as the Windows dialog does.
pub fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        return path;
    }
    path.with_extension(FILE_TYPE.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_without_an_extension_gets_the_layout_one() {
        assert_eq!(
            with_extension(PathBuf::from("/w/my layout")),
            PathBuf::from("/w/my layout.hydraui")
        );
        assert_eq!(
            with_extension(PathBuf::from("/w/a.hydraui")),
            PathBuf::from("/w/a.hydraui")
        );
        // A typed extension is the user's choice, as in the Windows dialog.
        assert_eq!(
            with_extension(PathBuf::from("/w/a.toml")),
            PathBuf::from("/w/a.toml")
        );
    }

    #[test]
    fn the_file_type_names_its_extension() {
        let (label, pattern, ext) = FILE_TYPE;
        assert_eq!(ext, "hydraui");
        assert_eq!(pattern, format!("*.{ext}"));
        assert!(label.contains(pattern), "{label}");
    }
}
