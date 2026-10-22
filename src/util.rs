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
/// A test build ("ZClients Test", or any run with ZCLIENTS_SUPPORT_DIR set)
/// keeps all of its data apart from the real app's.
pub fn is_test_build() -> bool {
    static TEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TEST.get_or_init(|| {
        std::env::var_os("ZCLIENTS_SUPPORT_DIR").is_some()
            || objc2_foundation::NSBundle::mainBundle().bundleIdentifier().is_some_and(|b| b.to_string().ends_with(".test"))
    })
}

pub fn support_dir() -> PathBuf {
    let dir = match std::env::var_os("ZCLIENTS_SUPPORT_DIR") {
        Some(d) => PathBuf::from(d),
        None if is_test_build() => home().join("Library/Application Support/ZPortSwift Test"),
        None => home().join("Library/Application Support/ZPortSwift"),
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn private_dir(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = support_dir().join(name);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir
}

pub fn downloads_dir() -> PathBuf {
    home().join("Downloads")
}

/// PDFs a page opened in a window, kept so they can be shown as a tab.
/// Each file belongs to one tab and is removed when that tab goes.
pub fn pdf_previews_dir() -> PathBuf {
    let dir = support_dir().join("PDF Previews");
    let _ = std::fs::create_dir_all(&dir);
    dir
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

pub fn log(msg: impl AsRef<str>) {
    eprintln!("ZClients: {}", msg.as_ref());
}
