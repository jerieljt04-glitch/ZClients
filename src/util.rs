//! Small helpers shared by the AppKit/WebKit glue.

use std::cell::Cell;
use std::path::PathBuf;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::NSColor;
use objc2_foundation::{NSString, NSTimer, NSURL, NSUUID};
use uuid::Uuid;

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn ns_uuid(id: Uuid) -> Retained<NSUUID> {
    use objc2::AllocAnyThread;
    let s = ns(&id.hyphenated().to_string().to_uppercase());
    NSUUID::initWithUUIDString(NSUUID::alloc(), &s).expect("a formatted UUID always parses")
}

/// Erase an object's type, for Foundation collections of mixed values.
pub fn any<T: objc2::Message>(obj: Retained<T>) -> Retained<objc2::runtime::AnyObject> {
    // SAFETY: every Objective-C object is an AnyObject.
    unsafe { Retained::cast_unchecked(obj) }
}

pub fn ns_url(s: &str) -> Option<Retained<NSURL>> {
    NSURL::URLWithString(&ns(s))
}

pub fn url_string(u: &NSURL) -> Option<String> {
    u.absoluteString().map(|s| s.to_string())
}

/// Run `f` once on the main run loop after `secs`. Using a timer (rather than
/// running inline) is what breaks re-entrancy: callers never run nested inside
/// an AppKit or WebKit callback that is still holding app state.
///
/// The timer runs in the run loop's *common* modes, so it also fires while
/// an alert, a menu or a live window resize is tracking. A timer in the
/// default mode alone waited for all of those: the UI stopped refreshing
/// and the profile save stalled for as long as a page's `alert()` was up.
/// Callers never hold a `RefCell` borrow across a modal call, so firing
/// there is safe.
pub fn after(secs: f64, f: impl FnOnce() + 'static) -> Retained<NSTimer> {
    let slot = Cell::new(Some(f));
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        if let Some(f) = slot.take() {
            f();
        }
    });
    // SAFETY: the timer is added to, and fires on, the main run loop only.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(secs, false, &block) };
    schedule(&timer);
    timer
}

pub fn later(f: impl FnOnce() + 'static) {
    after(0.0, f);
}

/// Run `f` on the main thread, from any thread (the main dispatch queue,
/// which the main run loop serves in its common modes).
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    unsafe extern "C" {
        static _dispatch_main_q: std::ffi::c_void;
        fn dispatch_async_f(queue: *const std::ffi::c_void, context: *mut std::ffi::c_void, work: extern "C" fn(*mut std::ffi::c_void));
    }
    type Job = Box<dyn FnOnce() + Send>;
    extern "C" fn run(context: *mut std::ffi::c_void) {
        // SAFETY: `context` is the Box<Job> leaked below, run exactly once.
        let job = unsafe { Box::from_raw(context.cast::<Job>()) };
        job();
    }
    let job: Box<Job> = Box::new(Box::new(f));
    // SAFETY: the main queue is a process-lifetime static; `run` frees `job`.
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(job).cast(), run) };
}

pub fn every(secs: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| f());
    // SAFETY: as above, main run loop only.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(secs, true, &block) };
    // A little slack lets macOS coalesce wake-ups, which is most of what keeps
    // an idle app at ~0% CPU.
    timer.setTolerance(secs / 4.0);
    schedule(&timer);
    timer
}

fn schedule(timer: &NSTimer) {
    // SAFETY: NSRunLoopCommonModes is a constant string owned by Foundation.
    unsafe { objc2_foundation::NSRunLoop::mainRunLoop().addTimer_forMode(timer, objc2_foundation::NSRunLoopCommonModes) };
}

pub fn color(hex: &str) -> Retained<NSColor> {
    let (r, g, b) = crate::model::hex_to_rgb(hex);
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

pub fn color_hex(c: &NSColor) -> Option<String> {
    let srgb = c.colorUsingColorSpace(&objc2_app_kit::NSColorSpace::sRGBColorSpace())?;
    let ch = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(format!("#{:02x}{:02x}{:02x}", ch(srgb.redComponent()), ch(srgb.greenComponent()), ch(srgb.blueComponent())))
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/Library/Application Support/ZPortSwift` — the Swift build's folder, so
/// profiles carry over untouched.
/// A test build ("ZClients Test", or `cargo test` with ZCLIENTS_SUPPORT_DIR
/// set) keeps all of its data apart from the real app's. An environment
/// variable alone never makes the real app a test build: that would move its
/// vault key out of the Keychain into a plain file and enable self-tests.
pub fn is_test_build() -> bool {
    static TEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TEST.get_or_init(|| {
        (cfg!(test) && std::env::var_os("ZCLIENTS_SUPPORT_DIR").is_some())
            || objc2_foundation::NSBundle::mainBundle().bundleIdentifier().is_some_and(|b| b.to_string().ends_with(".test"))
    })
}

pub fn support_dir() -> PathBuf {
    let dir = match std::env::var_os("ZCLIENTS_SUPPORT_DIR") {
        Some(d) => PathBuf::from(d),
        None if is_test_build() => home().join("Library/Application Support/ZPortSwift Test"),
        None => home().join("Library/Application Support/ZPortSwift"),
    };
    ensure_dir(&dir, None);
    dir
}

/// Create a folder (and set its mode) once per run, not on every file
/// access: these paths are hit on every tab switch and save.
fn ensure_dir(dir: &std::path::Path, mode: Option<u32>) {
    use std::os::unix::fs::PermissionsExt;
    static MADE: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if made.iter().any(|d| d == dir) && dir.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    if let Some(m) = mode {
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(m));
    }
    made.push(dir.to_path_buf());
}

pub fn private_dir(name: &str) -> PathBuf {
    let dir = support_dir().join(name);
    ensure_dir(&dir, Some(0o700));
    dir
}

pub fn downloads_dir() -> PathBuf {
    home().join("Downloads")
}

/// PDFs shown as tabs. Each preview has its own random 0700 folder inside
/// this one (see `pdf.rs`); the folder itself is private too.
pub fn pdf_previews_dir() -> PathBuf {
    private_dir("PDF Previews")
}

enum Write {
    File { path: PathBuf, data: Vec<u8>, mode: u32 },
    Remove(PathBuf),
    Flush(std::sync::mpsc::Sender<()>),
}

/// One background thread does the app's routine disk writes (tab places,
/// session backups) in the order they were asked for, so a write and a
/// later delete of the same file can never swap. Each write syncs to disk,
/// which used to stall the window on every tab switch.
fn writer() -> &'static std::sync::Mutex<std::sync::mpsc::Sender<Write>> {
    static W: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Write>>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Write>();
        std::thread::Builder::new()
            .name("zclients-writer".into())
            .spawn(move || {
                for w in rx {
                    match w {
                        Write::File { path, data, mode } => {
                            if let Err(e) = write_atomic(&path, &data, mode) {
                                log(format!("could not write {}: {e}", path.display()));
                            }
                        }
                        Write::Remove(path) => {
                            let _ = std::fs::remove_file(path);
                        }
                        Write::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("writer thread");
        std::sync::Mutex::new(tx)
    })
}

pub fn write_soon(path: PathBuf, data: Vec<u8>, mode: u32) {
    let _ = writer().lock().unwrap_or_else(|e| e.into_inner()).send(Write::File { path, data, mode });
}

pub fn remove_soon(path: PathBuf) {
    let _ = writer().lock().unwrap_or_else(|e| e.into_inner()).send(Write::Remove(path));
}

/// Wait (bounded) until every queued write is on disk. Used on quit.
pub fn flush_writes() {
    let (tx, rx) = std::sync::mpsc::channel();
    if writer().lock().unwrap_or_else(|e| e.into_inner()).send(Write::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
    }
}

/// macOS `O_NOFOLLOW`: never open through a symbolic link.
pub const O_NOFOLLOW: i32 = 0x0100;

#[cfg(test)]
thread_local! {
    /// Test-only failure injection: the next write fails after the
    /// temporary file was partly written (a full disk, a crash).
    pub static FAIL_NEXT_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Write `data` to `path` so that a crash, a full disk or a planted link can
/// never leave a half-written or redirected file:
///
/// * the temporary file has an unpredictable name and is created with
///   `O_CREAT|O_EXCL|O_NOFOLLOW`, so a symlink or file planted there by
///   another process makes the write fail instead of following it;
/// * it is flushed to disk before it replaces `path` with `rename(2)`,
///   which swaps the directory entry (a symlink at `path` is replaced, not
///   followed);
/// * the directory is synced afterwards so the rename itself survives a
///   power cut.
///
/// On any error the original file is untouched and the temporary removed.
pub fn write_atomic(path: &std::path::Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let tmp = dir.join(format!(".{}.{}.tmp", name.to_string_lossy(), Uuid::new_v4().simple()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(O_NOFOLLOW).open(&tmp)?;
        #[cfg(test)]
        if FAIL_NEXT_WRITE.replace(false) {
            f.write_all(&data[..data.len() / 2])?;
            return Err(std::io::Error::other("injected write failure"));
        }
        f.write_all(data)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        // Make the rename durable. Failure here is not fatal: the data is
        // already in place.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Called first thing in `main`, so timings count from process start.
pub fn mark_start() {
    START.get_or_init(std::time::Instant::now);
}

pub fn ms_since_start() -> f64 {
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// How many recent log lines Export Diagnostics can include.
const LOG_LINES: usize = 300;

static RECENT: std::sync::Mutex<std::collections::VecDeque<String>> = std::sync::Mutex::new(std::collections::VecDeque::new());

/// Log a line. A Finder-launched app's stderr goes nowhere, so each line
/// is also kept (redacted, timestamped, last 300 only) for Export
/// Diagnostics and sent to the unified log (`log show --predicate
/// 'subsystem == "com.zport.swift"'`). Only the redacted text leaves the
/// process; stderr keeps the full line for self-test runs in a terminal.
pub fn log(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("ZClients: {msg}");
    let redacted = crate::privacy::redact(msg);
    oslog::write(&redacted);
    let line = format!("[{:>9.3}s] {redacted}", ms_since_start() / 1000.0);
    let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    if recent.len() == LOG_LINES {
        recent.pop_front();
    }
    recent.push_back(line);
}

/// The last log lines, oldest first, already redacted.
pub fn recent_logs() -> Vec<String> {
    RECENT.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

/// The unified log, through the C entry point the `os_log` macro expands to
/// (there is no function form). The format lives in `__oslogstring`, as the
/// compiler puts it, so `log show` can render it.
mod oslog {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    struct OsLog {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        static __dso_handle: c_void;
        fn os_log_create(subsystem: *const c_char, category: *const c_char) -> *mut OsLog;
        fn _os_log_impl(dso: *const c_void, log: *mut OsLog, kind: u8, format: *const c_char, buf: *mut u8, size: u32);
    }

    #[unsafe(link_section = "__TEXT,__oslogstring,cstring_literals")]
    static FORMAT: [u8; 11] = *b"%{public}s\0";

    struct Handle(*mut OsLog);
    // SAFETY: os_log_t objects are immutable and documented as thread-safe.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    fn handle() -> *mut OsLog {
        static LOG: std::sync::OnceLock<Handle> = std::sync::OnceLock::new();
        LOG.get_or_init(|| Handle(unsafe { os_log_create(c"com.zport.swift".as_ptr(), c"app".as_ptr()) })).0
    }

    pub fn write(text: &str) {
        if cfg!(test) {
            return;
        }
        let log = handle();
        let Ok(c) = std::ffi::CString::new(text.replace('\0', " ")) else { return };
        // One public string argument: summary (has non-scalar), count 1,
        // then descriptor (string, public), size 8, the pointer.
        let mut buf = [0u8; 12];
        buf[0] = 0x02;
        buf[1] = 1;
        buf[2] = 0x22;
        buf[3] = 8;
        buf[4..12].copy_from_slice(&(c.as_ptr() as usize as u64).to_ne_bytes());
        // OS_LOG_TYPE_DEFAULT
        unsafe { _os_log_impl(&raw const __dso_handle, log, 0, FORMAT.as_ptr().cast(), buf.as_mut_ptr(), buf.len() as u32) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("zc-util-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn log_lines_are_kept_redacted_and_bounded() {
        log("signed in as jane@acme.com at https://crm.zoho.com/crm/org1/tab/Leads?x=1 token abcdefghij0123456789XYZ");
        // Other tests log from their own threads: find this line by its text.
        let last = recent_logs().into_iter().rev().find(|l| l.contains("signed in as")).unwrap();
        assert!(!last.contains("jane@acme.com"), "{last}");
        assert!(!last.contains("org1"), "{last}");
        assert!(!last.contains("abcdefghij0123456789XYZ"), "{last}");
        assert!(last.contains("[email]") && last.contains("https://crm.zoho.com/[…]") && last.contains("[token]"), "{last}");
        for i in 0..LOG_LINES + 20 {
            log(format!("line {i}"));
        }
        let all = recent_logs();
        assert_eq!(all.len(), LOG_LINES);
        assert!(all.iter().any(|l| l.ends_with(&format!("line {}", LOG_LINES + 19))));
    }

    #[test]
    fn atomic_write_replaces_and_sets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch();
        let p = d.join("clients.json");
        write_atomic(&p, b"one", 0o600).unwrap();
        write_atomic(&p, b"two", 0o600).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temporary files left");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn failed_write_keeps_the_original_and_cleans_up() {
        let d = scratch();
        let p = d.join("clients.json");
        write_atomic(&p, b"good", 0o600).unwrap();
        FAIL_NEXT_WRITE.set(true);
        assert!(write_atomic(&p, b"partial-and-lost", 0o600).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"good", "partial write never replaces the file");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn read_only_directory_fails_without_damage() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch();
        let p = d.join("clients.json");
        write_atomic(&p, b"good", 0o600).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o500)).unwrap();
        let r = write_atomic(&p, b"new", 0o600);
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(r.is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"good");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_symlink_at_the_destination_is_replaced_not_followed() {
        let d = scratch();
        let victim = d.join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let p = d.join("clients.json");
        std::os::unix::fs::symlink(&victim, &p).unwrap();
        write_atomic(&p, b"data", 0o600).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious", "link target untouched");
        assert!(!std::fs::symlink_metadata(&p).unwrap().file_type().is_symlink());
        let _ = std::fs::remove_dir_all(d);
    }
}
