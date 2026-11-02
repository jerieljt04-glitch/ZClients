//! Settings › Saved Logins: every client's saved Zoho email and password in
//! one window, to look one up or to change it when a client's password
//! changes. macOS asks for Touch ID or the Mac's password before the window
//! opens (as Safari's Passwords does); passwords stay hidden until "Show" is
//! pressed for that row, and the window keeps nothing once it is closed.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSButton, NSColor, NSPasteboard, NSScrollView, NSSecureTextField, NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSError, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSTimer};
use uuid::Uuid;

use crate::{app, keychain, theme, util, web};

const W: f64 = 760.0;
const ROW_H: f64 = 72.0;
const TOP: f64 = 72.0;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

struct Row {
    client: Uuid,
    email: Retained<NSTextField>,
    secure: Retained<NSSecureTextField>,
    plain: Retained<NSTextField>,
    show: Retained<NSButton>,
    status: Retained<NSTextField>,
    saved: bool,
}

impl Row {
    fn shown(&self) -> bool {
        !self.plain.isHidden()
    }

    fn password(&self) -> String {
        if self.shown() { self.plain.stringValue().to_string() } else { self.secure.stringValue().to_string() }
    }

    fn set_status(&self, text: &str, error: bool) {
        self.status.setStringValue(&util::ns(text));
        let color = if error { theme::destructive() } else { NSColor::secondaryLabelColor() };
        self.status.setTextColor(Some(&color));
    }
}

thread_local! {
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static ROWS: RefCell<Vec<Row>> = const { RefCell::new(Vec::new()) };
    static ACTIONS: RefCell<Option<Retained<LoginsActions>>> = const { RefCell::new(None) };
    /// The authentication in progress: its LAContext and the timer waiting
    /// for macOS's answer (which arrives on another thread).
    static PENDING: RefCell<Option<(Retained<AnyObject>, Retained<NSTimer>)>> = const { RefCell::new(None) };
}

define_class!(
    /// A flipped page so the list reads top to bottom.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct LoginsPage;

    impl LoginsPage {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct LoginsActions;

    unsafe impl NSObjectProtocol for LoginsActions {}

    impl LoginsActions {
        #[unsafe(method(toggleShow:))]
        fn toggle_show(&self, s: Option<&NSButton>) {
            let Some(i) = s.map(|b| b.tag() as usize) else { return };
            ROWS.with_borrow(|rows| {
                let Some(r) = rows.get(i) else { return };
                if r.shown() {
                    r.secure.setStringValue(&r.plain.stringValue());
                    r.plain.setStringValue(&util::ns(""));
                    r.plain.setHidden(true);
                    r.secure.setHidden(false);
                    r.show.setTitle(&util::ns("Show"));
                } else {
                    r.plain.setStringValue(&r.secure.stringValue());
                    r.secure.setHidden(true);
                    r.plain.setHidden(false);
                    r.show.setTitle(&util::ns("Hide"));
                }
            });
        }

        #[unsafe(method(copyPassword:))]
        fn copy_password(&self, s: Option<&NSButton>) {
            let Some(i) = s.map(|b| b.tag() as usize) else { return };
            ROWS.with_borrow(|rows| {
                let Some(r) = rows.get(i) else { return };
                let password = r.password();
                if password.is_empty() {
                    r.set_status("No password to copy", true);
                    return;
                }
                copy_secret(&password);
                r.set_status("Password copied", false);
            });
        }

        #[unsafe(method(saveLogin:))]
        fn save_login(&self, s: Option<&NSButton>) {
            if let Some(i) = s.map(|b| b.tag() as usize) {
                save_row(i);
            }
        }
    }

    unsafe impl NSWindowDelegate for LoginsActions {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            // Nothing secret outlives the window.
            let rows = ROWS.with_borrow_mut(std::mem::take);
            for r in &rows {
                r.secure.setStringValue(&util::ns(""));
                r.plain.setStringValue(&util::ns(""));
            }
            if let Some(w) = WINDOW.with_borrow_mut(Option::take) {
                util::later(move || w.setContentView(None));
            }
        }
    }
);

fn actions() -> Retained<LoginsActions> {
    ACTIONS.with_borrow_mut(|a| {
        a.get_or_insert_with(|| {
            let this = LoginsActions::alloc(mtm()).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

fn target() -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(actions()))
}

/// Settings' "Show Saved Logins…": ask macOS who is at the Mac, then open
/// the window.
pub fn show() {
    if let Some(w) = WINDOW.with_borrow(Clone::clone) {
        w.makeKeyAndOrderFront(None);
        return;
    }
    if PENDING.with_borrow(Option::is_some) {
        return;
    }
    // The automated self-tests run without anyone at the Mac.
    if util::is_test_build() && std::env::var_os("ZCLIENTS_SELFTEST").is_some() {
        open_window();
        return;
    }
    authenticate("show the Zoho logins saved for your clients", |ok| {
        if ok {
            open_window();
        }
    });
}

#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

/// LAPolicyDeviceOwnerAuthentication: Touch ID, an Apple Watch, or the
/// Mac's login password.
const LA_POLICY_DEVICE_OWNER: isize = 2;

const WAITING: u8 = 0;
const PASSED: u8 = 1;
const FAILED: u8 = 2;

/// Touch ID or the Mac's password, through LocalAuthentication. `done` runs
/// on the main thread. A Mac with no login password at all cannot be
/// asked; then there is nothing to check against, and the answer is yes.
fn authenticate(reason: &str, done: impl FnOnce(bool) + 'static) {
    let Some(class) = AnyClass::get(c"LAContext") else {
        util::log("LocalAuthentication is not available");
        return done(false);
    };
    let context: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let can: Bool = unsafe { msg_send![&*context, canEvaluatePolicy: LA_POLICY_DEVICE_OWNER, error: std::ptr::null_mut::<*mut NSError>()] };
    if !can.as_bool() {
        util::log("saved logins: no device-owner authentication on this Mac; opening without it");
        return done(true);
    }
    let result = Arc::new(AtomicU8::new(WAITING));
    let answer = result.clone();
    // Called by LocalAuthentication on its own queue: it only stores the answer.
    let reply = RcBlock::new(move |ok: Bool, _error: *mut NSError| {
        answer.store(if ok.as_bool() { PASSED } else { FAILED }, Ordering::SeqCst);
    });
    let reason = util::ns(reason);
    unsafe {
        let _: () = msg_send![&*context, evaluatePolicy: LA_POLICY_DEVICE_OWNER, localizedReason: &*reason, reply: &*reply];
    }
    let done = RefCell::new(Some(done));
    let timer = util::every(0.1, move || {
        let state = result.load(Ordering::SeqCst);
        if state == WAITING {
            return;
        }
        if let Some((_, timer)) = PENDING.with_borrow_mut(Option::take) {
            timer.invalidate();
        }
        if let Some(done) = done.borrow_mut().take() {
            done(state == PASSED);
        }
    });
    PENDING.with_borrow_mut(|p| *p = Some((context, timer)));
}

/// Real clients in sidebar order, with their saved login if any.
fn clients() -> Vec<(Uuid, String)> {
    app::store(|s| s.ordered().iter().filter(|c| !c.is_search()).map(|c| (c.id, c.name.clone())).collect())
}

fn open_window() {
    let mtm = mtm();
    let clients = clients();
    let page_h = TOP + ROW_H * clients.len().max(1) as f64 + theme::SECTION_GAP;
    let win_h = page_h.clamp(260.0, 640.0);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, W, win_h),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&util::ns("Saved Logins"));
    window.setMinSize(NSSize::new(W, 260.0));
    window.setDelegate(Some(ProtocolObject::from_ref(&*actions())));

    let page: Retained<LoginsPage> = {
        let this = LoginsPage::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, W, page_h)] }
    };
    let m = theme::SHEET_MARGIN;
    let intro = NSTextField::wrappingLabelWithString(
        &util::ns(
            "The Zoho email and password ZClients fills in on each client's Zoho sign-in page. When a client's password changes, type the new one and press Save. Clearing both fields and saving removes the login. Logins are stored encrypted in ZClients's vault on this Mac.",
        ),
        mtm,
    );
    intro.setFont(Some(&theme::font(theme::Text::Footnote)));
    intro.setTextColor(Some(&NSColor::secondaryLabelColor()));
    intro.setFrame(rect(m, 14.0, W - m * 2.0, 48.0));
    page.addSubview(&intro);

    let (email_x, email_w) = (m, 250.0);
    let (pw_x, pw_w) = (email_x + email_w + 10.0, 200.0);
    let show_x = pw_x + pw_w + 8.0;
    let copy_x = show_x + 70.0;
    let save_x = copy_x + 70.0;

    let mut rows = Vec::new();
    if clients.is_empty() {
        let none = crate::ui::text("No clients yet.", theme::Text::Body, theme::Weight::Regular);
        none.setFrame(rect(m, TOP, W - m * 2.0, 20.0));
        page.addSubview(&none);
    }
    for (i, (id, name)) in clients.iter().enumerate() {
        let y = TOP + ROW_H * i as f64;
        let login = keychain::read_login(*id);

        let title = crate::ui::text(name, theme::Text::Headline, theme::Weight::Semibold);
        title.setFrame(rect(m, y, email_w + pw_w, 20.0));
        page.addSubview(&title);

        let status = crate::ui::text(if login.is_some() { "" } else { "No saved login" }, theme::Text::Footnote, theme::Weight::Regular);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setFrame(rect(show_x, y + 2.0, W - show_x - m, 16.0));
        status.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        page.addSubview(&status);

        let fy = y + 26.0;
        let email = NSTextField::textFieldWithString(&util::ns(login.as_ref().map(|l| l.email.as_str()).unwrap_or("")), mtm);
        email.setPlaceholderString(Some(&util::ns("you@company.com")));
        email.setFrame(rect(email_x, fy, email_w, theme::FIELD_H));
        email.setAccessibilityLabel(Some(&util::ns(&format!("{name} email"))));
        page.addSubview(&email);

        let secure = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(pw_x, fy, pw_w, theme::FIELD_H));
        secure.setStringValue(&util::ns(login.as_ref().map(|l| l.password.as_str()).unwrap_or("")));
        secure.setPlaceholderString(Some(&util::ns("Password")));
        secure.setAccessibilityLabel(Some(&util::ns(&format!("{name} password"))));
        page.addSubview(&secure);
        let plain = NSTextField::textFieldWithString(&util::ns(""), mtm);
        plain.setFrame(rect(pw_x, fy, pw_w, theme::FIELD_H));
        plain.setPlaceholderString(Some(&util::ns("Password")));
        plain.setAccessibilityLabel(Some(&util::ns(&format!("{name} password, shown"))));
        plain.setHidden(true);
        page.addSubview(&plain);

        let button = |title: &str, action, x: f64, w: f64| {
            let b = unsafe { NSButton::buttonWithTitle_target_action(&util::ns(title), Some(&target()), Some(action), mtm) };
            b.setTag(i as isize);
            b.setFrame(rect(x, fy - 2.0, w, theme::BUTTON_H));
            b.setAccessibilityLabel(Some(&util::ns(&format!("{title} — {name}"))));
            page.addSubview(&b);
            b
        };
        let show = button("Show", sel!(toggleShow:), show_x, 66.0);
        button("Copy", sel!(copyPassword:), copy_x, 66.0);
        button("Save", sel!(saveLogin:), save_x, W - save_x - m);

        rows.push(Row { client: *id, email, secure, plain, show, status, saved: login.is_some() });
    }
    ROWS.with_borrow_mut(|r| *r = rows);

    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, W, win_h));
    scroll.setHasVerticalScroller(true);
    scroll.setDocumentView(Some(&page));
    scroll.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable);
    window.setContentView(Some(&scroll));
    window.setAutorecalculatesKeyViewLoop(true);
    window.center();
    window.makeKeyAndOrderFront(None);
    // Open with nothing selected, so no address is highlighted on screen.
    window.makeFirstResponder(None);
    WINDOW.with_borrow_mut(|w| *w = Some(window));
}

/// What pressing Save on a row does with what was typed.
#[derive(Debug, PartialEq, Eq)]
enum SaveAction {
    Write,
    Remove,
    NothingSaved,
    NeedPassword,
    NeedEmail,
    BadEmail,
}

fn save_action(email: &str, password: &str, saved: bool) -> SaveAction {
    let email = email.trim();
    match (email.is_empty(), password.is_empty()) {
        (true, true) if saved => SaveAction::Remove,
        (true, true) => SaveAction::NothingSaved,
        (false, true) => SaveAction::NeedPassword,
        (true, false) => SaveAction::NeedEmail,
        (false, false) if !email.contains('@') || email.contains(char::is_whitespace) => SaveAction::BadEmail,
        (false, false) => SaveAction::Write,
    }
}

fn save_row(i: usize) {
    let Some((client, email, password, saved)) = ROWS.with_borrow(|rows| rows.get(i).map(|r| (r.client, r.email.stringValue().to_string(), r.password(), r.saved))) else { return };
    let status = |text: &str, error: bool| ROWS.with_borrow(|rows| rows.get(i).map(|r| r.set_status(text, error)));
    match save_action(&email, &password, saved) {
        SaveAction::Write => {
            let login = keychain::Login { email: email.trim().to_string(), password };
            if keychain::write_login(client, &login) {
                ROWS.with_borrow_mut(|rows| rows.get_mut(i).map(|r| r.saved = true));
                status("Saved", false);
            } else {
                status("Not saved: the vault key in your Keychain is not available", true);
            }
        }
        SaveAction::Remove => {
            if !web::confirm("Remove this saved login?", "ZClients will no longer fill in this client's Zoho sign-in. You stay signed in.", "Remove") {
                return;
            }
            if keychain::delete_login(client) {
                ROWS.with_borrow_mut(|rows| rows.get_mut(i).map(|r| r.saved = false));
                status("Removed", false);
            } else {
                status("Could not be removed; try again", true);
            }
        }
        SaveAction::NothingSaved => {
            status("Type an email and a password first", true);
        }
        SaveAction::NeedPassword => {
            status("Type the password too", true);
        }
        SaveAction::NeedEmail => {
            status("Type the email too", true);
        }
        SaveAction::BadEmail => {
            status("That email address doesn't look right", true);
        }
    }
}

/// Put a password on the clipboard, marked so clipboard managers and
/// Universal Clipboard history skip it (the nspasteboard.org convention).
fn copy_secret(password: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    unsafe {
        pb.setString_forType(&util::ns(password), objc2_app_kit::NSPasteboardTypeString);
    }
    pb.setString_forType(&util::ns(""), &util::ns("org.nspasteboard.ConcealedType"));
}

/// Self-test: what the window shows (test builds only).
pub fn describe_for_test() -> String {
    if !util::is_test_build() {
        return String::new();
    }
    let open = WINDOW.with_borrow(Option::is_some);
    let rows: Vec<String> = ROWS.with_borrow(|rows| {
        rows.iter()
            .map(|r| {
                format!(
                    "{} [{}] password {} chars, status {:?}",
                    r.email.stringValue(),
                    if r.shown() { "shown" } else { "hidden" },
                    r.password().chars().count(),
                    r.status.stringValue().to_string()
                )
            })
            .collect()
    });
    format!("open={open} rows={rows:?}")
}

/// Self-test: save a picture of the window's contents (test builds only).
pub fn snapshot_for_test(path: &str) {
    if !util::is_test_build() {
        return;
    }
    let Some(view) = WINDOW.with_borrow(|w| w.as_ref().and_then(|w| w.contentView())) else { return };
    let bounds = view.bounds();
    let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) else { return };
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let props = objc2_foundation::NSDictionary::new();
    if let Some(png) = unsafe { rep.representationUsingType_properties(objc2_app_kit::NSBitmapImageFileType::PNG, &props) } {
        let _ = std::fs::write(path, png.to_vec());
    }
}

/// Self-test: type into a row and press Save (test builds only).
pub fn edit_for_test(i: usize, email: &str, password: &str) {
    if !util::is_test_build() {
        return;
    }
    ROWS.with_borrow(|rows| {
        if let Some(r) = rows.get(i) {
            r.email.setStringValue(&util::ns(email));
            r.secure.setStringValue(&util::ns(password));
        }
    });
    save_row(i);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_writes_only_a_complete_login_and_removes_on_empty() {
        assert_eq!(save_action("a@b.com", "pw", false), SaveAction::Write);
        assert_eq!(save_action("  a@b.com ", "pw", true), SaveAction::Write);
        assert_eq!(save_action("", "", true), SaveAction::Remove);
        assert_eq!(save_action("  ", "", false), SaveAction::NothingSaved);
        assert_eq!(save_action("a@b.com", "", true), SaveAction::NeedPassword);
        assert_eq!(save_action("", "pw", false), SaveAction::NeedEmail);
        assert_eq!(save_action("not an email", "pw", false), SaveAction::BadEmail);
        assert_eq!(save_action("ab.com", "pw", false), SaveAction::BadEmail);
    }
}
