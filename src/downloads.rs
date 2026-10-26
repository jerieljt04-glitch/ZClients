//! Downloads go to ~/Downloads and are listed under the toolbar's download
//! button, with progress, Cancel and Retry. PDFs open as a ZClients tab, not
//! in another app — but only files that really are PDFs (`pdf.rs` checks the
//! bytes) and that carry the quarantine flag. Apps, installers, disk images,
//! scripts, archives and macro documents ask before they are saved.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSProgressReporting, NSString, NSURL, NSURLResponse};
use objc2_web_kit::{WKDownload, WKDownloadDelegate};
use uuid::Uuid;

use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Downloading,
    Done,
    Failed,
}

pub struct Item {
    key: usize,
    pub name: String,
    pub path: Option<PathBuf>,
    pub state: State,
    /// Why it failed, or why it was not opened (shown in the list).
    pub note: Option<String>,
    /// The server's MIME type, when known.
    mime: Option<String>,
    /// The profile whose page started it; a PDF opens as a tab there.
    client: Option<Uuid>,
    source_url: Option<String>,
    /// The tab showing this PDF, so picking it again goes back there.
    tab: Option<Uuid>,
}

impl Item {
    /// Stable for the item's whole life, unlike its place in the list
    /// (new downloads go to the top). Menus refer to items by this.
    pub fn key(&self) -> usize {
        self.key
    }

    fn new(key: usize, name: String, path: Option<PathBuf>, state: State, client: Option<Uuid>) -> Self {
        Item { key, name, path, state, note: None, mime: None, client, source_url: None, tab: None }
    }
}

/// Keys for downloads the app saves itself. Odd, so they never collide with
/// a WKDownload's (aligned) address.
fn own_key() -> usize {
    NEXT_KEY.with(|n| {
        let k = n.get();
        n.set(k + 2);
        k
    })
}

pub fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Kinds of file that can run code or hide other files, which always ask
/// before they are saved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Risk {
    App,
    Installer,
    DiskImage,
    Script,
    Archive,
    MacroDocument,
}

impl Risk {
    pub fn describe(self) -> &'static str {
        match self {
            Risk::App => "an app or program",
            Risk::Installer => "an installer",
            Risk::DiskImage => "a disk image",
            Risk::Script => "a script",
            Risk::Archive => "an archive (it may contain other files)",
            Risk::MacroDocument => "a document that can contain macros",
        }
    }
}

pub fn risk_of(name: &str) -> Option<Risk> {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "app" | "exe" | "msi" | "com" | "scr" | "bat" | "cmd" | "jar" | "apk" | "dll" | "so" | "dylib" | "bin" | "run" | "elf" | "msix" | "appx" => Risk::App,
        "pkg" | "mpkg" | "deb" | "rpm" => Risk::Installer,
        "dmg" | "iso" | "img" | "sparseimage" | "sparsebundle" | "vhd" | "vmdk" => Risk::DiskImage,
        "sh" | "command" | "zsh" | "bash" | "csh" | "ps1" | "psm1" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "hta" | "scpt" | "applescript" | "workflow" | "terminal" | "py"
        | "rb" | "pl" | "php" | "lnk" | "url" | "webloc" | "inetloc" | "reg" => Risk::Script,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "tbz" | "xz" | "txz" | "zst" | "cab" | "lha" | "lzh" | "sit" | "sitx" | "cpio" | "xip" => Risk::Archive,
        "docm" | "dotm" | "xlsm" | "xltm" | "xlam" | "pptm" | "potm" | "ppam" | "ppsm" | "sldm" | "xla" | "xlm" => Risk::MacroDocument,
        _ => return None,
    })
}

thread_local! {
    /// WebKit downloads by key, for progress and Cancel.
    static LIVE: RefCell<HashMap<usize, Retained<WKDownload>>> = RefCell::new(HashMap::new());
}

/// The item with this key, if it is still listed.
fn find<R>(key: usize, f: impl FnOnce(&Item) -> R) -> Option<R> {
    ITEMS.with_borrow(|v| v.iter().find(|i| i.key == key).map(f))
}

/// Fraction done (0..1) of a download still running, when known.
pub fn progress(key: usize) -> Option<f64> {
    find(key, |i| i.state == State::Downloading).filter(|d| *d)?;
    LIVE.with_borrow(|m| m.get(&key).map(|d| d.progress().fractionCompleted()))
}

/// Stop a running download and delete what it wrote so far: WebKit writes
/// to the final name, so a partial file would look complete.
pub fn cancel(key: usize) -> Option<PathBuf> {
    if find(key, |i| i.state == State::Downloading) != Some(true) {
        return None;
    }
    let partial = path_of(key);
    if let Some(d) = LIVE.with_borrow_mut(|m| m.remove(&key)) {
        unsafe { d.cancel(None) };
    }
    if let Some(p) = &partial {
        let _ = std::fs::remove_file(p);
    }
    update(key, |i| {
        i.state = State::Failed;
        i.note = Some("Cancelled".into());
    });
    partial
}

/// Cancel every running download (quitting). Returns the partial files,
/// already deleted once, so the caller can make sure just before exit.
pub fn cancel_all() -> Vec<PathBuf> {
    let keys: Vec<usize> = ITEMS.with_borrow(|v| v.iter().filter(|i| i.state == State::Downloading).map(|i| i.key).collect());
    keys.into_iter().filter_map(cancel).collect()
}

/// The address to download again, for Retry.
pub fn retry_url(key: usize) -> Option<(Option<Uuid>, String)> {
    find(key, |i| (i.state == State::Failed).then(|| Some((i.client, i.source_url.clone()?)))).flatten().flatten()
}

/// Where the item's file is (or will be).
pub fn path_of(key: usize) -> Option<PathBuf> {
    find(key, |i| i.path.clone()).flatten()
}

/// Explain on the item why an action could not be done.
pub fn set_note(key: usize, note: &str) {
    update(key, |i| i.note = Some(note.to_string()));
}

/// A download that becomes a PDF tab of `client` when it finishes.
struct Preview {
    client: Uuid,
    source_url: Option<String>,
    name: String,
    path: Option<PathBuf>,
}

thread_local! {
    static ITEMS: RefCell<Vec<Item>> = const { RefCell::new(Vec::new()) };
    static PREVIEWS: RefCell<HashMap<usize, Preview>> = RefCell::new(HashMap::new());
    static NEXT_KEY: Cell<usize> = const { Cell::new(1) };
    static DELEGATE: RefCell<Option<Retained<DownloadDelegate>>> = const { RefCell::new(None) };
}

const MAX_ITEMS: usize = 60;

pub fn with_items<R>(f: impl FnOnce(&[Item]) -> R) -> R {
    ITEMS.with_borrow(|v| f(v))
}

pub fn active_count() -> usize {
    ITEMS.with_borrow(|v| v.iter().filter(|i| i.state == State::Downloading).count())
}

pub fn clear_finished() {
    ITEMS.with_borrow_mut(|v| v.retain(|i| i.state == State::Downloading));
    crate::app::changed(crate::app::TOOLBAR);
}

fn push(item: Item) {
    ITEMS.with_borrow_mut(|v| {
        v.insert(0, item);
        while v.len() > MAX_ITEMS {
            match v.iter().rposition(|i| i.state != State::Downloading) {
                Some(i) => {
                    v.remove(i);
                }
                None => break,
            }
        }
    });
    crate::app::changed(crate::app::TOOLBAR);
}

fn update(key: usize, f: impl FnOnce(&mut Item)) {
    ITEMS.with_borrow_mut(|v| {
        if let Some(i) = v.iter_mut().find(|i| i.key == key) {
            f(i);
        }
    });
    crate::app::changed(crate::app::TOOLBAR);
}

/// A server controls the suggested name, so reduce it to one plain file name:
/// no directories, no leading dots, no colons, bounded length.
pub fn safe_filename(name: &str) -> String {
    // Control characters, and the invisible direction marks that can make
    // "invoice\u{202E}fdp.exe" display as "invoiceexe.pdf", are dropped.
    let visible: String = name.chars().filter(|c| !c.is_control() && !is_direction_mark(*c)).collect();
    let last = visible.rsplit('/').find(|s| !s.is_empty()).unwrap_or("").to_string();
    let mut f = last.replace(':', "-").trim().to_string();
    while f.starts_with('.') {
        f.remove(0);
    }
    let f = shorten_keeping_extension(f.trim(), MAX_NAME_BYTES);
    if f.is_empty() { "Download".into() } else { f }
}

/// Longest file name handed to the file system (APFS allows 255 bytes).
const MAX_NAME_BYTES: usize = 200;

/// Shorten a long name from the end of its stem, never its extension:
/// cutting the end turned "…quarterly report.xlsx" into "…quarterly rep",
/// a file nothing would open (and whose type the risk check could not see).
fn shorten_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 16 && ext.chars().all(|c| c.is_alphanumeric()) => (stem, Some(ext)),
        _ => (name, None),
    };
    let room = max - ext.map_or(0, |e| e.len() + 1);
    let mut stem = stem.to_string();
    while stem.len() > room {
        stem.pop();
    }
    let stem = stem.trim_end();
    match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem.to_string(),
    }
}

/// A free name in `dir`. Files still downloading count as taken: WebKit only
/// creates them as bytes arrive, and two downloads given the same path fail.
fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let in_flight: Vec<PathBuf> = ITEMS.with_borrow(|v| v.iter().filter(|i| i.state == State::Downloading).filter_map(|i| i.path.clone()).collect());
    let taken = |p: &Path| p.exists() || in_flight.iter().any(|f| f == p);
    let candidate = dir.join(name);
    if !taken(&candidate) {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str());
    for n in 1..1000 {
        let file = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let c = dir.join(file);
        if !taken(&c) {
            return c;
        }
    }
    dir.join(format!("{stem} ({})", uuid::Uuid::new_v4()))
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    pub struct DownloadDelegate;

    unsafe impl NSObjectProtocol for DownloadDelegate {}

    unsafe impl WKDownloadDelegate for DownloadDelegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn decide_destination(&self, download: &WKDownload, response: &NSURLResponse, suggested: &NSString, completion: &block2::DynBlock<dyn Fn(*mut NSURL)>) {
            let key = download as *const WKDownload as usize;
            LIVE.with_borrow_mut(|m| m.insert(key, objc2::Message::retain(download)));
            let mime = response.MIMEType().map(|m| m.to_string());
            // A PDF a page opened in a window: into its own private preview
            // folder, checked when it finishes.
            let preview = PREVIEWS.with_borrow_mut(|m| {
                let p = m.get_mut(&key)?;
                p.name = pdf_name(&suggested.to_string());
                let path = crate::pdf::new_preview_dir().ok()?.join(&p.name);
                p.path = Some(path.clone());
                Some(path)
            });
            if PREVIEWS.with_borrow(|m| m.contains_key(&key)) {
                match preview {
                    Some(path) => {
                        let url = NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy()));
                        completion.call((Retained::as_ptr(&url) as *mut NSURL,));
                    }
                    None => completion.call((std::ptr::null_mut(),)),
                }
                return;
            }
            let pdf_mime = mime.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("application/pdf"));
            let name = if pdf_mime { pdf_name(&suggested.to_string()) } else { safe_filename(&suggested.to_string()) };
            // The page's profile, so the PDF opens as a tab next to it.
            let client = unsafe { download.webView() }.and_then(|w| w.downcast_ref::<crate::web::ZWebView>().map(|z| z.ivars().client_id));
            let source_url = unsafe { download.originalRequest() }.and_then(|r| r.URL()).and_then(|u| util::url_string(&u)).filter(|u| u.starts_with("https:"));
            let origin = source_url.as_deref().and_then(crate::policy::parse).and_then(|u| crate::policy::Origin::of(&u)).map(|o| o.display());
            let finish = std::rc::Rc::new(completion.copy());
            let cancel = finish.clone();
            let place = move |name: String| {
                let path = unique_path(&util::downloads_dir(), &name);
                let url = NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy()));
                let mut item = Item::new(key, path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), Some(path), State::Downloading, client);
                item.source_url = source_url.clone();
                item.mime = mime.clone();
                push(item);
                // WebKit tags the file as downloaded from the web, so
                // Gatekeeper checks it on open; checked again on finish.
                finish.call((Retained::as_ptr(&url) as *mut NSURL,));
            };
            match risk_of(&name) {
                None => place(name),
                Some(Risk::Archive) if origin.as_deref().is_some_and(|o| crate::settings::download_permission(o) == crate::settings::SitePermission::Allow) => place(name),
                Some(risk) => {
                    // Asked on the next turn: never inside WebKit's callback.
                    util::later(move || {
                        let from = origin.clone().unwrap_or_else(|| "a page".into());
                        let who = client.map(crate::web::profile_name).map(|p| format!("{p} — ")).unwrap_or_default();
                        let mut buttons = vec!["Download"];
                        // Archives (WorkDrive folders are zips) can be allowed
                        // for a whole site; programs and scripts always ask.
                        if risk == Risk::Archive && origin.is_some() {
                            buttons.push("Always Allow Archives from This Site");
                        }
                        buttons.push("Cancel");
                        let (choice, _) = crate::web::run_alert(
                            &format!("{who}Download “{name}”?"),
                            &format!("This file is {} from {from}. Only open it if you trust where it came from.", risk.describe()),
                            &buttons,
                            None,
                        );
                        let ok = choice + 1 < buttons.len();
                        if ok
                            && buttons[choice].starts_with("Always")
                            && let Some(o) = &origin
                        {
                            crate::settings::set_download_permission(o, crate::settings::SitePermission::Allow);
                        }
                        if ok {
                            place(name);
                        } else {
                            LIVE.with_borrow_mut(|m| m.remove(&key));
                            record_failure_note(&name, "Cancelled");
                            // No destination: WebKit cancels the download.
                            cancel.call((std::ptr::null_mut(),));
                        }
                    });
                }
            }
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, download: &WKDownload) {
            let key = download as *const WKDownload as usize;
            LIVE.with_borrow_mut(|m| m.remove(&key));
            if let Some(p) = PREVIEWS.with_borrow_mut(|m| m.remove(&key)) {
                let Some(path) = p.path else { return };
                match crate::pdf::adopt_downloaded(&path) {
                    Ok(()) => util::later(move || {
                        crate::app::open_pdf_tab(p.client, path, p.name, p.source_url);
                    }),
                    Err(e) => {
                        crate::pdf::remove_preview(&util::pdf_previews_dir(), &path);
                        record_failure_note(&p.name, &format!("Not opened: {e}"));
                    }
                }
                return;
            }
            let path = ITEMS.with_borrow(|v| v.iter().find(|i| i.key == key).and_then(|i| i.path.clone()));
            let quarantined = path.as_deref().is_some_and(ensure_quarantined);
            update(key, |i| {
                i.state = State::Done;
                if !quarantined {
                    i.note = Some("Could not be marked as downloaded from the internet; not opened automatically".into());
                }
            });
            if quarantined {
                util::later(move || {
                    show_pdf_where(|i| i.key == key);
                });
            } else {
                util::log("a finished download could not be quarantined");
            }
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(&self, download: &WKDownload, error: &NSError, _resume: Option<&objc2_foundation::NSData>) {
            let key = download as *const WKDownload as usize;
            LIVE.with_borrow_mut(|m| m.remove(&key));
            let reason = error.localizedDescription().to_string();
            util::log(format!("download failed: {reason}"));
            if let Some(p) = PREVIEWS.with_borrow_mut(|m| m.remove(&key)) {
                if let Some(path) = &p.path {
                    crate::pdf::remove_preview(&util::pdf_previews_dir(), path);
                }
                record_failure_note(&p.name, &reason);
                return;
            }
            // Never leave a partial file that looks complete.
            let partial = ITEMS.with_borrow(|v| v.iter().find(|i| i.key == key && i.state == State::Downloading).and_then(|i| i.path.clone()));
            if let Some(p) = partial {
                let _ = std::fs::remove_file(p);
            }
            update(key, |i| {
                if i.state == State::Downloading {
                    i.state = State::Failed;
                    i.note = Some(reason.clone());
                }
            });
        }
    }
);

pub fn delegate() -> Retained<DownloadDelegate> {
    DELEGATE.with_borrow_mut(|d| {
        d.get_or_insert_with(|| {
            let mtm = MainThreadMarker::new().expect("main thread");
            let this = DownloadDelegate::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

pub fn adopt(download: &WKDownload) {
    let d = delegate();
    unsafe { download.setDelegate(Some(ProtocolObject::from_ref(&*d))) };
}

/// Fetch a PDF into the previews folder, then open it as a tab of `client`.
pub fn adopt_as_pdf_tab(download: &WKDownload, client: Uuid, source_url: Option<String>) {
    let key = download as *const WKDownload as usize;
    PREVIEWS.with_borrow_mut(|m| m.insert(key, Preview { client, source_url, name: "Document.pdf".into(), path: None }));
    adopt(download);
}

/// A server-suggested name for a PDF tab: one plain file name ending in .pdf.
pub fn pdf_name(suggested: &str) -> String {
    let name = safe_filename(suggested);
    let lower = name.to_ascii_lowercase();
    // "Unknown" is WebKit's stand-in when a blob: PDF has no name.
    if matches!(lower.as_str(), "download" | "unknown" | "unknown.pdf") {
        "Document.pdf".into()
    } else if lower.ends_with(".pdf") {
        name
    } else {
        format!("{name}.pdf")
    }
}

/// Show the listed PDF with this key as a tab. False when it is not a
/// finished PDF, so the caller opens it some other way.
pub fn show_pdf(key: usize) -> bool {
    show_pdf_where(|i| i.key == key)
}

fn show_pdf_where(find: impl Fn(&Item) -> bool) -> bool {
    let found = ITEMS.with_borrow(|v| {
        let i = v.iter().find(|i| find(i)).filter(|i| i.state == State::Done && i.note.is_none())?;
        // Named .pdf or served as a PDF, *and* the bytes are a PDF.
        let path = i.path.clone().filter(|p| is_pdf(p) || i.mime.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("application/pdf")))?;
        crate::pdf::mime_allows_pdf(i.mime.as_deref()).then_some(())?;
        Some((i.key, path, i.name.clone(), i.client, i.source_url.clone(), i.tab))
    });
    let Some((key, path, name, client, source, tab)) = found else { return false };
    if let Err(e) = crate::pdf::validate(&path) {
        if e == crate::pdf::PdfError::NotPdf {
            update(key, |i| i.note = Some("Named like a PDF but is not one; not opened".into()));
        }
        return false;
    }
    if let (Some(c), Some(t)) = (client, tab)
        && crate::app::show_tab(c, t)
    {
        return true;
    }
    // Its own profile, or the one in front if that profile is gone.
    let active = crate::app::store(|s| s.active_client_id);
    let opened = client.and_then(|c| crate::app::open_downloaded_pdf(c, &path, &name, source.clone())).or_else(|| crate::app::open_downloaded_pdf(active?, &path, &name, source));
    match opened {
        Some(tab) => {
            ITEMS.with_borrow_mut(|v| {
                if let Some(i) = v.iter_mut().find(|i| i.key == key) {
                    i.tab = Some(tab);
                }
            });
            true
        }
        None => false,
    }
}

/// Save a copy of a file the app already has (a PDF tab) to ~/Downloads.
pub fn save_copy(src: &Path, suggested: &str) {
    match std::fs::read(src) {
        Ok(data) => save_bytes(data, suggested, None),
        Err(e) => {
            util::log(format!("could not read {}: {e}", src.display()));
            record_failure(suggested);
        }
    }
}

/// Bytes the app read itself (blob:/data: right-click downloads). WebKit's
/// own downloads are quarantined automatically; these get the same marker by
/// hand so they are not quietly more trusted than a normal download.
/// `client` is the page's profile: a PDF saved for it opens as its tab.
pub fn save_bytes(data: Vec<u8>, suggested: &str, client: Option<Uuid>) {
    if let Some(key) = save(data, suggested, client, None)
        && client.is_some()
    {
        util::later(move || {
            show_pdf_where(|i| i.key == key);
        });
    }
}

/// The page in `tab` is saving its own document (the PDF viewer's download
/// button). It is already on screen, so it is not opened again, and picking
/// it from the list goes back to that tab.
pub fn save_from_tab(data: Vec<u8>, suggested: &str, client: Uuid, tab: Option<Uuid>) {
    save(data, suggested, Some(client), tab);
}

fn save(data: Vec<u8>, suggested: &str, client: Option<Uuid>, tab: Option<Uuid>) -> Option<usize> {
    let path = unique_path(&util::downloads_dir(), &safe_filename(suggested));
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let key = own_key();
    match write_new_file(&path, &data) {
        Ok(()) if quarantine(&path) => {
            let mut item = Item::new(key, name, Some(path), State::Done, client);
            item.tab = tab;
            push(item);
            Some(key)
        }
        Ok(()) => {
            // A file the app wrote that macOS would not treat as
            // downloaded is removed rather than left more trusted than it is.
            let _ = std::fs::remove_file(&path);
            util::log("could not quarantine a saved file; removed it");
            record_failure_note(&name, "Could not be marked as downloaded from the internet");
            None
        }
        Err(e) => {
            util::log(format!("could not save {name}: {e}"));
            push(Item::new(key, name, None, State::Failed, None));
            None
        }
    }
}

pub fn record_failure(name: &str) {
    push(Item::new(own_key(), name.into(), None, State::Failed, None));
}

fn record_failure_note(name: &str, note: &str) {
    let mut item = Item::new(own_key(), name.into(), None, State::Failed, None);
    item.note = Some(note.into());
    push(item);
}

/// Create a new file (never through a link, never over an existing one).
fn write_new_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).custom_flags(util::O_NOFOLLOW).open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

const QUARANTINE: &std::ffi::CStr = c"com.apple.quarantine";

/// Mark a file as downloaded from the internet, so Gatekeeper and
/// Launch Services check it on open. False if that failed.
fn quarantine(path: &Path) -> bool {
    use std::ffi::CString;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let value = format!("0083;{:08x};ZClients;", stamp as u32);
    let Ok(p) = CString::new(path.to_string_lossy().as_bytes()) else { return false };
    // XATTR_NOFOLLOW = 1
    unsafe { setxattr(p.as_ptr(), QUARANTINE.as_ptr(), value.as_ptr().cast(), value.len(), 0, 1) == 0 }
}

fn has_quarantine(path: &Path) -> bool {
    use std::ffi::CString;
    let Ok(p) = CString::new(path.to_string_lossy().as_bytes()) else { return false };
    unsafe { getxattr(p.as_ptr(), QUARANTINE.as_ptr(), std::ptr::null_mut(), 0, 0, 1) > 0 }
}

/// WebKit normally quarantines its downloads; make sure, and add the flag
/// if it is missing. False only if the file cannot carry it.
fn ensure_quarantined(path: &Path) -> bool {
    has_quarantine(path) || quarantine(path)
}

unsafe extern "C" {
    fn setxattr(path: *const std::ffi::c_char, name: *const std::ffi::c_char, value: *const std::ffi::c_void, size: usize, position: u32, options: i32) -> i32;
    fn getxattr(path: *const std::ffi::c_char, name: *const std::ffi::c_char, value: *mut std::ffi::c_void, size: usize, position: u32, options: i32) -> isize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_names_are_reduced_to_one_plain_file() {
        assert_eq!(safe_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_filename("/"), "Download");
        assert_eq!(safe_filename(".."), "Download");
        assert_eq!(safe_filename(".hidden"), "hidden");
        assert_eq!(safe_filename("a:b.pdf"), "a-b.pdf");
        assert_eq!(safe_filename("report.pdf"), "report.pdf");
        assert_eq!(safe_filename("invoice\u{202E}fdp.exe"), "invoicefdp.exe", "no disguised extensions");
        assert_eq!(safe_filename("a\u{2066}b\u{200F}.zip"), "ab.zip");
        assert_eq!(safe_filename("line\nbreak\t.pdf"), "linebreak.pdf");
        assert_eq!(safe_filename("Отчёт 2026.pdf"), "Отчёт 2026.pdf", "ordinary non-Latin names stay");
    }

    #[test]
    fn long_names_keep_their_extension_and_their_risk() {
        let long = format!("{} report.xlsx", "Quarterly ".repeat(30));
        let short = safe_filename(&long);
        assert!(short.len() <= MAX_NAME_BYTES, "{}", short.len());
        assert!(short.ends_with(".xlsx"), "{short}");
        let exe = format!("{}.exe", "a".repeat(400));
        assert_eq!(risk_of(&safe_filename(&exe)), Some(Risk::App), "still recognised after shortening");
        let multibyte = format!("{}.pdf", "é".repeat(300));
        let m = safe_filename(&multibyte);
        assert!(m.len() <= MAX_NAME_BYTES && m.ends_with(".pdf"), "never splits a character");
        assert_eq!(safe_filename("short.pdf"), "short.pdf");
        let no_ext = "x".repeat(300);
        assert_eq!(safe_filename(&no_ext).len(), MAX_NAME_BYTES);
    }

    #[test]
    fn pdf_tab_names_end_in_pdf() {
        assert_eq!(pdf_name("Invoice-12.pdf"), "Invoice-12.pdf");
        assert_eq!(pdf_name("Invoice-12.PDF"), "Invoice-12.PDF");
        assert_eq!(pdf_name("statement"), "statement.pdf");
        assert_eq!(pdf_name(""), "Document.pdf");
        assert_eq!(pdf_name("Unknown"), "Document.pdf");
        assert_eq!(pdf_name("../x/report.pdf"), "report.pdf");
    }

    #[test]
    fn pdfs_are_recognised_by_extension() {
        assert!(is_pdf(Path::new("/x/Quote 12.PDF")));
        assert!(!is_pdf(Path::new("/x/Folder.zip")));
        assert!(!is_pdf(Path::new("/x/pdf")));
        assert_eq!(own_key() % 2, 1);
    }

    #[test]
    fn risky_types_are_recognised_whatever_the_case() {
        assert_eq!(risk_of("Setup.EXE"), Some(Risk::App));
        assert_eq!(risk_of("Install.pkg"), Some(Risk::Installer));
        assert_eq!(risk_of("disk.dmg"), Some(Risk::DiskImage));
        assert_eq!(risk_of("run.command"), Some(Risk::Script));
        assert_eq!(risk_of("Folder.zip"), Some(Risk::Archive));
        assert_eq!(risk_of("Budget.xlsm"), Some(Risk::MacroDocument));
        assert_eq!(risk_of("invoice.pdf"), None);
        assert_eq!(risk_of("report.xlsx"), None);
        assert_eq!(risk_of("noextension"), None);
        assert_eq!(risk_of(&safe_filename("invoice\u{202E}fdp.exe")), Some(Risk::App), "a disguised name is still caught");
    }

    #[test]
    fn saved_files_are_quarantined_or_removed() {
        let dir = std::env::temp_dir().join(format!("zc-q-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.pdf");
        write_new_file(&f, b"%PDF-1.4").unwrap();
        assert!(write_new_file(&f, b"again").is_err(), "never overwrites");
        assert!(!has_quarantine(&f));
        assert!(quarantine(&f));
        assert!(has_quarantine(&f));
        assert!(ensure_quarantined(&f));
        assert!(!quarantine(&dir.join("missing")), "a failure is reported, not ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn menu_actions_follow_the_item_not_its_place_in_the_list() {
        ITEMS.with_borrow_mut(|v| v.clear());
        let mut failed = Item::new(own_key(), "Report.pdf".into(), None, State::Failed, None);
        failed.source_url = Some("https://crm.zoho.com/report.pdf".into());
        let report = failed.key();
        ITEMS.with_borrow_mut(|v| v.insert(0, failed));
        // The menu was built here (Report.pdf at index 0); then a new
        // download arrives at the top before the user clicks Retry.
        ITEMS.with_borrow_mut(|v| v.insert(0, Item::new(own_key(), "Other.zip".into(), None, State::Failed, None)));
        assert_eq!(retry_url(report).map(|(_, u)| u).as_deref(), Some("https://crm.zoho.com/report.pdf"));
        assert!(path_of(report).is_none());
        let other = ITEMS.with_borrow(|v| v[0].key());
        assert!(retry_url(other).is_none(), "the new item has nothing to retry");
        assert!(retry_url(12345).is_none(), "a key no longer listed does nothing");
        ITEMS.with_borrow_mut(|v| v.clear());
    }

    #[test]
    fn names_still_downloading_are_not_handed_out_twice() {
        let dir = std::env::temp_dir().join(format!("zc-dl-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "Folder.zip");
        assert_eq!(first, dir.join("Folder.zip"));
        ITEMS.with_borrow_mut(|v| v.push(Item::new(1, "Folder.zip".into(), Some(first.clone()), State::Downloading, None)));
        assert_eq!(unique_path(&dir, "Folder.zip"), dir.join("Folder (1).zip"));
        ITEMS.with_borrow_mut(|v| v.clear());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
