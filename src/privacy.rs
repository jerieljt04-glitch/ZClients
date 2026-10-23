//! Settings & Privacy (⌘,): per-profile sign-in retention, saved login,
//! clearing website data, camera/microphone use, download permissions,
//! keyboard choices, unfinished deletions, version — and a diagnostics
//! export that is redacted before it is written.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSPopUpButton, NSScrollView, NSTextField, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize};
use uuid::Uuid;

use crate::model::Workspace;
use crate::{app, cleanup, keychain, session, settings, util, web};

const W: f64 = 620.0;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

define_class!(
    /// A flipped page so the settings read top to bottom.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Page;

    impl Page {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct PrivacyActions;

    impl PrivacyActions {
        #[unsafe(method(pickProfile:))]
        fn pick_profile(&self, s: Option<&NSPopUpButton>) {
            if let Some(p) = s {
                let profiles = profiles();
                SELECTED.set(profiles.get(p.indexOfSelectedItem().max(0) as usize).map(|p| p.0));
                util::later(rebuild);
            }
        }

        #[unsafe(method(keepChanged:))]
        fn keep_changed(&self, _s: Option<&AnyObject>) {
            apply_retention();
        }

        #[unsafe(method(removeLogin:))]
        fn remove_login(&self, _s: Option<&AnyObject>) {
            let Some(id) = SELECTED.get() else { return };
            if web::confirm("Remove the saved login?", "ZClients will no longer fill in this profile's Zoho sign-in. You stay signed in.", "Remove") {
                if !keychain::delete_login(id) {
                    web::run_alert("The saved login could not be removed", "Try again; if it keeps failing, export diagnostics.", &["OK"], None);
                }
                rebuild();
            }
        }

        #[unsafe(method(clearCookies:))]
        fn clear_cookies(&self, _s: Option<&AnyObject>) {
            clear_profile_data(true);
        }

        #[unsafe(method(clearAllData:))]
        fn clear_all(&self, _s: Option<&AnyObject>) {
            clear_profile_data(false);
        }

        #[unsafe(method(retryCleanup:))]
        fn retry_cleanup(&self, s: Option<&NSButton>) {
            let jobs = cleanup::pending_jobs();
            if let Some(job) = s.and_then(|b| jobs.get(b.tag() as usize)) {
                cleanup::run(job.client);
                util::after(2.0, rebuild);
            }
        }

        #[unsafe(method(stopMedia:))]
        fn stop_media(&self, s: Option<&NSButton>) {
            let calls = web::tabs_in_calls();
            if let Some((_, tab, _, _)) = s.and_then(|b| calls.get(b.tag() as usize)) {
                web::stop_media(*tab);
                util::after(0.5, rebuild);
            }
        }

        #[unsafe(method(resetDownloadSite:))]
        fn reset_site(&self, s: Option<&NSButton>) {
            let sites: Vec<String> = settings::get(|st| st.downloads.keys().cloned().collect());
            if let Some(site) = s.and_then(|b| sites.get(b.tag() as usize)) {
                settings::set_download_permission(site, settings::SitePermission::Ask);
                util::later(rebuild);
            }
        }

        #[unsafe(method(clearPreviews:))]
        fn clear_previews(&self, _s: Option<&AnyObject>) {
            let n = app::clear_previews_and_download_history();
            web::run_alert(
                "PDF previews and download history cleared",
                &format!("{n} unused preview(s) removed. PDFs still open in a tab stay until the tab is closed. Files in your Downloads folder were not touched."),
                &["OK"],
                None,
            );
        }

        #[unsafe(method(toggleSetting:))]
        fn toggle_setting(&self, s: Option<&NSButton>) {
            let Some(b) = s else { return };
            let on = b.state() == NSControlStateValueOn;
            match b.tag() {
                1 => settings::update(|st| st.control_a_selects_all = on),
                2 => settings::update(|st| st.page_owns_editor_shortcuts = on),
                3 if util::is_test_build() => settings::update(|st| st.developer_mode = on),
                _ => {}
            }
        }

        #[unsafe(method(exportDiagnostics:))]
        fn export(&self, _s: Option<&AnyObject>) {
            export_diagnostics();
        }
    }
);

thread_local! {
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static ACTIONS: RefCell<Option<Retained<PrivacyActions>>> = const { RefCell::new(None) };
    static SELECTED: Cell<Option<Uuid>> = const { Cell::new(None) };
    static CONTROLS: RefCell<Option<(Retained<NSButton>, Retained<NSPopUpButton>)>> = const { RefCell::new(None) };
}

fn actions() -> Retained<AnyObject> {
    ACTIONS.with_borrow_mut(|a| {
        let a = a.get_or_insert_with(|| {
            let this = PrivacyActions::alloc(mtm()).set_ivars(());
            unsafe { msg_send![super(this), init] }
        });
        Retained::into_super(Retained::into_super(a.clone()))
    })
}

fn profiles() -> Vec<(Uuid, String)> {
    app::store(|s| s.ordered().iter().filter(|c| !c.is_search()).map(|c| (c.id, c.name.clone())).collect())
}

pub fn show() {
    if SELECTED.get().is_none_or(|id| app::store(|s| s.client(id).is_none())) {
        SELECTED.set(app::store(|s| s.active_client().filter(|c| !c.is_search()).map(|c| c.id)).or_else(|| profiles().first().map(|p| p.0)));
    }
    let window = WINDOW.with_borrow_mut(|w| {
        w.get_or_insert_with(|| {
            let win = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm()),
                    rect(0.0, 0.0, W, 720.0),
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { win.setReleasedWhenClosed(false) };
            win.setTitle(&util::ns("ZClients Settings"));
            win.setMinSize(NSSize::new(W, 400.0));
            win.center();
            win
        })
        .clone()
    });
    rebuild();
    window.makeKeyAndOrderFront(None);
}

struct Builder {
    page: Retained<Page>,
    y: f64,
}

impl Builder {
    fn heading(&mut self, text: &str) {
        self.y += 14.0;
        let l = crate::ui::label(text, 15.0, true);
        l.setFrame(rect(20.0, self.y, W - 40.0, 22.0));
        l.setAccessibilityRole(Some(&util::ns("AXHeading")));
        self.page.addSubview(&l);
        self.y += 28.0;
    }

    fn text(&mut self, text: &str, secondary: bool) {
        let l = NSTextField::wrappingLabelWithString(&util::ns(text), mtm());
        l.setFont(Some(&NSFont::systemFontOfSize(if secondary { 11.5 } else { 13.0 })));
        if secondary {
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
        let lines = (text.len() as f64 / 92.0).ceil().max(1.0) + text.matches('\n').count() as f64;
        let h = lines * if secondary { 15.0 } else { 17.0 } + 2.0;
        l.setFrame(rect(24.0, self.y, W - 48.0, h));
        self.page.addSubview(&l);
        self.y += h + 6.0;
    }

    fn button(&mut self, title: &str, action: objc2::runtime::Sel, tag: isize, x: f64) -> Retained<NSButton> {
        let b = unsafe { NSButton::buttonWithTitle_target_action(&util::ns(title), Some(&actions()), Some(action), mtm()) };
        b.setTag(tag);
        b.sizeToFit();
        let w = b.frame().size.width.max(80.0);
        b.setFrame(rect(x, self.y, w, 28.0));
        self.page.addSubview(&b);
        b
    }

    fn row_with_button(&mut self, text: &str, title: &str, action: objc2::runtime::Sel, tag: isize) {
        let l = crate::ui::label(text, 12.5, false);
        l.setFrame(rect(24.0, self.y + 5.0, W - 200.0, 18.0));
        self.page.addSubview(&l);
        let b = self.button(title, action, tag, W - 170.0);
        b.setAccessibilityLabel(Some(&util::ns(&format!("{title}: {text}"))));
        self.y += 32.0;
    }

    fn checkbox(&mut self, title: &str, on: bool, action: objc2::runtime::Sel, tag: isize) -> Retained<NSButton> {
        let b = unsafe { NSButton::checkboxWithTitle_target_action(&util::ns(title), Some(&actions()), Some(action), mtm()) };
        b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        b.setTag(tag);
        b.setFrame(rect(24.0, self.y, W - 48.0, 22.0));
        self.page.addSubview(&b);
        self.y += 28.0;
        b
    }
}

fn when(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    }
}

fn rebuild() {
    let Some(window) = WINDOW.with_borrow(|w| w.clone()) else { return };
    let page: Retained<Page> = {
        let this = Page::alloc(mtm()).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, W, 100.0)] }
    };
    let mut b = Builder { page: page.clone(), y: 6.0 };
    let profiles = profiles();
    let selected = SELECTED.get();

    // ---- profile ------------------------------------------------------------
    b.heading("Profile");
    if profiles.is_empty() {
        b.text("No profiles yet.", true);
    } else {
        let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm()), rect(24.0, b.y, 280.0, 26.0), false);
        for (_, name) in &profiles {
            popup.addItemWithTitle(&util::ns(name));
        }
        if let Some(i) = profiles.iter().position(|p| Some(p.0) == selected) {
            popup.selectItemAtIndex(i as isize);
        }
        unsafe {
            popup.setTarget(Some(&actions()));
            popup.setAction(Some(sel!(pickProfile:)));
        }
        popup.setAccessibilityLabel(Some(&util::ns("Profile to change")));
        page.addSubview(&popup);
        b.y += 36.0;
    }
    if let Some(id) = selected {
        let r = app::store(|s| s.client(id).map(|c| c.retention())).unwrap_or_default();
        let keep = b.checkbox("Keep me signed in", r.keep, sel!(keepChanged:), 0);
        b.y -= 28.0;
        let days = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm()), rect(220.0, b.y - 2.0, 240.0, 26.0), false);
        for d in session::RETENTION_CHOICES {
            days.addItemWithTitle(&util::ns(&format!("for up to {d} days unused")));
        }
        days.selectItemAtIndex(session::RETENTION_CHOICES.iter().position(|d| *d == r.days).unwrap_or(2) as isize);
        days.setEnabled(r.keep);
        unsafe {
            days.setTarget(Some(&actions()));
            days.setAction(Some(sel!(keepChanged:)));
        }
        days.setAccessibilityLabel(Some(&util::ns("How long to stay signed in without using this profile")));
        page.addSubview(&days);
        CONTROLS.with_borrow_mut(|c| *c = Some((keep, days)));
        b.y += 30.0;
        b.text(
            "Kept sessions are restored only into this profile, for Zoho and Google sign-in cookies, and never longer than chosen. Signing out — or Zoho ending the session — always signs you out here too.",
            true,
        );
        match keychain::saved_login_email(id) {
            Some(email) => b.row_with_button(&format!("Saved login: {email}"), "Remove…", sel!(removeLogin:), 0),
            None => b.text("Saved login: none. (Add one from the profile's Customise sheet.)", true),
        }
        b.row_with_button("Cookies (signs this profile out of every site)", "Clear Cookies…", sel!(clearCookies:), 0);
        b.row_with_button("All website data: cookies, storage, cache, offline data", "Clear All…", sel!(clearAllData:), 0);
    }

    // ---- deletions -------------------------------------------------------------
    let jobs = cleanup::pending_jobs();
    if !jobs.is_empty() {
        b.heading("Unfinished profile deletions");
        for (i, job) in jobs.iter().enumerate() {
            b.row_with_button(&format!("“{}”: {} left", job.name, job.remaining().join(", ")), "Retry", sel!(retryCleanup:), i as isize);
        }
    }

    // ---- camera & microphone ----------------------------------------------
    b.heading("Camera, microphone and screen");
    b.text("Allowed without asking each time: Google Meet (meet.google.com) and Zoho Meeting and Cliq in each Zoho data centre, when they are the page itself. Every other site — including other Zoho apps and widgets — asks first. macOS also asks once for ZClients.", true);
    let calls = web::tabs_in_calls();
    if calls.is_empty() {
        b.text("Nothing is using the camera or microphone.", false);
    }
    for (i, (client, tab, cam, mic)) in calls.iter().enumerate() {
        let (profile, title) = app::store(|s| (s.client(*client).map(|c| c.name.clone()).unwrap_or_default(), s.find_tab(*tab).map(|t| t.display_title()).unwrap_or_default()));
        let what = match (cam, mic) {
            (true, true) => "Camera and microphone",
            (true, false) => "Camera",
            _ => "Microphone",
        };
        b.row_with_button(&format!("{what} on: “{title}” ({profile})"), "Stop", sel!(stopMedia:), i as isize);
    }
    if web::keeping_awake() {
        b.text("A call is on, so ZClients is keeping the display awake. This ends with the call.", true);
    }
    let uses = web::recent_media_uses();
    if !uses.is_empty() {
        b.text("Recent requests (this session only, not saved):", true);
        for u in uses.iter().take(8) {
            b.text(&format!("{} — {} — {} — {}", u.origin, u.what, u.decision, when(u.at)), true);
        }
    }

    // ---- downloads ---------------------------------------------------------
    b.heading("Downloads and PDF previews");
    b.text("A download a page starts without a recent click asks first. Apps, installers, disk images, scripts, archives and macro documents always ask.", true);
    let sites: Vec<(String, settings::SitePermission)> = settings::get(|s| s.downloads.iter().map(|(k, v)| (k.clone(), *v)).collect());
    for (i, (site, p)) in sites.iter().enumerate() {
        let what = match p {
            settings::SitePermission::Allow => "always allowed",
            settings::SitePermission::Block => "blocked",
            settings::SitePermission::Ask => "asks",
        };
        b.row_with_button(&format!("{site}: {what}"), "Reset", sel!(resetDownloadSite:), i as isize);
    }
    b.row_with_button("PDF previews no tab is showing, and the downloads list", "Clear…", sel!(clearPreviews:), 0);
    b.text("PDF previews are private copies kept only while their tab is open (or reopenable); the rest are removed at launch and at quit.", true);

    // ---- keyboard ---------------------------------------------------------
    b.heading("Keyboard");
    b.checkbox("Control-A selects all (as on Windows); off: start of line", settings::get(|s| s.control_a_selects_all), sel!(toggleSetting:), 1);
    b.checkbox("Web pages keep ⌘D, ⌘S, ⌘K and ⇧⌘S while they have the keyboard", settings::get(|s| s.page_owns_editor_shortcuts), sel!(toggleSetting:), 2);
    b.text("The app's own versions stay available: Toggle Sidebar ⌃⌘S, Google Search ⌥⌘K, Download Current Page ⌥⌘S, Duplicate Tab from the File menu.", true);

    // ---- developer (test builds only) ---------------------------------------
    if util::is_test_build() {
        b.heading("Developer (test build only)");
        b.checkbox("Developer mode: allow Safari's Web Inspector for newly opened tabs", settings::get(|s| s.developer_mode), sel!(toggleSetting:), 3);
    }

    // ---- about -------------------------------------------------------------
    b.heading("About");
    b.text(&format!("ZClients {} (build {}).", env!("CARGO_PKG_VERSION"), env!("ZCLIENTS_BUILD")), false);
    b.text(
        "ZClients does not check for updates or send anything anywhere by itself. Each profile's website data lives in its own WebKit store. Saved logins and kept sessions are encrypted in ZClients's vault; its key is protected by your Mac's Keychain. Diagnostics are only written when you export them, and are redacted first.",
        true,
    );
    b.button("Export Diagnostics…", sel!(exportDiagnostics:), 0, 24.0);
    b.y += 44.0;

    page.setFrame(rect(0.0, 0.0, W, b.y));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm()), rect(0.0, 0.0, W, 720.0));
    scroll.setHasVerticalScroller(true);
    scroll.setDocumentView(Some(&page));
    scroll.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable);
    window.setContentView(Some(&scroll));
}

fn apply_retention() {
    let Some(id) = SELECTED.get() else { return };
    let Some((keep, days)) = CONTROLS.with_borrow(|c| c.clone()) else { return };
    let r = session::Retention {
        keep: keep.state() == NSControlStateValueOn,
        days: session::RETENTION_CHOICES.get(days.indexOfSelectedItem().max(0) as usize).copied().unwrap_or(session::DEFAULT_RETENTION_DAYS),
    };
    days.setEnabled(r.keep);
    if app::mutate(app::TOOLBAR, |s| s.set_retention(id, r)) {
        web::retention_changed(id);
    }
}

/// Clear the selected profile's cookies (or all its website data), in both
/// its stores, and its kept-session backup so nothing comes back.
fn clear_profile_data(cookies_only: bool) {
    let Some(id) = SELECTED.get() else { return };
    let name = app::store(|s| s.client(id).map(|c| c.name.clone())).unwrap_or_default();
    let (title, body) = if cookies_only {
        (format!("Clear cookies for “{name}”?"), "This signs the profile out of Zoho, Google and every other site in it. Its tabs stay.")
    } else {
        (
            format!("Clear all website data for “{name}”?"),
            "This signs the profile out and removes its cookies, local storage, databases, caches and offline data. Its tabs, saved login and settings stay.",
        )
    };
    if !web::confirm(&title, body, "Clear") {
        return;
    }
    for ws in [Workspace::Zoho, Workspace::GoogleSearch] {
        let store_id = web::store_identifier(id, ws);
        session::clear_backup(store_id);
        web::clear_website_data(store_id, cookies_only, move |ok| {
            if !ok {
                web::run_alert("Some website data could not be cleared", "Try again in a moment.", &["OK"], None);
            }
        });
    }
}

// ---- diagnostics ------------------------------------------------------------

/// Remove anything personal or secret from diagnostic text: email
/// addresses, URL paths and queries (only scheme and host stay), UUIDs, and
/// long tokens (cookie values, keys, hashes).
pub fn redact(text: &str) -> String {
    text.split_inclusive(|c: char| c.is_whitespace())
        .map(|word| {
            let trimmed = word.trim_end();
            let tail = &word[trimmed.len()..];
            let core = trimmed.trim_matches(|c: char| "\"'(),;<>[]{}".contains(c));
            let redacted = redact_word(core);
            if redacted == core { word.to_string() } else { format!("{}{tail}", trimmed.replace(core, &redacted)) }
        })
        .collect()
}

fn redact_word(w: &str) -> String {
    if w.contains('@') && w.contains('.') {
        return "[email]".into();
    }
    if let Some(u) = crate::policy::parse(w).filter(|u| u.has_host()) {
        return format!("{}://{}/[…]", u.scheme(), u.host_str().unwrap_or(""));
    }
    // Looks like an address but does not parse: nothing of it is kept.
    if w.contains("://") || w.contains('?') {
        return "[url]".into();
    }
    if w.starts_with("file:") || w.starts_with('/') && w.len() > 1 {
        return "[path]".into();
    }
    if Uuid::parse_str(w).is_ok() {
        return "[id]".into();
    }
    let tokenish = w.len() >= 20 && w.chars().all(|c| c.is_ascii_alphanumeric() || "+/=_-.%".contains(c)) && w.chars().any(|c| c.is_ascii_digit());
    if tokenish {
        return "[token]".into();
    }
    w.to_string()
}

pub fn diagnostics_text() -> String {
    let mut out = String::new();
    let os = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersionString().to_string();
    out.push_str(&format!(
        "ZClients {} (build {})\nmacOS {os}\ntest build: {}\nprofiles file read-only (recovery): {}\n",
        env!("CARGO_PKG_VERSION"),
        env!("ZCLIENTS_BUILD"),
        util::is_test_build(),
        app::is_read_only()
    ));
    let rows: Vec<(usize, bool, u32, usize, bool)> = app::store(|s| {
        s.clients
            .iter()
            .filter(|c| !c.is_search())
            .map(|c| (c.tabs.len(), c.retention().keep, c.retention().days, c.tabs.iter().filter(|t| t.pdf_path.is_some()).count(), keychain::login_exists(c.id)))
            .collect()
    });
    out.push_str(&format!("profiles: {}\n", rows.len()));
    for (i, (tabs, keep, days, pdfs, login)) in rows.iter().enumerate() {
        out.push_str(&format!("  profile {}: {tabs} tab(s), {pdfs} PDF tab(s), keep signed in: {keep} ({days} d), saved login: {login}\n", i + 1));
    }
    out.push_str(&format!("awake tabs: {}\ncalls: {}\nkeeping display awake: {}\n", web::live_tabs().len(), web::tabs_in_calls().len(), web::keeping_awake()));
    out.push_str(&format!("unfinished deletions: {}\n", cleanup::pending_jobs().len()));
    for job in cleanup::pending_jobs() {
        out.push_str(&format!("  {} attempt(s); left: {}; last error: {}\n", job.attempts, job.remaining().join(", "), job.last_error.unwrap_or_default()));
    }
    let s = settings::get(|s| s.clone());
    out.push_str(&format!(
        "settings: control-A select-all {}, page owns editor keys {}, developer mode {}, inspector allowed {}\n",
        s.control_a_selects_all,
        s.page_owns_editor_shortcuts,
        s.developer_mode,
        settings::web_inspector_allowed()
    ));
    for (site, p) in &s.downloads {
        out.push_str(&format!("download permission: {site} {p:?}\n"));
    }
    for u in web::recent_media_uses() {
        out.push_str(&format!("media request: {} {} {}\n", u.origin, u.what, u.decision));
    }
    out.push_str(&format!("user agent suffix: {}\n", web::user_agent_suffix_in_use()));
    redact(&out)
}

pub fn export_diagnostics() {
    let text = diagnostics_text();
    let panel = objc2_app_kit::NSSavePanel::savePanel(mtm());
    panel.setNameFieldStringValue(&util::ns("ZClients Diagnostics.txt"));
    panel.setMessage(Some(&util::ns("Diagnostics are redacted: no addresses, page URLs, cookies, passwords or profile names.")));
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return;
    }
    let Some(path) = panel.URL().and_then(|u| u.path()).map(|p| p.to_string()) else { return };
    if let Err(e) = util::write_atomic(std::path::Path::new(&path), text.as_bytes(), 0o600) {
        web::run_alert("Diagnostics could not be saved", &e.to_string(), &["OK"], None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_never_carry_personal_or_secret_text() {
        let raw = "user jane.doe@acme.com opened https://crm.zoho.com/crm/org123/tab/Leads/5550001?token=abc and file:///Users/jane/Downloads/x.pdf cookie=Zx9aQ1b2C3d4E5f6G7h8I9j0 id 3F2504E0-4F89-11D3-9A0C-0305E82C3301 /Users/jane/Library";
        let r = redact(raw);
        for secret in ["jane", "acme", "org123", "5550001", "token=abc", "Zx9aQ1b2C3d4E5f6G7h8I9j0", "3F2504E0", "Downloads"] {
            assert!(!r.contains(secret), "{secret} leaked: {r}");
        }
        assert!(r.contains("https://crm.zoho.com/[…]"), "hosts stay: {r}");
        assert!(r.contains("opened"), "ordinary words stay");
        let r = redact("x https://xn--.com/0aa?q=0aa y");
        assert!(!r.contains("0aa"), "an unparseable address is removed whole: {r}");
    }

    #[test]
    fn redaction_keeps_plain_diagnostics() {
        let s = "profiles: 3\n  profile 1: 4 tab(s), keep signed in: true (90 d)\n";
        assert_eq!(redact(s), s);
    }
}
