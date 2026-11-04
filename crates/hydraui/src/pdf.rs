//! PDF files in the file pane (spec §7.8): each page drawn as a picture by hayro, off the UI
//! thread, so the pane shows the pages as they come.
use std::path::Path;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

/// The largest PDF the file pane shows.
pub const MAX_PDF_BYTES: u64 = 100 * 1024 * 1024;
/// The most pages drawn; a longer PDF says how many more it has.
pub const MAX_PAGES: usize = 300;
/// Pages are drawn at 1.5 pixels a point, sharp enough to read when fitted to the pane.
const SCALE: f32 = 1.5;
/// The longest side a page is drawn at, whatever its size, so a poster doesn't take gigabytes.
const MAX_SIDE: f32 = 4000.0;

/// One drawn page: a PNG and its size in pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfPage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A PDF file's bytes; too big, unreadable or not a PDF can't be shown.
pub fn load(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_PDF_BYTES {
        return Err("over 100 MB".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if !is_pdf_bytes(&bytes) {
        return Err("not a PDF".into());
    }
    Ok(bytes)
}

/// Whether the bytes are a PDF: `%PDF-` near the start (readers allow some junk before it).
pub fn is_pdf_bytes(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

/// How many pixels a point a `width` by `height` point page is drawn at.
pub fn page_scale(width: f32, height: f32) -> f32 {
    let longest = width.max(height).max(1.0);
    SCALE.min(MAX_SIDE / longest)
}

/// Draws a PDF's pages in order: `count` is told how many there are, then `page` gets each;
/// either returns false to stop (the pane closed the file).
pub fn render(
    bytes: Vec<u8>,
    mut count: impl FnMut(usize) -> bool,
    mut page: impl FnMut(PdfPage) -> bool,
) -> Result<(), String> {
    let pdf = Pdf::new(bytes).map_err(|e| match e {
        LoadPdfError::Decryption(_) => "it is password-protected".to_string(),
        _ => "it doesn't parse".to_string(),
    })?;
    let pages = pdf.pages();
    if pages.is_empty() {
        return Err("it has no pages".into());
    }
    if !count(pages.len()) {
        return Ok(());
    }
    let cache = RenderCache::new();
    let interpreter = InterpreterSettings::default();
    let settings = RenderSettings::default();
    for p in pages.iter().take(MAX_PAGES) {
        let (w, h) = p.render_dimensions();
        let scale = page_scale(w, h);
        let pixmap = hayro::render(
            p,
            &cache,
            &interpreter,
            &settings,
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let (width, height) = (pixmap.width() as u32, pixmap.height() as u32);
        let png = pixmap.into_png().map_err(|e| e.to_string())?;
        if !page(PdfPage { png, width, height }) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF of `pages` 200 by 100 point pages, each with a filled box, its xref offsets worked out.
    fn tiny_pdf(pages: usize) -> Vec<u8> {
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + i)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                kids.join(" ")
            ),
            "<< /Length 21 >>\nstream\n0 0 1 rg 10 10 50 50 re f\nendstream".to_string(),
        ];
        for _ in 0..pages {
            objs.push(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 3 0 R >>".into(),
            );
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn a_pdf_is_known_by_its_bytes() {
        assert!(is_pdf_bytes(b"%PDF-1.7\n"));
        assert!(
            is_pdf_bytes(b"junk\n%PDF-1.4\n"),
            "some junk before is allowed"
        );
        assert!(!is_pdf_bytes(b"%PDF"));
        assert!(!is_pdf_bytes(b"not a pdf"));
        assert!(!is_pdf_bytes(b""));
    }

    #[test]
    fn a_file_that_isnt_a_pdf_cant_be_shown() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake.pdf");
        std::fs::write(&fake, "hello").unwrap();
        assert_eq!(load(&fake), Err("not a PDF".into()));
        let real = dir.path().join("real.pdf");
        std::fs::write(&real, tiny_pdf(1)).unwrap();
        assert_eq!(load(&real), Ok(tiny_pdf(1)));
        assert!(load(&dir.path().join("gone.pdf")).is_err());
    }

    #[test]
    fn pages_are_drawn_at_one_and_a_half_pixels_a_point_up_to_a_limit() {
        assert_eq!(page_scale(612.0, 792.0), 1.5);
        assert_eq!(
            page_scale(8000.0, 2000.0),
            0.5,
            "a poster is shrunk to 4000 px"
        );
        assert_eq!(page_scale(0.0, 0.0), 1.5);
    }

    #[test]
    fn each_page_is_drawn_in_order_after_the_count() {
        let mut count = None;
        let mut pages = Vec::new();
        render(
            tiny_pdf(2),
            |n| {
                count = Some(n);
                true
            },
            |p| {
                pages.push(p);
                true
            },
        )
        .unwrap();
        assert_eq!(count, Some(2));
        assert_eq!(pages.len(), 2);
        assert_eq!((pages[0].width, pages[0].height), (300, 150));
        assert!(pages[0].png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn drawing_stops_when_told() {
        let mut drawn = 0;
        render(
            tiny_pdf(3),
            |_| true,
            |_| {
                drawn += 1;
                false
            },
        )
        .unwrap();
        assert_eq!(drawn, 1);
        let mut drawn = 0;
        render(
            tiny_pdf(3),
            |_| false,
            |_| {
                drawn += 1;
                true
            },
        )
        .unwrap();
        assert_eq!(drawn, 0, "nothing is drawn once the file is closed");
    }

    #[test]
    fn a_broken_pdf_says_why() {
        let err = render(b"%PDF-1.4\ngarbage".to_vec(), |_| true, |_| true).unwrap_err();
        assert!(!err.is_empty());
    }
}
