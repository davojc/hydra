//! hydraui's icon in the macOS Dock: a bare binary has no bundle to carry one, so the app sets it
//! at startup. Windows embeds `assets/hydra.ico` in the exe instead (`build.rs`).

/// The 512 px app icon, which `assets/make_icon.py` makes from the logo in `assets/logo`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
static ICON_PNG: &[u8] = include_bytes!("../../../assets/hydra.png");

/// Shows the app icon in the Dock and the app switcher.
#[cfg(target_os = "macos")]
pub fn set() {
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
    fn the_app_icon_is_a_512_px_png() {
        assert_eq!(&ICON_PNG[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR: width then height, big-endian, after the signature and chunk header.
        let size = |at: usize| u32::from_be_bytes(ICON_PNG[at..at + 4].try_into().unwrap());
        assert_eq!((size(16), size(20)), (512, 512));
    }
}
