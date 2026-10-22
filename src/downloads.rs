//! Downloads go to ~/Downloads and are listed under the toolbar's download
//! button.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURLResponse, NSURL};
use objc2_web_kit::{WKDownload, WKDownloadDelegate};

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
}

thread_local! {
    static ITEMS: RefCell<Vec<Item>> = const { RefCell::new(Vec::new()) };
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

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
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
        if !c.exists() {
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
            _response: &NSURLResponse,
            suggested: &NSString,
            completion: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let dir = util::downloads_dir();
            let path = unique_path(&dir, &safe_filename(&suggested.to_string()));
            let url = NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy()));
            push(Item {
                key: download as *const WKDownload as usize,
                name: path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                path: Some(path),
                state: State::Downloading,
            });
            // WebKit gets its destination immediately; it also tags the file
            // as downloaded from the web, so Gatekeeper checks it on open.
            completion.call((Retained::as_ptr(&url) as *mut NSURL,));
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, download: &WKDownload) {
            update(download as *const WKDownload as usize, |i| i.state = State::Done);
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(&self, download: &WKDownload, _error: &NSError, _resume: Option<&objc2_foundation::NSData>) {
            update(download as *const WKDownload as usize, |i| i.state = State::Failed);
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

/// Bytes the app read itself (blob:/data: right-click downloads). WebKit's
/// own downloads are quarantined automatically; these get the same marker by
/// hand so they are not quietly more trusted than a normal download.
pub fn save_bytes(data: Vec<u8>, suggested: &str) {
    let path = unique_path(&util::downloads_dir(), &safe_filename(suggested));
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match std::fs::write(&path, &data) {
        Ok(()) => {
            quarantine(&path);
            push(Item { key: 0, name, path: Some(path), state: State::Done });
        }
        Err(e) => {
            util::log(format!("could not save {name}: {e}"));
            push(Item { key: 0, name, path: None, state: State::Failed });
        }
    }
}

pub fn record_failure(name: &str) {
    push(Item { key: 0, name: name.into(), path: None, state: State::Failed });
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
}
