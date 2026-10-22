//! Downloads go to ~/Downloads and are listed under the toolbar's download
//! button. PDFs open as a ZClients tab, not in another app: a downloaded PDF
//! is shown in a tab as soon as it finishes (and again when picked from the
//! list), and one a page opens in a window is fetched straight into the
//! previews folder and shown as a tab.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURLResponse, NSURL};
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
    /// The profile whose page started it; a PDF opens as a tab there.
    client: Option<Uuid>,
    source_url: Option<String>,
    /// The tab showing this PDF, so picking it again goes back there.
    tab: Option<Uuid>,
}

impl Item {
    fn new(key: usize, name: String, path: Option<PathBuf>, state: State, client: Option<Uuid>) -> Self {
        Item { key, name, path, state, client, source_url: None, tab: None }
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
    let last = name.replace('\0', "").rsplit('/').find(|s| !s.is_empty()).unwrap_or("").to_string();
    let mut f = last.replace(':', "-").trim().to_string();
    while f.starts_with('.') {
        f.remove(0);
    }
    let mut f = f.trim().to_string();
    while f.len() > 200 {
        f.pop();
    }
    if f.is_empty() { "Download".into() } else { f }
}

/// A free name in `dir`. Files still downloading count as taken: WebKit only
/// creates them as bytes arrive, and two downloads given the same path fail.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let in_flight: Vec<PathBuf> =
        ITEMS.with_borrow(|v| v.iter().filter(|i| i.state == State::Downloading).filter_map(|i| i.path.clone()).collect());
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
        fn decide_destination(
            &self,
            download: &WKDownload,
            response: &NSURLResponse,
            suggested: &NSString,
            completion: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let key = download as *const WKDownload as usize;
            let preview_path = PREVIEWS.with_borrow_mut(|m| {
                let p = m.get_mut(&key)?;
                p.name = pdf_name(&suggested.to_string());
                let path = util::pdf_previews_dir().join(format!("{}.pdf", Uuid::new_v4()));
                p.path = Some(path.clone());
                Some(path)
            });
            if let Some(path) = preview_path {
                let url = NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy()));
                completion.call((Retained::as_ptr(&url) as *mut NSURL,));
                return;
            }
            let pdf_mime = response.MIMEType().is_some_and(|m| m.to_string().eq_ignore_ascii_case("application/pdf"));
            let name = if pdf_mime { pdf_name(&suggested.to_string()) } else { safe_filename(&suggested.to_string()) };
            let path = unique_path(&util::downloads_dir(), &name);
            let url = NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy()));
            // The page's profile, so the PDF opens as a tab next to it.
            let client = unsafe { download.webView() }.and_then(|w| w.downcast_ref::<crate::web::ZWebView>().map(|z| z.ivars().client_id));
            let source_url = unsafe { download.originalRequest() }
                .and_then(|r| r.URL())
                .and_then(|u| util::url_string(&u))
                .filter(|u| u.starts_with("https:"));
            let mut item = Item::new(key, path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), Some(path), State::Downloading, client);
            item.source_url = source_url;
            push(item);
            // WebKit gets its destination immediately; it also tags the file
            // as downloaded from the web, so Gatekeeper checks it on open.
            completion.call((Retained::as_ptr(&url) as *mut NSURL,));
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, download: &WKDownload) {
            let key = download as *const WKDownload as usize;
            if let Some(p) = PREVIEWS.with_borrow_mut(|m| m.remove(&key)) {
                if let Some(path) = p.path {
                    util::later(move || {
                        crate::app::open_pdf_tab(p.client, path, p.name, p.source_url);
                    });
                }
                return;
            }
            update(key, |i| i.state = State::Done);
            util::later(move || {
                show_pdf_where(|i| i.key == key);
            });
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(&self, download: &WKDownload, error: &NSError, _resume: Option<&objc2_foundation::NSData>) {
            let key = download as *const WKDownload as usize;
            util::log(format!("download failed: {}", error.localizedDescription()));
            if let Some(p) = PREVIEWS.with_borrow_mut(|m| m.remove(&key)) {
                if let Some(path) = &p.path {
                    let _ = std::fs::remove_file(path);
                }
                record_failure(&p.name);
                return;
            }
            update(key, |i| i.state = State::Failed);
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

/// Show the PDF at `index` in the downloads list as a tab. False when it
/// is not a finished PDF, so the caller opens it some other way.
pub fn show_pdf(index: usize) -> bool {
    let key = ITEMS.with_borrow(|v| v.get(index).map(|i| i.key));
    key.is_some_and(|k| show_pdf_where(|i| i.key == k))
}

fn show_pdf_where(find: impl Fn(&Item) -> bool) -> bool {
    let found = ITEMS.with_borrow(|v| {
        let i = v.iter().find(|i| find(i)).filter(|i| i.state == State::Done)?;
        let path = i.path.clone().filter(|p| is_pdf(p))?;
        Some((i.key, path, i.name.clone(), i.client, i.source_url.clone(), i.tab))
    });
    let Some((key, path, name, client, source, tab)) = found else { return false };
    if let (Some(c), Some(t)) = (client, tab) {
        if crate::app::show_tab(c, t) {
            return true;
        }
    }
    // Its own profile, or the one in front if that profile is gone.
    let active = crate::app::store(|s| s.active_client_id);
    let opened = client
        .and_then(|c| crate::app::open_downloaded_pdf(c, &path, &name, source.clone()))
        .or_else(|| crate::app::open_downloaded_pdf(active?, &path, &name, source));
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
    let path = unique_path(&util::downloads_dir(), &safe_filename(suggested));
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let key = own_key();
    match std::fs::write(&path, &data) {
        Ok(()) => {
            quarantine(&path);
            push(Item::new(key, name, Some(path), State::Done, client));
            if client.is_some() {
                util::later(move || {
                    show_pdf_where(|i| i.key == key);
                });
            }
        }
        Err(e) => {
            util::log(format!("could not save {name}: {e}"));
            push(Item::new(key, name, None, State::Failed, None));
        }
    }
}

pub fn record_failure(name: &str) {
    push(Item::new(own_key(), name.into(), None, State::Failed, None));
}

fn quarantine(path: &Path) {
    use std::ffi::CString;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let value = format!("0083;{:08x};ZClients;", stamp as u32);
    let (Ok(p), Ok(n)) = (CString::new(path.to_string_lossy().as_bytes()), CString::new("com.apple.quarantine")) else { return };
    unsafe {
        setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0);
    }
}

unsafe extern "C" {
    fn setxattr(path: *const i8, name: *const i8, value: *const std::ffi::c_void, size: usize, position: u32, options: i32) -> i32;
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
