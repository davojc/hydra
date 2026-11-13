//! hydraui's icon in the macOS Dock. Inside Hydra.app the bundle carries it (the Liquid Glass
//! icon on macOS 26+, see `scripts/macos/make-app.sh`), and setting an image here would cover
//! that with a flat picture, so only a bare binary sets one at startup. Windows embeds
//! `assets/hydra.ico` in the exe instead (`build.rs`).

/// The 1024 px icon on Apple's macOS grid, which `assets/make_icon.py` draws.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
static ICON_PNG: &[u8] = include_bytes!("../../../assets/hydra-macos.png");

/// Whether the executable at `exe` runs from an app bundle (`Something.app/Contents/MacOS/`).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn in_app_bundle(exe: &std::path::Path) -> bool {
    exe.parent().is_some_and(|dir| {
        dir.ends_with("Contents/MacOS")
            && dir
                .ancestors()
                .nth(2)
                .and_then(|app| app.extension())
                .is_some_and(|ext| ext == "app")
    })
}

/// Shows the app icon in the Dock and the app switcher when no bundle provides one.
#[cfg(target_os = "macos")]
pub fn set() {
    if std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe)) {
        return;
    }
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON_PNG);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    // SAFETY: on the main thread (`mtm`), with a valid image the app retains.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

/// Elsewhere the executable carries its icon.
#[cfg(not(target_os = "macos"))]
pub fn set() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_icon_is_a_1024_px_png() {
        assert_eq!(&ICON_PNG[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR: width then height, big-endian, after the signature and chunk header.
        let size = |at: usize| u32::from_be_bytes(ICON_PNG[at..at + 4].try_into().unwrap());
        assert_eq!((size(16), size(20)), (1024, 1024));
    }

    #[test]
    fn only_a_bundled_executable_leaves_the_icon_to_its_bundle() {
        use std::path::Path;
        assert!(in_app_bundle(Path::new(
            "/Applications/Hydra.app/Contents/MacOS/hydraui"
        )));
        assert!(!in_app_bundle(Path::new(
            "/Users/me/hydra/target/release/hydraui"
        )));
        assert!(!in_app_bundle(Path::new("/opt/Contents/MacOS/hydraui")));
    }
}
