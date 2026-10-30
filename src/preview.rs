//! File previews (PDFs, images, video, audio): validation and private storage.
//!
//! A file is only shown when its first bytes say what it is (`%PDF-`, a
//! JPEG/PNG/GIF/WebP/HEIC... signature, an MP4/QuickTime/MP3/WAV header), it
//! is a regular file reached without following a symlink, and it is no
//! larger than [`MAX_PREVIEW_BYTES`]. A name proves nothing: an HTML page
//! renamed `invoice.pdf` or `photo.jpg` is not opened. The preview copy is
//! named with an extension of the kind its bytes are, because WebKit picks a
//! file's type by its extension: bytes that are a PNG *and* HTML are shown
//! as a picture, never run as a page. SVG (which can carry scripts) is not a
//! preview kind.
//!
//! Each preview lives alone in a random folder:
//! `PDF Previews/<32 hex>/<name>`, folder 0700, file 0600. WebKit gets read
//! access to that one folder, never to the previews folder or anything
//! else. Folders no tab refers to are removed at launch and on quit. (The
//! folder keeps its 1.x name so older builds still find their PDFs.)

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::util;

/// Larger files are refused rather than copied into a preview; they stay in
/// the Downloads folder.
pub const MAX_PREVIEW_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewError {
    Missing,
    NotRegularFile,
    TooLarge,
    /// The bytes are not a kind the app shows.
    NotPreviewable,
    /// The bytes are one kind, the name another.
    WrongExtension,
    Io(String),
}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PreviewError::Missing => "the file is gone",
            PreviewError::NotRegularFile => "it is not a plain file",
            PreviewError::TooLarge => "it is larger than 200 MB",
            PreviewError::NotPreviewable => "it is not a PDF, picture, video or audio file",
            PreviewError::WrongExtension => "its name does not match its contents",
            PreviewError::Io(_) => "it could not be read",
        })
    }
}

/// What a preview tab shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Pdf,
    Image,
    Video,
    Audio,
}

impl Kind {
    /// The extensions a preview of this kind may have (lower case). WebKit
    /// shows each of them from a file URL.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Kind::Pdf => &["pdf"],
            Kind::Image => &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif"],
            Kind::Video => &["mp4", "m4v", "mov"],
            Kind::Audio => &["mp3", "m4a", "aac", "wav"],
        }
    }

    /// For VoiceOver and logs.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Pdf => "PDF",
            Kind::Image => "picture",
            Kind::Video => "video",
            Kind::Audio => "audio",
        }
    }
}

/// A file's kind and the extension its preview copy gets when its own name
/// has none of that kind's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Format {
    pub kind: Kind,
    pub ext: &'static str,
}

const fn fmt(kind: Kind, ext: &'static str) -> Option<Format> {
    Some(Format { kind, ext })
}

/// What the first bytes of a file (at least 12 when the file has them) say
/// it is.
pub fn format_of(head: &[u8]) -> Option<Format> {
    let at = |i: usize, sig: &[u8]| head.get(i..i + sig.len()) == Some(sig);
    if head.starts_with(b"%PDF-") {
        return fmt(Kind::Pdf, "pdf");
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return fmt(Kind::Image, "jpg");
    }
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return fmt(Kind::Image, "png");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return fmt(Kind::Image, "gif");
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return fmt(Kind::Image, "tiff");
    }
    if head.starts_with(b"RIFF") && at(8, b"WEBP") {
        return fmt(Kind::Image, "webp");
    }
    if head.starts_with(b"RIFF") && at(8, b"WAVE") {
        return fmt(Kind::Audio, "wav");
    }
    // ISO media (MP4, QuickTime, M4A, HEIC, AVIF): "ftyp" and a brand.
    if at(4, b"ftyp") {
        let brand = head.get(8..12)?;
        return match brand {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => fmt(Kind::Image, "heic"),
            b"avif" | b"avis" => fmt(Kind::Image, "avif"),
            b"M4A " | b"M4B " => fmt(Kind::Audio, "m4a"),
            b"qt  " => fmt(Kind::Video, "mov"),
            _ => fmt(Kind::Video, "mp4"),
        };
    }
    if head.starts_with(b"BM") && head.len() >= 14 {
        return fmt(Kind::Image, "bmp");
    }
    // MP3: an ID3 tag, or an MPEG audio frame sync (11 bits set).
    // FF FE / FF FF are a UTF-16 byte-order mark and padding, not audio; a
    // frame's layer bits are never 00.
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 && head[1] & 0x06 != 0 && head[1] < 0xFE) {
        return fmt(Kind::Audio, "mp3");
    }
    None
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// The kind a preview file is, by its name. Only meaningful for files that
/// passed [`validate`] (their name matches their bytes); used for labels.
pub fn kind_by_name(path: &Path) -> Option<Kind> {
    let ext = extension_of(path)?;
    [Kind::Pdf, Kind::Image, Kind::Video, Kind::Audio].into_iter().find(|k| k.extensions().contains(&ext.as_str()))
}

/// Is the response's MIME type consistent with something a preview shows?
/// A server that labels a file `text/html` (or SVG) is believed; generic
/// types defer to the bytes.
pub fn mime_allows_preview(mime: Option<&str>) -> bool {
    match mime.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()) {
        None => true,
        Some(m) => {
            matches!(
                m.as_str(),
                "" | "application/pdf" | "application/x-pdf" | "application/octet-stream" | "binary/octet-stream" | "application/download" | "application/force-download"
            ) || ((m.starts_with("image/") || m.starts_with("video/") || m.starts_with("audio/")) && !m.contains("svg"))
        }
    }
}

/// Does the MIME type say this is something a preview shows?
pub fn mime_is_previewable(mime: Option<&str>) -> bool {
    let m = mime.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()).unwrap_or_default();
    m == "application/pdf" || ((m.starts_with("image/") || m.starts_with("video/") || m.starts_with("audio/")) && !m.contains("svg"))
}

/// Open `path` without following a final symlink and check it is a regular
/// file of acceptable size whose bytes are a preview kind. The name is not
/// looked at. Returns the open file (positioned at 0) and its format.
pub fn sniff(path: &Path) -> Result<(std::fs::File, Format), PreviewError> {
    let mut f = std::fs::OpenOptions::new().read(true).custom_flags(util::O_NOFOLLOW).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::Missing,
        // ELOOP: the path is a symlink.
        _ if e.raw_os_error() == Some(62) => PreviewError::NotRegularFile,
        _ => PreviewError::Io(e.to_string()),
    })?;
    let meta = f.metadata().map_err(|e| PreviewError::Io(e.to_string()))?;
    if !meta.file_type().is_file() {
        return Err(PreviewError::NotRegularFile);
    }
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err(PreviewError::TooLarge);
    }
    let mut head = Vec::with_capacity(16);
    (&mut f).take(16).read_to_end(&mut head).map_err(|e| PreviewError::Io(e.to_string()))?;
    let format = format_of(&head).ok_or(PreviewError::NotPreviewable)?;
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(0)).map_err(|e| PreviewError::Io(e.to_string()))?;
    Ok((f, format))
}

/// [`sniff`], and the name's extension is one of the kind its bytes are:
/// what a file must be before a tab shows it.
pub fn open_validated(path: &Path) -> Result<(std::fs::File, Format), PreviewError> {
    let (f, format) = sniff(path)?;
    match extension_of(path) {
        Some(ext) if format.kind.extensions().contains(&ext.as_str()) => Ok((f, format)),
        _ => Err(PreviewError::WrongExtension),
    }
}

pub fn validate(path: &Path) -> Result<Kind, PreviewError> {
    open_validated(path).map(|(_, f)| f.kind)
}

/// A new, empty, private folder for one preview.
pub fn new_preview_dir() -> std::io::Result<PathBuf> {
    new_preview_dir_in(&util::pdf_previews_dir())
}

pub fn new_preview_dir_in(root: &Path) -> std::io::Result<PathBuf> {
    let dir = root.join(uuid::Uuid::new_v4().simple().to_string());
    // create_dir fails if anything (including a planted link) is there.
    std::fs::create_dir(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// A file name for inside a preview folder: the given name when its
/// extension already fits the bytes, else that name plus one that does.
pub fn preview_file_name(name: &str, format: Format) -> String {
    if format.kind == Kind::Pdf {
        return crate::downloads::pdf_name(name);
    }
    let name = crate::downloads::safe_filename(name);
    if extension_of(Path::new(&name)).is_some_and(|e| format.kind.extensions().contains(&e.as_str())) { name } else { format!("{name}.{}", format.ext) }
}

/// Copy a file whose bytes are a preview kind into its own new preview
/// folder, named to match those bytes. The source is read through the
/// descriptor that was checked, so swapping the file after the check
/// changes nothing.
pub fn copy_into_preview(src: &Path, name: &str) -> Result<PathBuf, PreviewError> {
    copy_into_preview_in(&util::pdf_previews_dir(), src, name)
}

pub fn copy_into_preview_in(root: &Path, src: &Path, name: &str) -> Result<PathBuf, PreviewError> {
    let (mut f, format) = sniff(src)?;
    let dir = new_preview_dir_in(root).map_err(|e| PreviewError::Io(e.to_string()))?;
    let dest = dir.join(preview_file_name(name, format));
    let result = (|| {
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(util::O_NOFOLLOW).open(&dest)?;
        let copied = std::io::copy(&mut (&mut f).take(MAX_PREVIEW_BYTES + 1), &mut out)?;
        if copied > MAX_PREVIEW_BYTES {
            return Err(std::io::Error::other("grew past the limit"));
        }
        out.sync_all()
    })();
    match result {
        Ok(()) => Ok(dest),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(PreviewError::Io(e.to_string()))
        }
    }
}

/// Check a file WebKit just wrote into a preview folder: valid, owned by its
/// folder alone (one link), and private.
pub fn adopt_downloaded(path: &Path) -> Result<(), PreviewError> {
    let (f, _) = open_validated(path)?;
    let meta = f.metadata().map_err(|e| PreviewError::Io(e.to_string()))?;
    if meta.nlink() != 1 {
        return Err(PreviewError::NotRegularFile);
    }
    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// The preview folder a tab's file lives in, if it is a well-formed one
/// directly inside `root`.
pub fn preview_dir_of(root: &Path, file: &Path) -> Option<PathBuf> {
    let dir = file.parent()?;
    let is_own = dir.parent() == Some(root) && dir.file_name().is_some_and(|n| n.len() == 32 && n.to_string_lossy().chars().all(|c| c.is_ascii_hexdigit()));
    is_own.then(|| dir.to_path_buf())
}

/// Remove a preview: its whole folder when it has one, else (an older flat
/// file) just the file. Never anything outside `root`.
pub fn remove_preview(root: &Path, file: &Path) {
    if let Some(dir) = preview_dir_of(root, file) {
        let _ = std::fs::remove_dir_all(dir);
    } else if file.parent() == Some(root) {
        let _ = std::fs::remove_file(file);
    }
}

/// Delete every preview no tab refers to. Returns how many were removed.
pub fn prune(root: &Path, keep: &HashSet<PathBuf>) -> usize {
    let keep_dirs: HashSet<PathBuf> = keep.iter().filter_map(|f| preview_dir_of(root, f)).collect();
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if keep.contains(&p) || keep_dirs.contains(&p) {
            continue;
        }
        let ok = if meta.file_type().is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        if ok.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Move a preview kept by an older build (a flat `<uuid>.pdf` beside all the
/// others) into its own private folder. Returns the new path, or None when
/// the file is not a valid preview (it is then removed).
pub fn migrate_flat(root: &Path, file: &Path, name: &str) -> Option<PathBuf> {
    if preview_dir_of(root, file).is_some() {
        return Some(file.to_path_buf());
    }
    if file.parent() != Some(root) {
        return None;
    }
    let moved = copy_into_preview_in(root, file, name).ok();
    let _ = std::fs::remove_file(file);
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("zc-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const PDF: &[u8] = b"%PDF-1.7\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n";

    #[test]
    fn html_renamed_to_pdf_is_refused() {
        let d = scratch();
        let p = d.join("invoice.pdf");
        std::fs::write(&p, b"<html><script>alert(1)</script>%PDF-</html>").unwrap();
        assert_eq!(validate(&p), Err(PreviewError::NotPreviewable));
        std::fs::write(&p, b"%PD").unwrap();
        assert_eq!(validate(&p), Err(PreviewError::NotPreviewable), "truncated / interrupted download");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(validate(&p), Err(PreviewError::NotPreviewable));
        std::fs::write(&p, PDF).unwrap();
        assert_eq!(validate(&p), Ok(Kind::Pdf));
        assert_eq!(validate(&d.join("gone.pdf")), Err(PreviewError::Missing));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn symlinks_and_directories_are_refused() {
        let d = scratch();
        let real = d.join("real.pdf");
        std::fs::write(&real, PDF).unwrap();
        let link = d.join("link.pdf");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(validate(&link), Err(PreviewError::NotRegularFile));
        std::fs::create_dir(d.join("dir.pdf")).unwrap();
        assert!(validate(&d.join("dir.pdf")).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn hard_linked_downloads_are_not_adopted() {
        let d = scratch();
        let a = d.join("a.pdf");
        std::fs::write(&a, PDF).unwrap();
        assert_eq!(adopt_downloaded(&a), Ok(()));
        std::fs::hard_link(&a, d.join("b.pdf")).unwrap();
        assert_eq!(adopt_downloaded(&a), Err(PreviewError::NotRegularFile));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn previews_get_private_folders_and_duplicate_names_never_collide() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let src = root.join("src.pdf");
        std::fs::write(&src, PDF).unwrap();
        let a = copy_into_preview_in(&root, &src, "Invoice.pdf").unwrap();
        let b = copy_into_preview_in(&root, &src, "Invoice.pdf").unwrap();
        assert_ne!(a.parent(), b.parent(), "same name, separate folders");
        assert_eq!(a.file_name().unwrap(), "Invoice.pdf");
        let dir_mode = std::fs::metadata(a.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(&a).unwrap().permissions().mode() & 0o777;
        assert_eq!((dir_mode, file_mode), (0o700, 0o600));
        assert!(preview_dir_of(&root, &a).is_some());
        assert!(preview_dir_of(&root, &src).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_sources_leave_no_folder_behind() {
        let root = scratch();
        let src = root.join("x.pdf");
        std::fs::write(&src, b"<html>").unwrap();
        assert!(copy_into_preview_in(&root, &src, "x.pdf").is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "only the source remains");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn orphans_are_pruned_and_referenced_previews_kept() {
        let root = scratch();
        let src = root.join("src.pdf");
        std::fs::write(&src, PDF).unwrap();
        let kept = copy_into_preview_in(&root, &src, "keep.pdf").unwrap();
        let orphan = copy_into_preview_in(&root, &src, "orphan.pdf").unwrap();
        std::fs::write(root.join("partial.pdf.download"), b"%PD").unwrap();
        let keep: HashSet<PathBuf> = [kept.clone()].into();
        let removed = prune(&root, &keep);
        assert_eq!(removed, 3, "orphan folder, flat source and interrupted download");
        assert!(kept.exists());
        assert!(!orphan.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn flat_legacy_previews_move_into_their_own_folder() {
        let root = scratch();
        let flat = root.join("3F2504E0-4F89-11D3-9A0C-0305E82C3301.pdf");
        std::fs::write(&flat, PDF).unwrap();
        let moved = migrate_flat(&root, &flat, "Quote.pdf").unwrap();
        assert!(!flat.exists());
        assert!(preview_dir_of(&root, &moved).is_some());
        let bad = root.join("bad.pdf");
        std::fs::write(&bad, b"<html>").unwrap();
        assert_eq!(migrate_flat(&root, &bad, "bad.pdf"), None);
        assert!(!bad.exists(), "an invalid legacy preview is dropped");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remove_preview_never_leaves_its_root() {
        let root = scratch();
        let outside = std::env::temp_dir().join(format!("zc-outside-{}.pdf", uuid::Uuid::new_v4()));
        std::fs::write(&outside, PDF).unwrap();
        remove_preview(&root, &outside);
        assert!(outside.exists());
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mime_types() {
        assert!(mime_allows_preview(Some("application/pdf")));
        assert!(mime_allows_preview(Some("application/octet-stream; charset=binary")));
        assert!(mime_allows_preview(None));
        assert!(mime_allows_preview(Some("image/jpeg")));
        assert!(mime_allows_preview(Some("video/mp4")));
        assert!(!mime_allows_preview(Some("text/html")));
        assert!(!mime_allows_preview(Some("image/svg+xml")));
        assert!(mime_is_previewable(Some("image/png")));
        assert!(mime_is_previewable(Some("audio/mpeg")));
        assert!(!mime_is_previewable(Some("application/octet-stream")));
        assert!(!mime_is_previewable(Some("image/svg+xml")));
    }

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1];
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R'];

    #[test]
    fn kinds_come_from_the_bytes() {
        let k = |b: &[u8]| format_of(b).map(|f| (f.kind, f.ext));
        assert_eq!(k(JPEG), Some((Kind::Image, "jpg")));
        assert_eq!(k(PNG), Some((Kind::Image, "png")));
        assert_eq!(k(b"GIF89a\x01\x00"), Some((Kind::Image, "gif")));
        assert_eq!(k(b"RIFF\x10\0\0\0WEBPVP8 "), Some((Kind::Image, "webp")));
        assert_eq!(k(b"RIFF\x10\0\0\0WAVEfmt "), Some((Kind::Audio, "wav")));
        assert_eq!(k(b"\0\0\0\x18ftypheic\0\0\0\0"), Some((Kind::Image, "heic")));
        assert_eq!(k(b"\0\0\0\x18ftypisom\0\0\0\0"), Some((Kind::Video, "mp4")));
        assert_eq!(k(b"\0\0\0\x14ftypqt  \0\0\0\0"), Some((Kind::Video, "mov")));
        assert_eq!(k(b"\0\0\0\x20ftypM4A \0\0\0\0"), Some((Kind::Audio, "m4a")));
        assert_eq!(k(b"ID3\x04\0\0\0\0\0\0"), Some((Kind::Audio, "mp3")));
        assert_eq!(k(&[0xFF, 0xFB, 0x90, 0x44]), Some((Kind::Audio, "mp3")));
        assert_eq!(k(PDF), Some((Kind::Pdf, "pdf")));
        for not in [&b"<svg xmlns='http://www.w3.org/2000/svg'>"[..], b"<html>", &[0xFF, 0xFE, b'h', 0], b"PK\x03\x04", b"", b"%PD"] {
            assert_eq!(format_of(not), None, "{not:?}");
        }
    }

    #[test]
    fn a_shown_file_must_be_named_as_what_its_bytes_are() {
        let d = scratch();
        let jpg = d.join("photo.jpg");
        std::fs::write(&jpg, JPEG).unwrap();
        assert_eq!(validate(&jpg), Ok(Kind::Image));
        // PNG bytes that are also HTML: named .html, WebKit would run it.
        let polyglot = d.join("photo.html");
        std::fs::write(&polyglot, [PNG, b"<script>alert(1)</script>"].concat()).unwrap();
        assert_eq!(validate(&polyglot), Err(PreviewError::WrongExtension));
        let mismatched = d.join("song.mp3");
        std::fs::write(&mismatched, PDF).unwrap();
        assert_eq!(validate(&mismatched), Err(PreviewError::WrongExtension));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn preview_copies_are_named_for_their_bytes() {
        let root = scratch();
        let src = root.join("download");
        std::fs::write(&src, [PNG, b"<script>alert(1)</script>"].concat()).unwrap();
        let shown = copy_into_preview_in(&root, &src, "page.html").unwrap();
        assert_eq!(shown.file_name().unwrap(), "page.html.png", "shown as a picture, never as a page");
        assert_eq!(validate(&shown), Ok(Kind::Image));
        std::fs::write(&src, JPEG).unwrap();
        let kept = copy_into_preview_in(&root, &src, "Receipt.JPEG").unwrap();
        assert_eq!(kept.file_name().unwrap(), "Receipt.JPEG");
        assert_eq!(kind_by_name(&kept), Some(Kind::Image));
        std::fs::write(&src, b"<svg onload='alert(1)'/>").unwrap();
        assert_eq!(copy_into_preview_in(&root, &src, "logo.svg"), Err(PreviewError::NotPreviewable));
        let _ = std::fs::remove_dir_all(root);
    }
}
