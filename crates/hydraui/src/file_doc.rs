//! Reading and saving a file for the file pane (spec §7.8): too big or binary can't be shown,
//! line endings and a BOM come back on save, saves are atomic; and what a change on disk means.
use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
const SNIFF: usize = 8 * 1024;
const BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, PartialEq)]
pub struct FileDoc {
    /// The text with `\n` line endings and no BOM, as the editor holds it.
    pub text: String,
    /// The file used CRLF (judged by its first line ending).
    pub crlf: bool,
    pub bom: bool,
}

pub fn decode(bytes: &[u8]) -> Result<FileDoc, String> {
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    if body[..body.len().min(SNIFF)].contains(&0) {
        return Err("binary file".into());
    }
    let raw = std::str::from_utf8(body).map_err(|_| "binary file".to_string())?;
    let crlf = raw
        .find('\n')
        .is_some_and(|i| i > 0 && raw.as_bytes()[i - 1] == b'\r');
    Ok(FileDoc {
        text: raw.replace("\r\n", "\n"),
        crlf,
        bom,
    })
}

/// `\r\n` and lone `\r` as `\n`: the editor can hold them after a paste from Windows.
pub fn normalize(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

pub fn encode(text: &str, crlf: bool, bom: bool) -> Vec<u8> {
    let text = normalize(text);
    let mut out = Vec::with_capacity(text.len() + 3);
    if bom {
        out.extend_from_slice(BOM);
    }
    if crlf {
        out.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    out
}

pub fn load(path: &Path) -> Result<FileDoc, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err("over 5 MB".into());
    }
    decode(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// Writes a temporary file beside `path`, then replaces `path` with it; on any failure the
/// temporary file is removed and `path` is untouched.
pub fn save(path: &Path, doc: &FileDoc) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(format!("{} is read-only", path.display()));
    }
    let name = path.file_name().ok_or("no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.hydraui-save"));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&encode(&doc.text, doc.crlf, doc.bom))?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("can't save {}: {e}", path.display())
    })
}

/// The largest image the file pane shows.
pub const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Ico,
}

impl ImageKind {
    pub fn name(self) -> &'static str {
        match self {
            ImageKind::Png => "PNG",
            ImageKind::Jpeg => "JPEG",
            ImageKind::Gif => "GIF",
            ImageKind::Ico => "ICO",
        }
    }
}

/// Whether the file pane shows the file as a picture: `.png`, `.jpg`, `.jpeg`, `.gif` or
/// `.ico`. (An SVG is text: it opens in an editor, with the picture as its Rendered view.)
pub fn is_image(path: &Path) -> bool {
    matches!(
        ext(path).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "ico")
    )
}

pub fn is_svg(path: &Path) -> bool {
    ext(path).as_deref() == Some("svg")
}

/// An image's kind from its first bytes, whatever its name says.
pub fn image_kind(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some(ImageKind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if bytes.starts_with(b"\0\0\x01\0") {
        Some(ImageKind::Ico)
    } else {
        None
    }
}

/// An image file's bytes and kind; too big, unreadable or not a PNG, JPEG, GIF or ICO can't
/// be shown.
pub fn load_image(path: &Path) -> Result<(ImageKind, Vec<u8>), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err("over 50 MB".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let kind = image_kind(&bytes).ok_or("not a PNG, JPEG, GIF or ICO image")?;
    Ok((kind, bytes))
}

/// A file size as people read it: bytes, KB or MB.
pub fn size_label(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

pub fn is_markdown(path: &Path) -> bool {
    matches!(ext(path).as_deref(), Some("md" | "markdown"))
}

fn ext(path: &Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// The highlighter's language for the file; `"text"` (no highlighting) when unknown.
pub fn language(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase());
    if matches!(name.as_deref(), Some("makefile" | "gnumakefile")) {
        return "make";
    }
    if matches!(name.as_deref(), Some("dockerfile" | "containerfile")) {
        return "dockerfile";
    }
    let by_ext = match ext(path).as_deref() {
        Some("rs") => "rust",
        Some("toml") => "toml",
        Some("json" | "jsonc") => "json",
        Some("md" | "markdown") => "markdown",
        Some("js" | "mjs" | "cjs" | "jsx") => "javascript",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        Some("py") => "python",
        Some("sh" | "bash") => "bash",
        Some("yml" | "yaml") => "yaml",
        Some("css" | "scss") => "css",
        Some("html" | "htm") => "html",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "cc" | "cxx" | "hpp" | "hh") => "cpp",
        Some("cs") => "csharp",
        Some("tsp") => "typespec",
        Some("dockerfile" | "containerfile") => "dockerfile",
        Some("hcl" | "tf" | "tfvars" | "tofu") => "hcl",
        _ => "text",
    };
    // Variants like `Dockerfile.dev`, unless the suffix is a known type (`Dockerfile.md`).
    let variant = |prefix| name.as_deref().is_some_and(|n| n.starts_with(prefix));
    if by_ext == "text" && (variant("dockerfile.") || variant("containerfile.")) {
        return "dockerfile";
    }
    by_ext
}

/// Where the cursor goes after a reload: the same line and column (in UTF-16 units, as the
/// editor counts), clamped to the new text; past the end, the end of the last line.
pub fn cursor_after_reload(line: u32, column: u32, text: &str) -> (u32, u32) {
    let lines: Vec<&str> = text.split('\n').collect();
    let width = |l: &str| l.encode_utf16().count() as u32;
    let last = lines.len() as u32 - 1;
    if line > last {
        return (last, width(lines[last as usize]));
    }
    (line, column.min(width(lines[line as usize])))
}

/// The editor position for a 1-based line and column (as tools print them), clamped to the
/// text; 0 counts as 1.
pub fn position_for(line: usize, column: usize, text: &str) -> (u32, u32) {
    cursor_after_reload(
        line.saturating_sub(1) as u32,
        column.saturating_sub(1) as u32,
        text,
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageUse {
    Local(PathBuf),
    Blocked,
}

/// What Rendered does with an image: a local file (relative to the Markdown file's folder)
/// loads; anything with a URL scheme, or protocol-relative, is never fetched (spec §7.8).
pub fn image_use(uri: &str, dir: &Path) -> ImageUse {
    let u = uri.trim();
    let has_scheme = u.find(':').is_some_and(|i| {
        // A drive letter (`C:`) is a path, not a scheme.
        i > 1
            && u[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    if u.is_empty() || has_scheme || u.starts_with("//") || u.starts_with("\\\\") {
        return ImageUse::Blocked;
    }
    let p = Path::new(u);
    ImageUse::Local(if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    })
}

/// Whether clicking a link in Rendered opens it: web and mail only; nothing runs (spec §7.8).
pub fn link_opens(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiskChange {
    Nothing,
    Reload(FileDoc),
    /// Changed on disk while there are unsaved edits (or it can't be read now).
    Changed,
    Deleted,
}

/// The disk-state marks on a file tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskFlags {
    /// Changed on disk while there were unsaved edits.
    pub changed: bool,
    pub deleted: bool,
}

/// The marks after a change on disk: a file that exists again isn't deleted, and a reload
/// clears both.
pub fn flags_after(f: DiskFlags, change: &DiskChange) -> DiskFlags {
    match change {
        DiskChange::Nothing => DiskFlags {
            deleted: false,
            ..f
        },
        DiskChange::Reload(_) => DiskFlags::default(),
        DiskChange::Changed => DiskFlags {
            changed: true,
            deleted: false,
        },
        DiskChange::Deleted => DiskFlags { deleted: true, ..f },
    }
}

/// What a change on disk means for an open file: `saved` is what was last loaded or saved,
/// `now` what is on disk (`None`: gone).
pub fn on_disk(saved: &FileDoc, dirty: bool, now: Option<Result<FileDoc, String>>) -> DiskChange {
    match now {
        None => DiskChange::Deleted,
        Some(Ok(doc)) if doc == *saved => DiskChange::Nothing,
        Some(Ok(doc)) if !dirty => DiskChange::Reload(doc),
        Some(_) => DiskChange::Changed,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn png_and_jpeg_files_show_as_pictures() {
        assert!(is_image(Path::new("a/shot.png")));
        assert!(is_image(Path::new("a/PHOTO.JPG")));
        assert!(is_image(Path::new("photo.jpeg")));
        assert!(is_image(Path::new("favicon.ICO")));
        assert!(is_image(Path::new("spinner.gif")));
        assert!(!is_image(Path::new("logo.svg")), "an SVG is text");
        assert!(!is_image(Path::new("png")));
        assert!(is_svg(Path::new("a/Logo.SVG")));
        assert!(!is_svg(Path::new("svg")));
    }

    #[test]
    fn an_images_kind_is_read_from_its_bytes() {
        assert_eq!(
            image_kind(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some(ImageKind::Png)
        );
        assert_eq!(
            image_kind(b"\xFF\xD8\xFF\xE0\0\x10JFIF"),
            Some(ImageKind::Jpeg)
        );
        assert_eq!(
            image_kind(b"\0\0\x01\0\x01\0\x10\x10"),
            Some(ImageKind::Ico)
        );
        assert_eq!(
            image_kind(b"\0\0\x02\0"),
            None,
            "a cursor file isn't an icon"
        );
        assert_eq!(image_kind(b"GIF89a\x01\0"), Some(ImageKind::Gif));
        assert_eq!(image_kind(b"GIF87a\x01\0"), Some(ImageKind::Gif));
        assert_eq!(image_kind(b"GIF8"), None);
        assert_eq!(image_kind(b"BM6\0"), None, "a BMP isn't shown");
        assert_eq!(image_kind(b""), None);
    }

    #[test]
    fn an_image_that_isnt_one_cant_be_shown() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake.png");
        std::fs::write(&fake, "not an image").unwrap();
        assert_eq!(
            load_image(&fake),
            Err("not a PNG, JPEG, GIF or ICO image".into())
        );
        let real = dir.path().join("real.jpg");
        std::fs::write(&real, b"\xFF\xD8\xFF\xE0rest").unwrap();
        assert_eq!(
            load_image(&real),
            Ok((ImageKind::Jpeg, b"\xFF\xD8\xFF\xE0rest".to_vec()))
        );
        assert!(load_image(&dir.path().join("gone.png")).is_err());
    }

    #[test]
    fn sizes_read_as_bytes_kb_or_mb() {
        assert_eq!(size_label(512), "512 bytes");
        assert_eq!(size_label(1024), "1 KB");
        assert_eq!(size_label(1500), "2 KB");
        assert_eq!(size_label(3 * 1024 * 1024 + 300 * 1024), "3.3 MB");
    }

    #[test]
    fn crlf_and_bom_are_recorded_and_stripped() {
        let doc = decode(b"\xEF\xBB\xBFone\r\ntwo\r\n").unwrap();
        assert_eq!(
            doc,
            FileDoc {
                text: "one\ntwo\n".into(),
                crlf: true,
                bom: true
            }
        );
        let doc = decode(b"one\ntwo").unwrap();
        assert_eq!(
            doc,
            FileDoc {
                text: "one\ntwo".into(),
                crlf: false,
                bom: false
            }
        );
    }

    #[test]
    fn a_crlf_bom_file_round_trips_with_only_the_edit_changed() {
        let original = b"\xEF\xBB\xBFline 1\r\nline 2\r\nline 3\r\n";
        let mut doc = decode(original).unwrap();
        doc.text = doc.text.replace("line 2", "LINE TWO");
        assert_eq!(
            encode(&doc.text, doc.crlf, doc.bom),
            b"\xEF\xBB\xBFline 1\r\nLINE TWO\r\nline 3\r\n".to_vec()
        );
    }

    #[test]
    fn binary_and_invalid_utf8_cant_be_shown() {
        assert_eq!(decode(b"abc\0def").unwrap_err(), "binary file");
        assert_eq!(decode(&[0xff, 0xfe, 0x41]).unwrap_err(), "binary file");
    }

    #[test]
    fn big_files_cant_be_shown() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("big.txt");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_BYTES + 1).unwrap();
        assert!(load(&p).unwrap_err().contains("5 MB"));
        assert!(load(&d.path().join("missing.txt")).is_err());
    }

    #[test]
    fn save_writes_the_endings_back_atomically() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "old\r\n").unwrap();
        let doc = FileDoc {
            text: "new\nline\n".into(),
            crlf: true,
            bom: false,
        };
        save(&p, &doc).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new\r\nline\r\n");
        let leftovers: Vec<_> = std::fs::read_dir(d.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temp file left");
    }

    #[test]
    fn saving_onto_a_read_only_file_fails_and_leaves_it_alone() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ro.txt");
        std::fs::write(&p, "keep").unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms.clone()).unwrap();
        let doc = FileDoc {
            text: "changed".into(),
            crlf: false,
            bom: false,
        };
        assert!(save(&p, &doc).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "keep");
        assert_eq!(
            std::fs::read_dir(d.path()).unwrap().count(),
            1,
            "no temp file left"
        );
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&p, perms).unwrap();
    }

    #[test]
    fn markdown_and_languages_come_from_the_extension() {
        assert!(is_markdown(Path::new("README.md")));
        assert!(is_markdown(Path::new("notes.MARKDOWN")));
        assert!(!is_markdown(Path::new("main.rs")));
        assert_eq!(language(Path::new("main.rs")), "rust");
        assert_eq!(language(Path::new("Cargo.toml")), "toml");
        assert_eq!(language(Path::new("a.ps1")), "text");
        assert_eq!(language(Path::new("Makefile")), "make");
        assert_eq!(language(Path::new("x.tsx")), "tsx");
        assert_eq!(language(Path::new("main.tsp")), "typespec");
        assert_eq!(language(Path::new("Dockerfile")), "dockerfile");
        assert_eq!(language(Path::new("Containerfile")), "dockerfile");
        assert_eq!(language(Path::new("app.dockerfile")), "dockerfile");
        assert_eq!(language(Path::new("Dockerfile.dev")), "dockerfile");
        assert_eq!(language(Path::new("Dockerfile.md")), "markdown");
        assert_eq!(language(Path::new("dockerfile-notes.txt")), "text");
        assert_eq!(language(Path::new("main.tf")), "hcl");
        assert_eq!(language(Path::new("prod.tfvars")), "hcl");
        assert_eq!(language(Path::new("config.hcl")), "hcl");
        assert_eq!(language(Path::new("main.tofu")), "hcl");
    }

    fn doc(t: &str) -> FileDoc {
        FileDoc {
            text: t.into(),
            crlf: false,
            bom: false,
        }
    }

    #[test]
    fn our_own_save_seen_on_disk_does_nothing() {
        assert_eq!(
            on_disk(&doc("a"), false, Some(Ok(doc("a")))),
            DiskChange::Nothing
        );
        assert_eq!(
            on_disk(&doc("a"), true, Some(Ok(doc("a")))),
            DiskChange::Nothing
        );
    }

    #[test]
    fn a_change_on_disk_reloads_unless_there_are_edits() {
        assert_eq!(
            on_disk(&doc("a"), false, Some(Ok(doc("b")))),
            DiskChange::Reload(doc("b"))
        );
        assert_eq!(
            on_disk(&doc("a"), true, Some(Ok(doc("b")))),
            DiskChange::Changed
        );
        assert_eq!(on_disk(&doc("a"), false, None), DiskChange::Deleted);
        assert_eq!(
            on_disk(&doc("a"), true, Some(Err("binary file".into()))),
            DiskChange::Changed
        );
    }

    #[test]
    fn pasted_windows_line_endings_dont_double_up_on_save() {
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(encode("a\r\nb\n", true, false), b"a\r\nb\r\n".to_vec());
        assert_eq!(encode("a\r\nb\n", false, false), b"a\nb\n".to_vec());
    }

    #[test]
    fn only_local_images_load() {
        let dir = Path::new("C:/docs");
        assert_eq!(
            image_use("img/a.png", dir),
            ImageUse::Local(dir.join("img/a.png"))
        );
        assert_eq!(
            image_use("C:/pics/b.png", dir),
            ImageUse::Local(PathBuf::from("C:/pics/b.png"))
        );
        for remote in [
            "https://example.com/a.png",
            "HTTPS://example.com/a.png",
            " http://example.com/a.png",
            "ftp://example.com/a.png",
            "//example.com/a.png",
            "data:image/png;base64,AAAA",
        ] {
            assert_eq!(image_use(remote, dir), ImageUse::Blocked, "{remote}");
        }
    }

    #[test]
    fn links_open_only_for_web_and_mail() {
        assert!(link_opens("https://example.com"));
        assert!(link_opens("HTTP://example.com"));
        assert!(link_opens("mailto:a@b.c"));
        assert!(!link_opens("file:///C:/evil.exe"));
        assert!(!link_opens("ms-settings:"));
        assert!(!link_opens("C:\\Windows\\notepad.exe"));
    }

    #[test]
    fn a_file_back_on_disk_is_no_longer_deleted() {
        let deleted = DiskFlags {
            changed: false,
            deleted: true,
        };
        assert_eq!(
            flags_after(deleted, &DiskChange::Nothing),
            DiskFlags {
                changed: false,
                deleted: false
            }
        );
        assert_eq!(
            flags_after(deleted, &DiskChange::Changed),
            DiskFlags {
                changed: true,
                deleted: false
            }
        );
        let changed = DiskFlags {
            changed: true,
            deleted: false,
        };
        assert_eq!(
            flags_after(changed, &DiskChange::Reload(doc("x"))),
            DiskFlags::default()
        );
        assert_eq!(
            flags_after(changed, &DiskChange::Deleted),
            DiskFlags {
                changed: true,
                deleted: true
            }
        );
    }

    #[test]
    fn the_cursor_keeps_its_place_after_a_reload() {
        let text = "one\ntwo\nthree";
        assert_eq!(cursor_after_reload(1, 2, text), (1, 2));
        assert_eq!(
            cursor_after_reload(1, 9, text),
            (1, 3),
            "column clamps to the line"
        );
        assert_eq!(
            cursor_after_reload(7, 1, text),
            (2, 5),
            "past the end: end of the last line"
        );
        assert_eq!(
            cursor_after_reload(0, 4, "é€x"),
            (0, 3),
            "columns count UTF-16 units"
        );
        assert_eq!(cursor_after_reload(3, 3, ""), (0, 0));
    }

    #[test]
    fn a_line_and_column_become_an_editor_position() {
        assert_eq!(position_for(1, 1, "one\ntwo"), (0, 0));
        assert_eq!(position_for(2, 3, "one\ntwo"), (1, 2));
        assert_eq!(position_for(0, 0, "one"), (0, 0), "0 means the start");
    }

    #[test]
    fn a_line_past_the_end_goes_to_the_last_line() {
        assert_eq!(position_for(99, 1, "one\ntwo"), (1, 3));
        assert_eq!(position_for(2, 99, "one\ntwo"), (1, 3));
    }
}
