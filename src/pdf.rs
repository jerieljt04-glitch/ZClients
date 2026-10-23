//! PDF previews: validation and private storage.
//!
//! A file is only treated as a PDF when its bytes say so (`%PDF-` at the
//! start), it is a regular file reached without following a symlink, and it
//! is no larger than [`MAX_PDF_BYTES`]. A name ending in `.pdf` proves
//! nothing: an HTML page renamed `invoice.pdf` is not opened.
//!
//! Each preview lives alone in a random folder:
//! `PDF Previews/<32 hex>/<name>.pdf`, folder 0700, file 0600. WebKit gets
//! read access to that one folder, never to the previews folder or anything
//! else. Folders no tab refers to are removed at launch and on quit.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::util;

/// Larger "PDFs" are refused rather than read into a preview.
pub const MAX_PDF_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum PdfError {
    Missing,
    NotRegularFile,
    TooLarge,
    NotPdf,
    Io(String),
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PdfError::Missing => "the file is gone",
            PdfError::NotRegularFile => "it is not a plain file",
            PdfError::TooLarge => "it is larger than 200 MB",
            PdfError::NotPdf => "it is not a PDF document",
            PdfError::Io(_) => "it could not be read",
        })
    }
}

/// The bytes start like a PDF.
pub fn has_pdf_magic(head: &[u8]) -> bool {
    head.starts_with(b"%PDF-")
}

/// Is the response's MIME type consistent with a PDF? A server that labels
/// a file `text/html` is believed; generic types defer to the bytes.
pub fn mime_allows_pdf(mime: Option<&str>) -> bool {
    match mime.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()) {
        None => true,
        Some(m) => matches!(
            m.as_str(),
            "" | "application/pdf" | "application/x-pdf" | "application/octet-stream" | "binary/octet-stream" | "application/download" | "application/force-download"
        ),
    }
}

/// Open `path` without following a final symlink and check it is a regular
/// PDF of acceptable size. Returns the open file (positioned at 0).
pub fn open_validated(path: &Path) -> Result<std::fs::File, PdfError> {
    let mut f = std::fs::OpenOptions::new().read(true).custom_flags(util::O_NOFOLLOW).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PdfError::Missing,
        // ELOOP: the path is a symlink.
        _ if e.raw_os_error() == Some(62) => PdfError::NotRegularFile,
        _ => PdfError::Io(e.to_string()),
    })?;
    let meta = f.metadata().map_err(|e| PdfError::Io(e.to_string()))?;
    if !meta.file_type().is_file() {
        return Err(PdfError::NotRegularFile);
    }
    if meta.len() > MAX_PDF_BYTES {
        return Err(PdfError::TooLarge);
    }
    let mut head = [0u8; 5];
    f.read_exact(&mut head).map_err(|_| PdfError::NotPdf)?;
    if !has_pdf_magic(&head) {
        return Err(PdfError::NotPdf);
    }
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(0)).map_err(|e| PdfError::Io(e.to_string()))?;
    Ok(f)
}

pub fn validate(path: &Path) -> Result<(), PdfError> {
    open_validated(path).map(|_| ())
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

/// A file name for inside a preview folder.
pub fn preview_file_name(name: &str) -> String {
    crate::downloads::pdf_name(name)
}

/// Copy a validated PDF into its own new preview folder. The source is read
/// through the descriptor that was validated, so swapping the file after the
/// check changes nothing.
pub fn copy_into_preview(src: &Path, name: &str) -> Result<PathBuf, PdfError> {
    copy_into_preview_in(&util::pdf_previews_dir(), src, name)
}

pub fn copy_into_preview_in(root: &Path, src: &Path, name: &str) -> Result<PathBuf, PdfError> {
    let mut f = open_validated(src)?;
    let dir = new_preview_dir_in(root).map_err(|e| PdfError::Io(e.to_string()))?;
    let dest = dir.join(preview_file_name(name));
    let result = (|| {
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(util::O_NOFOLLOW).open(&dest)?;
        let copied = std::io::copy(&mut (&mut f).take(MAX_PDF_BYTES + 1), &mut out)?;
        if copied > MAX_PDF_BYTES {
            return Err(std::io::Error::other("grew past the limit"));
        }
        out.sync_all()
    })();
    match result {
        Ok(()) => Ok(dest),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(PdfError::Io(e.to_string()))
        }
    }
}

/// Check a file WebKit just wrote into a preview folder: valid, owned by its
/// folder alone (one link), and private.
pub fn adopt_downloaded(path: &Path) -> Result<(), PdfError> {
    let f = open_validated(path)?;
    let meta = f.metadata().map_err(|e| PdfError::Io(e.to_string()))?;
    if meta.nlink() != 1 {
        return Err(PdfError::NotRegularFile);
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
/// the file is not a valid PDF (it is then removed).
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
        assert_eq!(validate(&p), Err(PdfError::NotPdf));
        std::fs::write(&p, b"%PD").unwrap();
        assert_eq!(validate(&p), Err(PdfError::NotPdf), "truncated / interrupted download");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(validate(&p), Err(PdfError::NotPdf));
        std::fs::write(&p, PDF).unwrap();
        assert_eq!(validate(&p), Ok(()));
        assert_eq!(validate(&d.join("gone.pdf")), Err(PdfError::Missing));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn symlinks_and_directories_are_refused() {
        let d = scratch();
        let real = d.join("real.pdf");
        std::fs::write(&real, PDF).unwrap();
        let link = d.join("link.pdf");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(validate(&link), Err(PdfError::NotRegularFile));
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
        assert_eq!(adopt_downloaded(&a), Err(PdfError::NotRegularFile));
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
        assert!(mime_allows_pdf(Some("application/pdf")));
        assert!(mime_allows_pdf(Some("application/octet-stream; charset=binary")));
        assert!(mime_allows_pdf(None));
        assert!(!mime_allows_pdf(Some("text/html")));
        assert!(!mime_allows_pdf(Some("image/svg+xml")));
    }
}
