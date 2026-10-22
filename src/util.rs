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
pub fn after(secs: f64, f: impl FnOnce() + 'static) -> Retained<NSTimer> {
    let slot = Cell::new(Some(f));
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        if let Some(f) = slot.take() {
            f();
        }
    });
    // SAFETY: timers are scheduled on, and fire on, the main run loop only.
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) }
}

pub fn later(f: impl FnOnce() + 'static) {
    after(0.0, f);
}

pub fn every(secs: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| f());
    // SAFETY: as above, main run loop only.
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, true, &block) };
    // A little slack lets macOS coalesce wake-ups, which is most of what keeps
    // an idle app at ~0% CPU.
    timer.setTolerance(secs / 4.0);
    timer
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

/// PDFs a page opened in a window, kept so they can be shown as a tab.
/// Each file belongs to one tab and is removed when that tab goes.
pub fn pdf_previews_dir() -> PathBuf {
    let dir = support_dir().join("PDF Previews");
    ensure_dir(&dir, None);
    dir
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

/// Write via a temporary file and rename, so a crash mid-write can never
/// leave a half-written file behind.
pub fn write_atomic(path: &std::path::Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = path.with_extension("tmp~");
    {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
        f.write_all(data)?;
        f.sync_data()?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path)
}

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Called first thing in `main`, so timings count from process start.
pub fn mark_start() {
    START.get_or_init(std::time::Instant::now);
}

pub fn ms_since_start() -> f64 {
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

pub fn log(msg: impl AsRef<str>) {
    eprintln!("ZClients: {}", msg.as_ref());
}
