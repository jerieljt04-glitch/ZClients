//! The WebKit layer: one `WKWebView` per awake tab, one persistent data store
//! per profile, and the delegates that decide what every page may do.
//!
//! Tabs stay awake like in a browser (10 on Macs with 10 GB of memory or
//! less, 16 up to 20 GB, otherwise 24): a background tab costs little,
//! because macOS compresses it, and putting one to sleep means it reloads
//! when the user comes back, which looks like a refresh nobody asked for.
//! Only real memory pressure puts tabs to sleep early, and Memory Saver puts
//! a background tab to sleep once nobody has looked at it for two hours (one
//! when memory is tight). A tab in a call is never put to sleep. A tab that
//! goes to sleep saves its exact place first and wakes up there, still signed
//! in.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSBackingStoreType, NSEvent, NSMenu, NSMenuItem, NSOpenPanel, NSResponder, NSTextField, NSUserInterfaceItemIdentification, NSView,
    NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueObservingOptions, NSNotification, NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration,
    NSObjectProtocol, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKContentWorld, WKDownload, WKFindConfiguration, WKFindResult, WKFrameInfo, WKMediaCaptureType, WKNavigation, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy, WKNavigationType, WKOpenPanelParameters, WKPermissionDecision, WKScriptMessage, WKScriptMessageHandler,
    WKSecurityOrigin, WKUIDelegate, WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use url::Url;
use uuid::Uuid;

use crate::model::{Tab, Workspace};
use crate::policy::{Capability, Decision, Origin};
use crate::{app, downloads, keychain, policy, scripts, session, settings, tabstate, ui, util};

/// How recently a real key press or click must have happened for a page to
/// open a window, start a download or open another app without asking.
/// Only native AppKit events count: page JavaScript cannot fake them.
const GESTURE_WINDOW: Duration = Duration::from_secs(5);
const DOWNLOAD_GESTURE_WINDOW: Duration = Duration::from_secs(10);
/// A popup may open one more popup (OAuth inside a widget), no deeper.
const MAX_POPUP_DEPTH: u8 = 2;

thread_local! {
    static LAST_INPUT: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn note_input() {
    LAST_INPUT.set(Some(Instant::now()));
}

/// A real keyboard or mouse event reached a page within `within`.
pub fn recent_gesture(within: Duration) -> bool {
    LAST_INPUT.get().is_some_and(|t| t.elapsed() < within)
}

const KVO_KEYS: &[&str] = &["title", "URL", "canGoBack", "canGoForward", "loading", "cameraCaptureState", "microphoneCaptureState"];

/// What WebKit appends to its own standard user agent: exactly what Safari
/// on this Mac appends ("Version/<installed Safari> Safari/605.1.15"), so
/// Zoho and Google serve the same desktop experience as to Safari. Nothing
/// is spoofed beyond that, and no Safari version is hard-coded: if Safari's
/// version cannot be read, only "Safari/605.1.15" is added.
fn application_name_for_user_agent() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let version = std::fs::read_to_string("/Applications/Safari.app/Contents/Info.plist").ok().and_then(|plist| safari_version(&plist));
        user_agent_suffix(version.as_deref())
    })
}

fn safari_version(plist: &str) -> Option<String> {
    let i = plist.find("<key>CFBundleShortVersionString</key>")?;
    let rest = &plist[i..];
    let a = rest.find("<string>")? + 8;
    let b = rest[a..].find("</string>")?;
    let v = rest[a..a + b].trim();
    (!v.is_empty() && v.len() < 16 && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_string())
}

fn user_agent_suffix(version: Option<&str>) -> String {
    match version {
        Some(v) => format!("Version/{v} Safari/605.1.15"),
        None => "Safari/605.1.15".into(),
    }
}

/// The Search workspace keeps its own cookie store, derived from the client
/// id the same way the Swift build did so existing Search sessions carry over.
pub fn store_identifier(client: Uuid, workspace: Workspace) -> Uuid {
    if workspace == Workspace::Zoho {
        return client;
    }
    let mut b = *client.as_bytes();
    for (i, k) in [0x47u8, 0x53, 0x45, 0x41, 0x52, 0x43, 0x48, 0x21].iter().enumerate() {
        b[i] ^= k;
    }
    Uuid::from_bytes(b)
}

// ---------------------------------------------------------------------------
// ZWebView: WKWebView plus right-click downloads.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct ContextTarget {
    link: Option<String>,
    image: Option<String>,
    media: Option<String>,
    name: Option<String>,
    at: Option<Instant>,
}

pub struct ZWebIvars {
    pub client_id: Uuid,
    pub workspace: Workspace,
    context: RefCell<Option<ContextTarget>>,
    menu_targets: RefCell<Vec<(String, Option<String>)>>,
    /// Keys held for the last click in this page, and when. A page's own
    /// click handler that calls window.open() (most links in Zoho) does
    /// not pass them on to WebKit's navigation action.
    last_click: Cell<Option<(objc2_app_kit::NSEventModifierFlags, Instant)>>,
    /// The keyboard is in a code editor (see `scripts::EDITOR_HOOK`).
    code_editor: Cell<bool>,
}

define_class!(
    #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ZWebIvars]
    pub struct ZWebView;

    impl ZWebView {
        // WebKit's own "Download Image/Linked File" items save outside the
        // app's view; they are replaced with items that go through the
        // Downloads list and the download allowlist.
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, menu: &NSMenu, event: &NSEvent) {
            unsafe {
                let _: () = msg_send![super(self), willOpenMenu: menu, withEvent: event];
            }
            self.decorate_menu(menu);
        }

        // WebKit claims every ⌘-shortcut for the page and never hands the
        // unhandled ones back to the menu bar, so with a page focused ⌘V,
        // ⌘S, ⌘T, ⌘D... did nothing. Offer them to the app's menus first, as
        // Safari does: Paste then runs WebKit's own paste on this view (the
        // page still gets its paste event), and anything the menus don't
        // define — ⌘Enter in a Zoho editor, say — still reaches the page.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            note_input();
            // Control+A selects all too, the way it does in browsers on
            // Windows (on a Mac it would only move to the start of a line).
            // A setting turns this back into the Mac behaviour.
            // In a code editor, Select All / Undo / Redo are the editor's own:
            // it gets the keystroke, as in Safari.
            if self.ivars().code_editor.get() {
                use objc2_app_kit::NSEventModifierFlags as M;
                if is_key(event, "a", M::Command) || is_key(event, "z", M::Command) || is_key(event, "z", M::Command | M::Shift) {
                    unsafe {
                        let _: () = msg_send![self, keyDown: event];
                    }
                    return true.into();
                }
                if is_key(event, "a", M::Control) && settings::get(|s| s.control_a_selects_all) {
                    if let Some(cmd_a) = as_command_key(event, "a") {
                        unsafe {
                            let _: () = msg_send![self, keyDown: &*cmd_a];
                        }
                    }
                    return true.into();
                }
            }
            if is_key(event, "a", objc2_app_kit::NSEventModifierFlags::Control) && settings::get(|s| s.control_a_selects_all) {
                unsafe {
                    let _: () = msg_send![self, selectAll: None::<&AnyObject>];
                }
                return true.into();
            }
            let on_meet = ns_str_url(unsafe { self.URL() }).and_then(|u| policy::parse(&u)).is_some_and(|u| policy::is_google_meet(&u));
            menu_first(event, on_meet) || unsafe { msg_send![super(self), performKeyEquivalent: event] }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            note_input();
            unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            note_input();
            self.ivars().last_click.set(Some((event.modifierFlags(), Instant::now())));
            unsafe {
                let _: () = msg_send![super(self), mouseDown: event];
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            note_input();
            unsafe {
                let _: () = msg_send![super(self), rightMouseDown: event];
            }
        }

        #[unsafe(method(zcOpenLinkInTab:))]
        fn open_link_in_tab(&self, sender: &NSMenuItem) {
            let target = self.ivars().menu_targets.borrow().get(sender.tag() as usize).cloned();
            let Some((url, _)) = target else { return };
            let Some(u) = policy::parse(&url).filter(|u| policy::is_allowed_web(u, self.ivars().workspace)) else { return };
            let background = sender.representedObject().is_some();
            let (client, ws) = (self.ivars().client_id, self.ivars().workspace);
            let opener = POOL.with_borrow(|p| p.live.iter().find(|(_, l)| std::ptr::eq(&*l.view, self)).map(|(id, _)| *id));
            util::later(move || app::open_new_tab_from_page(client, ws, u, opener, background));
        }

        #[unsafe(method(zcDownloadContext:))]
        fn download_context(&self, sender: &NSMenuItem) {
            let index = sender.tag() as usize;
            let target = self.ivars().menu_targets.borrow().get(index).cloned();
            if let Some((url, name)) = target {
                start_context_download(self, &url, name);
            }
        }
    }
);

/// App shortcuts that are also common editor shortcuts in web apps (Zoho
/// Sheet fill-down ⌘D, Writer save ⌘S, insert link ⌘K, save as ⇧⌘S). While
/// a page has the keyboard they go to the page; the app's own commands keep
/// their always-available alternatives (⌃S, ⌥⌘K, ⌥⌘S, menus).
pub fn page_owned_shortcut(chars: &str, mods: objc2_app_kit::NSEventModifierFlags, on_meet: bool, page_owns_editor_keys: bool) -> bool {
    use objc2_app_kit::NSEventModifierFlags as M;
    let held = mods & (M::Command | M::Shift | M::Option | M::Control);
    let key = chars.to_ascii_lowercase();
    // In Google Meet ⌘D mutes the microphone; the menu's Duplicate Tab must
    // not take it (it opened a second copy of the call). Always.
    if on_meet && held == M::Command && key == "d" {
        return true;
    }
    page_owns_editor_keys && ((held == M::Command && matches!(key.as_str(), "d" | "s" | "k")) || (held == M::Command | M::Shift && key == "s"))
}

fn menu_first(event: &NSEvent, on_meet: bool) -> bool {
    let flags = event.modifierFlags();
    if !flags.intersects(objc2_app_kit::NSEventModifierFlags::Command | objc2_app_kit::NSEventModifierFlags::Control) {
        return false;
    }
    let chars = event.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
    if page_owned_shortcut(&chars, flags, on_meet, settings::get(|s| s.page_owns_editor_shortcuts)) {
        return false;
    }
    let Some(mtm) = MainThreadMarker::new() else { return false };
    NSApplication::sharedApplication(mtm).mainMenu().is_some_and(|m| m.performKeyEquivalent(event))
}

/// The same key press with ⌘ instead of its modifiers (Control-A as ⌘A for
/// the Windows select-all habit, in code editors).
fn as_command_key(event: &NSEvent, chars: &str) -> Option<Retained<NSEvent>> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        event.r#type(),
        event.locationInWindow(),
        objc2_app_kit::NSEventModifierFlags::Command,
        event.timestamp(),
        event.windowNumber(),
        None,
        &util::ns(chars),
        &util::ns(chars),
        false,
        event.keyCode(),
    )
}

/// `event` is exactly `mods` + `key` (no other modifier held).
fn is_key(event: &NSEvent, key: &str, mods: objc2_app_kit::NSEventModifierFlags) -> bool {
    use objc2_app_kit::NSEventModifierFlags as M;
    let held = event.modifierFlags() & (M::Command | M::Shift | M::Option | M::Control);
    held == mods && event.charactersIgnoringModifiers().is_some_and(|c| c.to_string().eq_ignore_ascii_case(key))
}

impl ZWebView {
    fn decorate_menu(&self, menu: &NSMenu) {
        let target = self.ivars().context.borrow().clone();
        // A target older than a few seconds belongs to an earlier right-click.
        let Some(t) = target.filter(|t| t.at.is_some_and(|at| at.elapsed() < Duration::from_secs(5))) else { return };
        let ws = self.ivars().workspace;
        let ok = |u: &Option<String>| u.as_deref().and_then(policy::parse).is_some_and(|u| policy::is_safe_download(&u, ws));
        let mut items: Vec<(&str, String)> = vec![];
        let showable = t.link.as_deref().and_then(policy::parse).is_some_and(|u| policy::is_allowed_web(&u, ws));
        if ok(&t.link) {
            items.push(("Download Linked File", t.link.clone().unwrap()));
        }
        if ok(&t.image) {
            items.push(("Save Image to Downloads", t.image.clone().unwrap()));
        }
        if ok(&t.media) {
            items.push(("Save Video to Downloads", t.media.clone().unwrap()));
        }
        if items.is_empty() && !showable {
            return;
        }
        let replaced = ["WKMenuItemIdentifierDownloadImage", "WKMenuItemIdentifierDownloadLinkedFile", "WKMenuItemIdentifierDownloadMedia"];
        for item in menu.itemArray().iter() {
            if item.identifier().is_some_and(|id| replaced.contains(&id.to_string().as_str())) {
                menu.removeItem(&item);
            }
        }
        let mtm = self.mtm();
        let mut targets = self.ivars().menu_targets.borrow_mut();
        targets.clear();
        for (i, (title, url)) in items.iter().enumerate() {
            let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &util::ns(title), Some(sel!(zcDownloadContext:)), &util::ns("")) };
            unsafe { item.setTarget(Some(self)) };
            item.setTag(i as isize);
            menu.insertItem_atIndex(&item, i as isize);
            targets.push((url.clone(), t.name.clone()));
        }
        let mut at = items.len() as isize;
        if showable {
            // WebKit's own "Open Link in New Window" stays; these add the
            // browser-style tab variants.
            let link = t.link.clone().unwrap_or_default();
            for (title, background) in [("Open Link in New Tab", false), ("Open Link in Background Tab", true)] {
                let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &util::ns(title), Some(sel!(zcOpenLinkInTab:)), &util::ns("")) };
                unsafe { item.setTarget(Some(self)) };
                item.setTag(targets.len() as isize);
                if background {
                    unsafe { item.setRepresentedObject(Some(&util::ns("background"))) };
                }
                targets.push((link.clone(), None));
                menu.insertItem_atIndex(&item, at);
                at += 1;
            }
        }
        menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), at);
    }

    fn client_world(&self) -> Retained<WKContentWorld> {
        unsafe { WKContentWorld::defaultClientWorld(self.mtm()) }
    }
}

fn new_zweb(mtm: MainThreadMarker, frame: NSRect, cfg: &WKWebViewConfiguration, client: Uuid, ws: Workspace) -> Retained<ZWebView> {
    let this = ZWebView::alloc(mtm).set_ivars(ZWebIvars {
        client_id: client,
        workspace: ws,
        context: RefCell::new(None),
        menu_targets: RefCell::new(vec![]),
        last_click: Cell::new(None),
        code_editor: Cell::new(false),
    });
    let web: Retained<ZWebView> = unsafe { msg_send![super(this), initWithFrame: frame, configuration: cfg] };
    unsafe {
        web.setAllowsBackForwardNavigationGestures(true);
        web.setAllowsMagnification(true);
        // Safari's Web Inspector can attach only in debug builds, or in the
        // test app with Developer Mode on. Never in the released app.
        web.setInspectable(settings::web_inspector_allowed());
    }
    web
}

fn start_context_download(web: &ZWebView, url: &str, name: Option<String>) {
    let ws = web.ivars().workspace;
    let Some(parsed) = policy::parse(url) else { return };
    if !policy::is_safe_download(&parsed, ws) {
        return;
    }
    if parsed.scheme() == "https" {
        let Some(nsurl) = util::ns_url(url) else { return };
        let req = NSURLRequest::requestWithURL(&nsurl);
        let block = RcBlock::new(|d: NonNull<WKDownload>| downloads::adopt(unsafe { d.as_ref() }));
        unsafe { web.startDownloadUsingRequest_completionHandler(&req, &block) };
        return;
    }
    // blob: and data: have no network request to hand WebKit, so the bytes
    // are read back through the page that owns them.
    let fallback = inline_filename(&parsed, name.as_deref(), None);
    let args = NSDictionary::<NSString, AnyObject>::from_retained_objects(
        &[&*util::ns("u"), &*util::ns("limit")],
        &[util::any(util::ns(url)), util::any(NSNumber::new_i64(MAX_INLINE_BYTES as i64))],
    );
    let parsed2 = parsed.clone();
    let client = web.ivars().client_id;
    let block = RcBlock::new(move |result: *mut AnyObject, _err: *mut NSError| {
        let dict = unsafe { result.as_ref() }.and_then(|r| r.downcast_ref::<NSDictionary>());
        let get = |k: &str| -> Option<String> {
            let d = dict?;
            let v = d.objectForKey(&util::ns(k))?;
            v.downcast_ref::<NSString>().map(|s| s.to_string())
        };
        // The page reports the size, but the decoded length is what counts.
        match get("data").filter(|b64| b64.len() / 4 * 3 <= MAX_INLINE_BYTES + 3).and_then(|b64| decode_base64(&b64)).filter(|b| b.len() <= MAX_INLINE_BYTES) {
            Some(bytes) => {
                let name = inline_filename(&parsed2, name.as_deref(), get("type").as_deref());
                downloads::save_bytes(bytes, &name, Some(client));
            }
            None => downloads::record_failure(&fallback),
        }
    });
    unsafe {
        web.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(&util::ns(scripts::READ_INLINE_BODY), Some(&args), None, &web.client_world(), Some(&block))
    };
}

/// blob:/data: downloads are read through the page, so they are capped.
pub const MAX_INLINE_BYTES: usize = 32 * 1024 * 1024;

fn inline_filename(url: &Url, suggested: Option<&str>, mime: Option<&str>) -> String {
    let mut base = suggested.unwrap_or("").trim().to_string();
    if base.is_empty() && url.scheme() != "blob" && url.scheme() != "data" {
        base = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("").to_string();
    }
    if base.is_empty() {
        base = "Download".into();
    }
    if !base.contains('.') {
        let ext = match mime.unwrap_or("").split(';').next().unwrap_or("").trim() {
            "application/pdf" => "pdf",
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "text/csv" => "csv",
            "text/plain" => "txt",
            "application/zip" => "zip",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
            "video/mp4" => "mp4",
            _ => "",
        };
        if !ext.is_empty() {
            base = format!("{base}.{ext}");
        }
    }
    base
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Script message bridge (client world only).
// ---------------------------------------------------------------------------

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct Bridge;

    unsafe impl NSObjectProtocol for Bridge {}

    unsafe impl WKScriptMessageHandler for Bridge {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _ucc: &WKUserContentController, message: &WKScriptMessage) {
            let name = unsafe { message.name() }.to_string();
            let Some(web) = (unsafe { message.webView() }) else { return };
            let Some(web) = web.downcast_ref::<ZWebView>() else { return };
            let body = unsafe { message.body() };
            let Some(dict) = body.downcast_ref::<NSDictionary>() else { return };
            // Everything here is page-controlled text: parse defensively.
            let get = |k: &str| -> Option<String> {
                let v = dict.objectForKey(&util::ns(k))?;
                let s = v.downcast_ref::<NSString>()?.to_string();
                (!s.is_empty() && s.len() < 4096).then_some(s)
            };
            let frame = unsafe { message.frameInfo() };
            let request = request_for(web, Some(&frame), web.ivars().workspace);
            if name == scripts::EDITOR_HANDLER {
                if policy::decide(Capability::Bridge, &request) == Decision::Allow {
                    web.ivars().code_editor.set(get("editor").as_deref() == Some("yes"));
                }
                return;
            }
            if name == scripts::CONTEXT_HANDLER {
                if policy::decide(Capability::Bridge, &request) != Decision::Allow {
                    return;
                }
                let t = ContextTarget { link: get("link"), image: get("image"), media: get("media"), name: get("name"), at: Some(Instant::now()) };
                *web.ivars().context.borrow_mut() = Some(t);
            } else if name == scripts::LOGIN_HANDLER {
                // Only Zoho's own sign-in page, as the tab's main frame, may
                // hand us a password.
                if policy::decide(Capability::Autofill, &request) != Decision::Allow {
                    return;
                }
                if let (Some(email), Some(password)) = (get("email"), get("password")) {
                    let client = web.ivars().client_id;
                    util::later(move || app::offer_to_save_login(client, email, password));
                }
            }
        }
    }
);

// ---------------------------------------------------------------------------
// KVO: title / URL / back-forward / loading changes.
// ---------------------------------------------------------------------------

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Uuid]
    struct Observer;

    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&NSDictionary>, _ctx: *mut c_void) {
            // Never act inline: KVO can fire from inside our own WebKit calls.
            if key.is_some_and(|k| k.to_string().ends_with("CaptureState")) {
                util::later(|| {
                    refresh_call_state();
                    app::changed(app::TABS);
                });
                return;
            }
            app::web_state_changed(*self.ivars());
        }
    }
);

// ---------------------------------------------------------------------------
// Coordinator: navigation + UI delegate for one tab or popup window.
// ---------------------------------------------------------------------------

pub struct CoordIvars {
    client_id: Uuid,
    tab_id: Option<Uuid>,
    workspace: Workspace,
    /// One-navigation latch: a trusted page may open an off-list HTTPS link
    /// just far enough to see whether it is a file. Cleared on commit and on
    /// both failure callbacks, otherwise it would become a general bypass.
    external_probe: Cell<bool>,
    /// That probe came from the user clicking a link, so an ordinary web page
    /// at the end of it opens in the default browser instead of nowhere.
    external_user: Cell<bool>,
    popup_window: RefCell<Option<Retained<NSWindow>>>,
    /// Popups opened by a trusted Zoho page may visit other sign-in providers
    /// (OAuth connections in widgets and integrations).
    popup_trusted: bool,
    /// A page opened this tab or popup and nothing has been shown in it yet.
    /// If its first load turns out to be a file, it closes once the download
    /// has started, like a browser tab that only downloaded something.
    fresh: Cell<bool>,
    /// The popup's main-frame response is a PDF being fetched to open as a
    /// tab; holds the address it came from (when that is worth keeping).
    pending_pdf: RefCell<Option<Option<String>>>,
    /// 0 for a tab, 1 for a popup a tab opened, 2 for a popup of a popup.
    depth: u8,
    /// When this page last showed alert/confirm/prompt dialogs.
    dialogs: RefCell<Vec<Instant>>,
    /// The user chose "Don't allow more dialogs from this page"; cleared
    /// when the tab goes to another page.
    dialogs_blocked: Cell<bool>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CoordIvars]
    struct Coordinator;

    unsafe impl NSObjectProtocol for Coordinator {}

    unsafe impl WKNavigationDelegate for Coordinator {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(&self, web: &WKWebView, action: &WKNavigationAction, handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>) {
            handler.call((self.action_policy(web, action),));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(&self, web: &WKWebView, response: &WKNavigationResponse, handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>) {
            handler.call((self.response_policy(web, response),));
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, web: &WKWebView, _nav: Option<&WKNavigation>) {
            self.ivars().external_probe.set(false);
            // Suppressed dialogs last until the page changes, as in Safari.
            self.ivars().dialogs_blocked.set(false);
            self.ivars().dialogs.borrow_mut().clear();
            // A new page starts outside any code editor; the editor hook
            // reports again when the keyboard enters one. Otherwise ⌘A/⌘Z
            // could stay routed as editor keys on the next page.
            if let Some(z) = web.downcast_ref::<ZWebView>() {
                z.ivars().code_editor.set(false);
            }
            if let Some(tab) = self.ivars().tab_id {
                ui::clear_load_error(tab);
            }
            if ns_str_url(unsafe { web.URL() }).is_some_and(|u| u != "about:blank") {
                self.ivars().fresh.set(false);
            }
            // The saved zoom goes on as soon as the new page commits, before
            // it first paints; applied only on finish, pages showed at 100 %
            // and then jumped.
            let iv = self.ivars();
            if let Some(url) = ns_str_url(unsafe { web.URL() }).and_then(|s| policy::parse(&s)).filter(|u| policy::is_allowed_web(u, iv.workspace)) {
                apply_saved_zoom(web, iv.client_id, &url);
            }
            self.state_changed(web);
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web: &WKWebView, _nav: Option<&WKNavigation>) {
            self.state_changed(web);
            if let Some(win) = self.ivars().popup_window.borrow().as_ref()
                && let Some(t) = unsafe { web.title() } {
                    win.setTitle(&t);
                }
            let iv = self.ivars();
            if let Some(url) = ns_str_url(unsafe { web.URL() }).and_then(|s| policy::parse(&s)).filter(|u| policy::is_allowed_web(u, iv.workspace)) {
                session::note_page_loaded(store_identifier(iv.client_id, iv.workspace));
                apply_saved_zoom(web, iv.client_id, &url);
            }
            self.maybe_sign_in(web);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, web: &WKWebView, _nav: Option<&WKNavigation>, e: &NSError) {
            self.ivars().external_probe.set(false);
            self.state_changed(web);
            if let (Some(tab), Some(kind)) = (self.ivars().tab_id, load_error_kind(&e.domain().to_string(), e.code())) {
                let failing = e.userInfo().objectForKey(&util::ns("NSErrorFailingURLStringKey")).and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string());
                ui::show_load_error(tab, kind, failing);
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, web: &WKWebView, _nav: Option<&WKNavigation>, _e: &NSError) {
            self.ivars().external_probe.set(false);
            self.state_changed(web);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_died(&self, web: &WKWebView) {
            // macOS ends background pages' processes to free memory. Reloading
            // them on the spot looked like pages refreshing by themselves and
            // spent the memory again; a background tab goes to sleep instead
            // and comes back (at its saved place) when it is next shown.
            if let Some(tab) = self.ivars().tab_id
                && POOL.with_borrow(|p| p.active) != Some(tab) {
                    util::later(move || release(tab));
                    return;
                }
            let url = ns_str_url(unsafe { web.URL() });
            let ok = url.as_deref().and_then(policy::parse).is_some_and(|u| policy::is_allowed_web(&u, self.ivars().workspace));
            if !ok {
                return;
            }
            // A page that keeps crashing WebKit would otherwise reload
            // forever: after a second crash within a minute it waits for
            // the user (Try Again) instead.
            let again = self.ivars().tab_id.is_some_and(|tab| POOL.with_borrow_mut(|p| !crash_allows_reload(p.crashes.entry(tab).or_default(), Instant::now())));
            match self.ivars().tab_id {
                Some(tab) if again => {
                    util::log("a page crashed twice within a minute; not reloading it again");
                    util::later(move || ui::show_load_error(tab, LoadError::Crashed, url));
                }
                _ => unsafe {
                    web.reload();
                },
            }
        }

        #[unsafe(method(webView:authenticationChallenge:shouldAllowDeprecatedTLS:))]
        fn deprecated_tls(&self, _web: &WKWebView, _c: &objc2_foundation::NSURLAuthenticationChallenge, decide: &block2::DynBlock<dyn Fn(Bool)>) {
            decide.call((Bool::NO,));
        }

        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        fn response_became_download(&self, _web: &WKWebView, _r: &WKNavigationResponse, download: &WKDownload) {
            let iv = self.ivars();
            match iv.pending_pdf.take() {
                Some(source) => downloads::adopt_as_pdf_tab(download, iv.client_id, source),
                None => downloads::adopt(download),
            }
            self.close_if_blank();
        }

        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        fn action_became_download(&self, _web: &WKWebView, _a: &WKNavigationAction, download: &WKDownload) {
            downloads::adopt(download);
            self.close_if_blank();
        }
    }

    impl Coordinator {
        /// Screen sharing (Meet's "Present now"). WebKit only offers
        /// getDisplayMedia to apps that answer this private delegate method
        /// (the one Safari implements); without it every request was refused.
        /// Meet and Zoho's own apps get the system's screen/window picker.
        #[unsafe(method(_webView:requestDisplayCapturePermissionForOrigin:initiatedByFrame:withSystemAudio:decisionHandler:))]
        fn display_capture_permission(&self, _web: &WKWebView, origin: &WKSecurityOrigin, _frame: &WKFrameInfo, _audio: Bool, decide: &block2::DynBlock<dyn Fn(isize)>) {
            const DENY: isize = 0;
            const SCREEN_PROMPT: isize = 1;
            let request = request_for(_web, Some(_frame), self.ivars().workspace);
            let (ok, how) = match policy::decide(Capability::ScreenCapture, &request) {
                Decision::Allow => (true, "allowed"),
                Decision::Prompt => {
                    let who = request.origin.as_ref().map(Origin::display).unwrap_or_default();
                    (confirm(&format!("Allow {who} to share your screen?"), "macOS will then ask which screen or window to share.", "Choose What to Share"), "asked")
                }
                Decision::Deny => (false, "denied"),
            };
            record_media_use(request.origin.as_ref(), "screen", if ok { how } else { "denied" });
            decide.call((if ok { SCREEN_PROMPT } else { DENY },));
        }

        /// The download button in WebKit's PDF viewer. WebKit hands the
        /// document to this (private, Safari-implemented) delegate method and
        /// does nothing at all when it is missing, which is why the button
        /// used to be dead.
        #[unsafe(method(_webView:saveDataToFile:suggestedFilename:mimeType:originatingURL:))]
        fn save_data_to_file(&self, _web: &WKWebView, data: &NSData, suggested: &NSString, mime: Option<&NSString>, _url: Option<&NSURL>) {
            let iv = self.ivars();
            // A PDF tab's file is named by an id; the tab title is its real name.
            let tab_title = iv.tab_id.and_then(|id| app::store(|s| s.find_tab(id).filter(|t| t.pdf_path.is_some()).map(|t| t.title.clone())));
            let is_pdf = mime.is_some_and(|m| m.to_string().eq_ignore_ascii_case("application/pdf"));
            let name = match tab_title {
                Some(title) => downloads::pdf_name(&title),
                None if is_pdf => downloads::pdf_name(&suggested.to_string()),
                None => downloads::safe_filename(&suggested.to_string()),
            };
            downloads::save_from_tab(data.to_vec(), &name, iv.client_id, iv.tab_id);
            util::later(ui::show_downloads);
        }
    }

    unsafe impl WKUIDelegate for Coordinator {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(&self, web: &WKWebView, cfg: &WKWebViewConfiguration, action: &WKNavigationAction, features: &WKWindowFeatures) -> Option<Retained<WKWebView>> {
            self.open_window(web, cfg, action, features)
        }

        #[unsafe(method(webViewDidClose:))]
        fn did_close(&self, _web: &WKWebView) {
            if let Some(win) = self.ivars().popup_window.borrow().clone() {
                util::later(move || win.close());
            } else if let Some(tab) = self.ivars().tab_id {
                let client = self.ivars().client_id;
                util::later(move || app::close_tab(client, tab));
            }
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_alert(&self, web: &WKWebView, message: &NSString, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn()>) {
            let done = done.copy();
            self.page_dialog(web, frame, message.to_string(), &["OK"], None, move |_| done.call(()));
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_confirm(&self, web: &WKWebView, message: &NSString, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn(Bool)>) {
            let done = done.copy();
            self.page_dialog(web, frame, message.to_string(), &["OK", "Cancel"], None, move |answer| {
                done.call((Bool::new(matches!(answer, Some((0, _)))),));
            });
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn js_prompt(&self, web: &WKWebView, prompt: &NSString, default: Option<&NSString>, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn(*mut NSString)>) {
            let done = done.copy();
            let default = default.map(|d| d.to_string()).unwrap_or_default();
            self.page_dialog(web, frame, prompt.to_string(), &["OK", "Cancel"], Some(default), move |answer| match answer {
                Some((0, Some(t))) => {
                    let s = util::ns(&t);
                    done.call((Retained::as_ptr(&s) as *mut NSString,));
                }
                _ => done.call((std::ptr::null_mut(),)),
            });
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media_permission(&self, web: &WKWebView, origin: &WKSecurityOrigin, frame: &WKFrameInfo, kind: WKMediaCaptureType, decide: &block2::DynBlock<dyn Fn(WKPermissionDecision)>) {
            let mut request = request_for(web, Some(frame), self.ivars().workspace);
            // WebKit names the origin asking; it must agree with the frame.
            let asked = Origin::from_parts(&unsafe { origin.protocol() }.to_string(), &unsafe { origin.host() }.to_string(), unsafe { origin.port() } as i64);
            if asked != request.origin {
                request.origin = None;
            }
            let cap = if kind == WKMediaCaptureType::Microphone { Capability::Microphone } else { Capability::Camera };
            let decision = match policy::decide(cap, &request) {
                Decision::Allow => WKPermissionDecision::Grant,
                Decision::Prompt => WKPermissionDecision::Prompt,
                Decision::Deny => WKPermissionDecision::Deny,
            };
            let what = match kind {
                WKMediaCaptureType::Camera => "camera",
                WKMediaCaptureType::Microphone => "microphone",
                _ => "camera and microphone",
            };
            let how = if decision == WKPermissionDecision::Grant { "allowed" } else if decision == WKPermissionDecision::Prompt { "asked" } else { "denied" };
            record_media_use(request.origin.as_ref(), what, how);
            decide.call((decision,));
        }

        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn open_panel(&self, web: &WKWebView, params: &WKOpenPanelParameters, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn(*mut NSArray<NSURL>)>) {
            if policy::decide(Capability::FileChooser, &request_for(web, Some(frame), self.ivars().workspace)) != Decision::Allow {
                return done.call((std::ptr::null_mut(),));
            }
            let panel = NSOpenPanel::openPanel(self.mtm());
            unsafe {
                panel.setAllowsMultipleSelection(params.allowsMultipleSelection());
                panel.setCanChooseDirectories(params.allowsDirectories());
                panel.setCanChooseFiles(true);
                panel.setCanCreateDirectories(false);
            }
            let _ = web;
            let response = panel.runModal();
            if response == objc2_app_kit::NSModalResponseOK {
                let urls = panel.URLs();
                done.call((Retained::as_ptr(&urls) as *mut NSArray<NSURL>,));
            } else {
                done.call((std::ptr::null_mut(),));
            }
        }
    }

    unsafe impl NSWindowDelegate for Coordinator {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            let me = self as *const Coordinator;
            util::later(move || POOL.with_borrow_mut(|p| p.popups.retain(|(c, _)| Retained::as_ptr(c) != me)));
        }
    }
);

/// The keys held for the click behind `action`: what WebKit recorded with
/// it, plus the keys of a click in `web` within the last second (a page
/// script that opens the link itself loses them on the way).
fn click_modifiers(web: &WKWebView, action: &WKNavigationAction) -> objc2_app_kit::NSEventModifierFlags {
    let recorded = unsafe { action.modifierFlags() };
    let clicked = web
        .downcast_ref::<ZWebView>()
        .and_then(|z| z.ivars().last_click.get())
        .filter(|(_, at)| at.elapsed() < Duration::from_secs(1))
        .map(|(m, _)| m)
        .unwrap_or(objc2_app_kit::NSEventModifierFlags::empty());
    recorded | clicked
}

/// ⌘-click: open in a new tab.
fn new_tab_click(web: &WKWebView, action: &WKNavigationAction) -> bool {
    click_modifiers(web, action).contains(objc2_app_kit::NSEventModifierFlags::Command)
}

/// ⇧⌘-click: that new tab comes to the front.
fn wants_front(web: &WKWebView, action: &WKNavigationAction) -> bool {
    use objc2_app_kit::NSEventModifierFlags as M;
    click_modifiers(web, action).contains(M::Command | M::Shift)
}

fn frame_origin(frame: &WKFrameInfo) -> Option<Origin> {
    let o = unsafe { frame.securityOrigin() };
    Origin::from_parts(&unsafe { o.protocol() }.to_string(), &unsafe { o.host() }.to_string(), unsafe { o.port() } as i64)
}

/// Everything `policy::decide` needs about a request from `frame` (None:
/// the tab's main frame) of `web`, read live from WebKit.
fn request_for(web: &WKWebView, frame: Option<&WKFrameInfo>, workspace: Workspace) -> policy::Request {
    let top = ns_str_url(unsafe { web.URL() }).and_then(|u| policy::parse(&u)).and_then(|u| Origin::of(&u));
    let (origin, main_frame) = match frame {
        Some(f) => (frame_origin(f), unsafe { f.isMainFrame() }),
        None => (top.clone(), true),
    };
    policy::Request { origin, top, main_frame, user_gesture: recent_gesture(GESTURE_WINDOW), workspace }
}

/// A two-button confirmation. True for the first button.
pub fn confirm(title: &str, message: &str, ok: &str) -> bool {
    run_alert(title, message, &[ok, "Cancel"], None).0 == 0
}

/// One camera/microphone/screen request, kept in memory for the Privacy
/// window (never written to disk).
#[derive(Clone)]
pub struct MediaUse {
    pub origin: String,
    pub what: &'static str,
    /// "allowed" (no prompt), "asked" (the user was prompted) or "denied".
    pub decision: &'static str,
    pub at: std::time::SystemTime,
}

thread_local! {
    static MEDIA_LOG: RefCell<Vec<MediaUse>> = const { RefCell::new(Vec::new()) };
}

fn record_media_use(origin: Option<&Origin>, what: &'static str, decision: &'static str) {
    let origin = origin.map(Origin::display).unwrap_or_else(|| "an unidentified frame".into());
    MEDIA_LOG.with_borrow_mut(|l| {
        l.insert(0, MediaUse { origin, what, decision, at: std::time::SystemTime::now() });
        l.truncate(30);
    });
}

pub fn recent_media_uses() -> Vec<MediaUse> {
    MEDIA_LOG.with_borrow(|l| l.clone())
}

/// Why a page failed to load, for the error screen. None for failures that
/// are not errors to show (a navigation cancelled or turned into a download).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadError {
    /// The page's web content process ended twice in a short time.
    Crashed,
    /// A PDF tab whose preview copy is missing or no longer a valid PDF.
    PdfMissing,
    /// The tab's saved address is not one this app shows any more.
    NotAllowed,
    Offline,
    Certificate,
    Authentication,
    Unsupported,
    Other,
}

pub fn load_error_kind(domain: &str, code: isize) -> Option<LoadError> {
    match (domain, code) {
        // Cancelled (a new navigation started) / frame load interrupted by a
        // policy change or a download: not an error the user needs to see.
        ("NSURLErrorDomain", -999) | ("WebKitErrorDomain", 102) | ("WebKitErrorDomain", 204) => None,
        ("NSURLErrorDomain", -1009 | -1005 | -1001 | -1003 | -1004 | -1020 | -1018) => Some(LoadError::Offline),
        ("NSURLErrorDomain", -1206..=-1200) => Some(LoadError::Certificate),
        ("NSURLErrorDomain", -1013 | -1012) => Some(LoadError::Authentication),
        ("NSURLErrorDomain", -1002) | ("WebKitErrorDomain", 100 | 101) => Some(LoadError::Unsupported),
        _ => Some(LoadError::Other),
    }
}

/// How often a page may crash before it is no longer reloaded by itself.
const CRASH_LIMIT: usize = 2;
const CRASH_WINDOW: Duration = Duration::from_secs(60);

/// Record a crash at `now` in `recent` (that tab's earlier crashes) and say
/// whether reloading on the spot is still reasonable.
fn crash_allows_reload(recent: &mut Vec<Instant>, now: Instant) -> bool {
    recent.retain(|t| now.saturating_duration_since(*t) < CRASH_WINDOW);
    recent.push(now);
    recent.len() < CRASH_LIMIT
}

fn apply_saved_zoom(web: &WKWebView, client: Uuid, url: &Url) {
    let Some(host) = url.host_str() else { return };
    let zoom = settings::zoom_for(client, host).unwrap_or(1.0).clamp(0.5, 3.0);
    unsafe {
        if (web.pageZoom() - zoom).abs() > 0.001 {
            web.setPageZoom(zoom);
        }
    }
}

/// A page wants to download a file without (or outside) a recent click,
/// or from a page the app does not show. Asks, honouring the per-site
/// choice. `decision` comes from `policy::decide(Capability::Download, ..)`.
fn download_consent(decision: Decision, origin: Option<&Origin>) -> bool {
    let key = origin.map(Origin::display).unwrap_or_default();
    match decision {
        Decision::Allow => true,
        Decision::Deny => {
            util::log("download refused: no click and not from a page this app shows");
            false
        }
        Decision::Prompt => match settings::download_permission(&key) {
            settings::SitePermission::Allow => true,
            settings::SitePermission::Block => false,
            settings::SitePermission::Ask => {
                let who = if key.is_empty() { "A page".to_string() } else { key.clone() };
                let (button, _) = run_alert(
                    &format!("{who} wants to download a file"),
                    "It started without a recent click in the page. Downloads are saved to your Downloads folder.",
                    &["Download", "Always Allow for This Site", "Cancel"],
                    None,
                );
                if button == 1 && !key.is_empty() {
                    settings::set_download_permission(&key, settings::SitePermission::Allow);
                }
                button <= 1
            }
        },
    }
}

/// Is this web view the tab in front, or a popup window that has the
/// keyboard? Background tabs may not open windows.
fn is_foreground(web: &WKWebView, tab: Option<Uuid>) -> bool {
    match tab {
        Some(t) => POOL.with_borrow(|p| p.active == Some(t)),
        None => web.window().is_some_and(|w| w.isKeyWindow()),
    }
}

fn scheme_app_name(scheme: &str) -> &'static str {
    match scheme {
        "mailto" => "your email app",
        "tel" | "facetime" => "FaceTime",
        "sms" => "Messages",
        "zoommtg" => "Zoom",
        "msteams" => "Microsoft Teams",
        _ => "another app",
    }
}

/// Dialogs are titled with the origin really asking, so a frame cannot
/// borrow a Zoho name for a spoofed prompt.
fn origin_title(frame: &WKFrameInfo) -> String {
    let host = unsafe { frame.securityOrigin().host() }.to_string();
    if host.is_empty() { "Untrusted page".into() } else { host }
}

/// How close together page dialogs must come before the user is offered
/// to stop them.
const DIALOG_BURST: Duration = Duration::from_secs(10);

/// Note a page dialog at `now`; true when it is at least the second from
/// this page within 10 s, so the "Don't allow more dialogs" box is shown.
fn dialog_offers_suppression(recent: &mut Vec<Instant>, now: Instant) -> bool {
    recent.retain(|t| now.saturating_duration_since(*t) < DIALOG_BURST);
    recent.push(now);
    recent.len() >= 2
}

/// A profile's name for dialog titles ("Acme — crm.zoho.com").
pub fn profile_name(client: Uuid) -> String {
    app::store(|s| s.client(client).map(|c| if c.is_search() { "Google Search".to_string() } else { c.name.clone() })).unwrap_or_else(|| "ZClients".into())
}

/// Modal alert. Returns the index of the button pressed and, for prompts, the
/// text entered.
pub fn run_alert(title: &str, message: &str, buttons: &[&str], input: Option<String>) -> (usize, Option<String>) {
    // Unattended self-tests (test builds only): ZCLIENTS_ALERTS=accept picks
    // the first button, =decline the last, and the question is logged.
    if util::is_test_build()
        && let Ok(answer) = std::env::var("ZCLIENTS_ALERTS")
    {
        let i = if answer == "accept" { 0 } else { buttons.len().saturating_sub(1) };
        util::log(format!("SELFTEST alert {title:?} -> {:?}", buttons.get(i)));
        return (i, input);
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&util::ns(title));
    alert.setInformativeText(&util::ns(message));
    alert.setAlertStyle(NSAlertStyle::Informational);
    for b in buttons {
        alert.addButtonWithTitle(&util::ns(b));
    }
    let field = input.map(|text| {
        let f = NSTextField::textFieldWithString(&util::ns(&text), mtm);
        f.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0)));
        alert.setAccessoryView(Some(&f));
        f
    });
    let response = alert.runModal();
    let index = (response - objc2_app_kit::NSAlertFirstButtonReturn).max(0) as usize;
    (index, field.map(|f| f.stringValue().to_string()))
}

fn ns_str_url(url: Option<Retained<NSURL>>) -> Option<String> {
    url.and_then(|u| util::url_string(&u))
}

impl Coordinator {
    fn new(mtm: MainThreadMarker, client: Uuid, tab: Option<Uuid>, ws: Workspace, popup_trusted: bool, fresh: bool, depth: u8) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CoordIvars {
            client_id: client,
            tab_id: tab,
            workspace: ws,
            external_probe: Cell::new(false),
            external_user: Cell::new(false),
            popup_window: RefCell::new(None),
            popup_trusted,
            fresh: Cell::new(fresh),
            pending_pdf: RefCell::new(None),
            depth,
            dialogs: RefCell::new(vec![]),
            dialogs_blocked: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Trust comes from the frame's *live* security origin, never from the
    /// URL that created it: a subframe that started on Zoho and was moved
    /// elsewhere still reports the Zoho request URL.
    fn trusted(&self, frame: &WKFrameInfo) -> bool {
        let request =
            policy::Request { origin: frame_origin(frame), top: None, main_frame: unsafe { frame.isMainFrame() }, user_gesture: false, workspace: self.ivars().workspace };
        policy::decide(Capability::Display, &request) == Decision::Allow
    }

    fn trusted_opt(&self, frame: Option<Retained<WKFrameInfo>>) -> bool {
        frame.is_some_and(|f| self.trusted(&f))
    }

    /// A page's alert(), confirm() or prompt(): a sheet on the window
    /// showing the page (never an app-modal alert, so a page that loops
    /// alert() cannot lock every other client), titled "<client> — <host>",
    /// with its tab brought to the front first. From the second dialog in
    /// 10 s the user may stop this page's dialogs until it navigates.
    /// `done` gets the button index and any typed text, or None when the
    /// dialog was not shown.
    fn page_dialog(
        &self,
        web: &WKWebView,
        frame: &WKFrameInfo,
        message: String,
        buttons: &'static [&'static str],
        input: Option<String>,
        done: impl FnOnce(Option<(usize, Option<String>)>) + 'static,
    ) {
        let iv = self.ivars();
        if !self.trusted(frame) || iv.dialogs_blocked.get() {
            return done(None);
        }
        if let Some(tab) = iv.tab_id
            && POOL.with_borrow(|p| p.active) != Some(tab)
        {
            let client = iv.client_id;
            util::later(move || {
                app::show_tab(client, tab);
            });
        }
        let offer_stop = dialog_offers_suppression(&mut iv.dialogs.borrow_mut(), Instant::now());
        let title = format!("{} — {}", profile_name(iv.client_id), origin_title(frame));
        let window = web.window().or_else(app_window);
        if (util::is_test_build() && std::env::var_os("ZCLIENTS_ALERTS").is_some()) || window.is_none() {
            let (i, text) = run_alert(&title, &message, buttons, input);
            return done(Some((i, text)));
        }
        let mtm = self.mtm();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&util::ns(&title));
        alert.setInformativeText(&util::ns(&message));
        for b in buttons {
            alert.addButtonWithTitle(&util::ns(b));
        }
        let field = input.map(|text| {
            let f = NSTextField::textFieldWithString(&util::ns(&text), mtm);
            f.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0)));
            alert.setAccessoryView(Some(&f));
            f
        });
        if offer_stop {
            alert.setShowsSuppressionButton(true);
            if let Some(b) = alert.suppressionButton() {
                b.setTitle(&util::ns("Don't allow more dialogs from this page"));
            }
        }
        let me = objc2::Message::retain(self);
        let alert2 = alert.clone();
        let done = Cell::new(Some(done));
        let handler = RcBlock::new(move |response: isize| {
            let stop = alert2.suppressionButton().is_some_and(|b| b.state() == objc2_app_kit::NSControlStateValueOn) && alert2.showsSuppressionButton();
            if stop {
                me.ivars().dialogs_blocked.set(true);
            }
            let index = (response - objc2_app_kit::NSAlertFirstButtonReturn).max(0) as usize;
            if let Some(d) = done.take() {
                d(Some((index, field.as_ref().map(|f| f.stringValue().to_string()))));
            }
        });
        if let Some(w) = window {
            alert.beginSheetModalForWindow_completionHandler(&w, Some(&handler));
        }
    }

    /// A download started from a tab or popup that never showed anything:
    /// close it. WebKit keeps the download going without its web view.
    fn close_if_blank(&self) {
        let iv = self.ivars();
        if !iv.fresh.replace(false) {
            return;
        }
        if let Some(win) = iv.popup_window.borrow().clone() {
            util::later(move || win.close());
        } else if let Some(tab) = iv.tab_id {
            let client = iv.client_id;
            util::later(move || app::discard_tab(client, tab));
        }
    }

    fn state_changed(&self, web: &WKWebView) {
        if let Some(tab) = self.ivars().tab_id {
            app::web_state_changed(tab);
        }
        let _ = web;
    }

    fn action_policy(&self, web: &WKWebView, action: &WKNavigationAction) -> WKNavigationActionPolicy {
        let iv = self.ivars();
        let main = unsafe { action.targetFrame() }.map(|f| unsafe { f.isMainFrame() }).unwrap_or(true);
        // Subframes are free to load: they are isolated by origin, and every
        // privileged thing they can ask for (dialogs, file picker, downloads,
        // camera) is checked against their own origin. CRM widgets live here.
        if !main {
            return WKNavigationActionPolicy::Allow;
        }
        let Some(text) = ns_str_url(unsafe { action.request() }.URL()) else { return WKNavigationActionPolicy::Cancel };
        let Some(url) = policy::parse(&text) else { return WKNavigationActionPolicy::Cancel };
        let source_trusted = self.trusted_opt(unsafe { action.sourceFrame() });
        let user_link = unsafe { action.navigationType() } == WKNavigationType::LinkActivated;

        // Search has its own cookie store: Zoho/Meet results become normal
        // tabs of this client, signed in with its profile.
        if iv.workspace == Workspace::GoogleSearch && iv.tab_id.is_some() && policy::should_open_in_client_workspace(&url) {
            let client = iv.client_id;
            util::later(move || app::open_in_client_workspace(client, url));
            return WKNavigationActionPolicy::Cancel;
        }

        // ⌘-click on a link: a new tab in the background, the user stays on
        // this page (⇧⌘-click opens it in front).
        if user_link && source_trusted && iv.tab_id.is_some() && new_tab_click(web, action) {
            if policy::is_allowed_web(&url, iv.workspace) {
                let (client, ws, opener, front) = (iv.client_id, iv.workspace, iv.tab_id, wants_front(web, action));
                util::later(move || app::open_new_tab_from_page(client, ws, url, opener, !front));
                return WKNavigationActionPolicy::Cancel;
            }
            if policy::is_https(&url) {
                open_externally(&url);
                return WKNavigationActionPolicy::Cancel;
            }
        }

        let source = unsafe { action.sourceFrame() };
        let source_request = || {
            let mut r = request_for(web, source.as_deref(), iv.workspace);
            r.user_gesture = recent_gesture(DOWNLOAD_GESTURE_WINDOW);
            r
        };

        if unsafe { action.shouldPerformDownload() } && policy::is_safe_download(&url, iv.workspace) {
            let r = source_request();
            return if download_consent(policy::decide(Capability::Download, &r), r.origin.as_ref()) {
                WKNavigationActionPolicy::Download
            } else {
                WKNavigationActionPolicy::Cancel
            };
        }

        // mailto:, tel: and similar go to the Mac's own app: straight away
        // after a click in a page this app shows, otherwise only if the user
        // confirms. Never silently.
        if matches!(url.scheme(), "mailto" | "tel" | "facetime" | "sms" | "zoommtg" | "msteams") {
            let r = request_for(web, source.as_deref(), iv.workspace);
            let open = match policy::decide(Capability::ExternalLink, &r) {
                Decision::Allow => true,
                Decision::Prompt => {
                    let who = r.origin.as_ref().map(Origin::display).unwrap_or_else(|| "A page".into());
                    confirm(&format!("Open {}?", scheme_app_name(url.scheme())), &format!("{who} wants to open a {}: link.", url.scheme()), "Open")
                }
                Decision::Deny => false,
            };
            if open {
                open_externally(&url);
            }
            return WKNavigationActionPolicy::Cancel;
        }

        // PDF tabs (including old previews kept by the Swift build).
        if url.scheme() == "file" {
            // Only this tab's own PDF, nothing else on disk.
            let own = iv.tab_id.and_then(|t| app::store(|s| s.find_tab(t).and_then(|t| t.pdf_path.clone())));
            let ok = url.to_file_path().is_ok_and(|p| own.is_some_and(|o| std::path::Path::new(&o) == p));
            return if ok { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel };
        }

        if policy::is_allowed_main_frame(&url, iv.workspace) {
            iv.external_probe.set(false);
            return WKNavigationActionPolicy::Allow;
        }

        // A popup a Zoho page opened (OAuth for a widget or integration) may
        // travel to the provider's sign-in pages.
        if iv.popup_trusted && policy::is_https(&url) {
            return WKNavigationActionPolicy::Allow;
        }

        if policy::is_https(&url) && (source_trusted || iv.external_probe.get()) {
            if !iv.external_probe.get() {
                iv.external_user.set(user_link);
            }
            iv.external_probe.set(true);
            return WKNavigationActionPolicy::Allow;
        }
        WKNavigationActionPolicy::Cancel
    }

    fn response_policy(&self, web: &WKWebView, nav: &WKNavigationResponse) -> WKNavigationResponsePolicy {
        let iv = self.ivars();
        let response = unsafe { nav.response() };
        let main = unsafe { nav.isForMainFrame() };
        let mime = response.MIMEType().map(|m| m.to_string().to_lowercase()).unwrap_or_default();
        let disposition = response
            .downcast_ref::<NSHTTPURLResponse>()
            .and_then(|h| h.valueForHTTPHeaderField(&util::ns("Content-Disposition")))
            .map(|d| d.to_string().to_lowercase())
            .unwrap_or_default();
        let can_show = unsafe { nav.canShowMIMEType() };
        let is_file = disposition.starts_with("attachment") || !can_show;

        // Embedded frames (CRM widgets, maps, video, embedded Zoho views)
        // display whatever they load. Only real file responses become
        // downloads. The old build cancelled every off-list frame here, which
        // is why widgets on zappsusercontent.com never appeared. Zoho starts
        // many downloads (WorkDrive folders as .zip, exports) in a hidden
        // frame, often without an "attachment" header: anything WebKit
        // cannot display there is a file too, or it is silently dropped.
        if !main {
            if !is_file {
                return WKNavigationResponsePolicy::Allow;
            }
            let file_url = ns_str_url(response.URL()).and_then(|s| policy::parse(&s));
            let from_zoho = file_url.as_ref().is_some_and(|u| policy::is_frame_download_allowed(u, iv.workspace));
            if !from_zoho {
                return WKNavigationResponsePolicy::Cancel;
            }
            let r = self.download_request(web, file_url.as_ref());
            return if download_consent(policy::decide(Capability::Download, &r), r.origin.as_ref()) {
                WKNavigationResponsePolicy::Download
            } else {
                WKNavigationResponsePolicy::Cancel
            };
        }

        let url = ns_str_url(response.URL()).and_then(|s| policy::parse(&s));
        let allowed_page = url.as_ref().is_some_and(|u| policy::is_allowed_web(u, iv.workspace) || u.scheme() == "file" || u.scheme() == "blob" || u.as_str() == "about:blank");
        let probe_was_user = iv.external_user.replace(false);
        iv.external_probe.set(false);

        if is_file {
            let r = self.download_request(web, url.as_ref());
            return if download_consent(policy::decide(Capability::Download, &r), r.origin.as_ref()) {
                WKNavigationResponsePolicy::Download
            } else {
                WKNavigationResponsePolicy::Cancel
            };
        }
        // A PDF a page opened in a popup window (print previews, invoices,
        // blob: documents) opens as a tab instead, like any browser. It is
        // fetched through the popup so cookies, POST bodies and blob: URLs
        // all still work, then shown from the previews folder.
        if mime == "application/pdf" && iv.tab_id.is_none() {
            let source = url.as_ref().filter(|u| policy::is_https(u)).map(|u| u.to_string());
            *iv.pending_pdf.borrow_mut() = Some(source);
            return WKNavigationResponsePolicy::Download;
        }
        // PDFs display right in the tab (WebKit has a built-in viewer).
        if mime == "application/pdf" {
            return WKNavigationResponsePolicy::Allow;
        }
        if allowed_page || iv.popup_trusted {
            return WKNavigationResponsePolicy::Allow;
        }
        if mime.starts_with("image/") || mime.starts_with("video/") || mime.starts_with("audio/") {
            let r = self.download_request(web, url.as_ref());
            return if download_consent(policy::decide(Capability::Download, &r), r.origin.as_ref()) {
                WKNavigationResponsePolicy::Download
            } else {
                WKNavigationResponsePolicy::Cancel
            };
        }
        // An ordinary outside website the user clicked through to (a lead's
        // homepage, a help article): open it in the default browser.
        if probe_was_user && let Some(u) = url {
            open_externally(&u);
        }
        WKNavigationResponsePolicy::Cancel
    }

    fn open_window(&self, web: &WKWebView, cfg: &WKWebViewConfiguration, action: &WKNavigationAction, features: &WKWindowFeatures) -> Option<Retained<WKWebView>> {
        let iv = self.ivars();
        let mtm = self.mtm();
        let text = ns_str_url(unsafe { action.request() }.URL()).unwrap_or_default();
        let url = policy::parse(&text);
        let source_trusted = self.trusted_opt(unsafe { action.sourceFrame() });
        let user_link = unsafe { action.navigationType() } == WKNavigationType::LinkActivated;
        let sized = unsafe { features.width().is_some() || features.height().is_some() };
        let request = unsafe { action.request() };
        // A form posted to a new window (WorkDrive's "download these files
        // and folders as one .zip" works this way) only works in a web view
        // that receives the post itself; a new tab would reload it as GET.
        let is_get = request.HTTPMethod().is_none_or(|m| m.to_string().eq_ignore_ascii_case("GET"));

        let source = unsafe { action.sourceFrame() };
        let source_request = request_for(web, source.as_deref(), iv.workspace);
        // Background tabs and pages without a recent click do not get new
        // windows on their own; a page the app shows may ask.
        let popup_ok = |what: &str| -> bool {
            if !is_foreground(web, iv.tab_id) {
                util::log("window.open from a background page refused");
                return false;
            }
            match policy::decide(Capability::Popup, &source_request) {
                Decision::Allow => true,
                Decision::Prompt if source_trusted => {
                    let who = source_request.origin.as_ref().map(Origin::display).unwrap_or_default();
                    confirm(&format!("{who} wants to open {what}"), "It was not opened by a click in the page.", "Allow")
                }
                _ => false,
            }
        };

        if let Some(u) = &url {
            if unsafe { action.shouldPerformDownload() } && source_trusted && policy::is_safe_download(u, iv.workspace) {
                if !download_consent(policy::decide(Capability::Download, &source_request), source_request.origin.as_ref()) {
                    return None;
                }
                // The page's own request, so a POSTed download keeps its body.
                let block = RcBlock::new(|d: NonNull<WKDownload>| downloads::adopt(unsafe { d.as_ref() }));
                unsafe { web.startDownloadUsingRequest_completionHandler(&request, &block) };
                return None;
            }
            // ⌘-click, target=_blank and plain window.open(url) to a page
            // this app shows: a new tab, like any browser.
            if policy::is_allowed_web(u, iv.workspace) && !sized && is_get && iv.tab_id.is_some() {
                if !popup_ok("a new tab") {
                    return None;
                }
                // Zoho opens most links from its own click handlers with
                // window.open; ⌘ held for that click means a background tab.
                let background = new_tab_click(web, action) && !wants_front(web, action);
                let (client, ws, u, opener) = (iv.client_id, iv.workspace, u.clone(), iv.tab_id);
                util::later(move || app::open_new_tab_from_page(client, ws, u, opener, background));
                return None;
            }
            // A link to an outside website: the default browser.
            if policy::is_https(u) && user_link && !policy::is_allowed_web(u, iv.workspace) && recent_gesture(GESTURE_WINDOW) {
                open_externally(u);
                return None;
            }
        }

        // Everything else a trusted page opens is a real popup window that
        // keeps `window.opener`: widget dialogs, OAuth "Connect" flows, Zoho
        // Sign and payment pages. Returning no web view here (as the old
        // build did) breaks those, because the page can never talk to its
        // popup.
        // Only a frame the app shows (judged by its live origin, never by
        // having been opened by one) may open a popup, and chains stop at
        // MAX_POPUP_DEPTH. A popup that wandered off to another site keeps
        // no power to open more windows.
        if !source_trusted || iv.depth >= MAX_POPUP_DEPTH || !popup_ok("a pop-up window") {
            return None;
        }
        let w = unsafe { features.width() }.map(|n| n.doubleValue()).unwrap_or(980.0).clamp(320.0, 1600.0);
        let h = unsafe { features.height() }.map(|n| n.doubleValue()).unwrap_or(720.0).clamp(240.0, 1200.0);
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&util::ns("Loading…"));
        let coord = Coordinator::new(mtm, iv.client_id, None, iv.workspace, true, true, iv.depth + 1);
        let popup = new_zweb(mtm, rect, cfg, iv.client_id, iv.workspace);
        unsafe {
            popup.setNavigationDelegate(Some(ProtocolObject::from_ref(&*coord)));
            popup.setUIDelegate(Some(ProtocolObject::from_ref(&*coord)));
        }
        window.setContentView(Some(&popup));
        window.setDelegate(Some(ProtocolObject::from_ref(&*coord)));
        window.center();
        window.makeKeyAndOrderFront(None);
        *coord.ivars().popup_window.borrow_mut() = Some(window);
        POOL.with_borrow_mut(|p| p.popups.push((coord, popup.clone())));
        Some(Retained::into_super(popup))
    }

    /// The request behind a response that is a file: the file's own origin
    /// (a data: file belongs to the page showing it), and whether there was
    /// a recent click.
    fn download_request(&self, web: &WKWebView, file_url: Option<&Url>) -> policy::Request {
        let page = || ns_str_url(unsafe { web.URL() }).and_then(|u| policy::parse(&u)).and_then(|u| Origin::of(&u));
        let origin = match file_url {
            Some(u) if u.scheme() == "data" => page(),
            Some(u) => Origin::of(u),
            None => None,
        };
        policy::Request { origin: origin.clone(), top: origin, main_frame: true, user_gesture: recent_gesture(DOWNLOAD_GESTURE_WINDOW), workspace: self.ivars().workspace }
    }

    /// On Zoho's sign-in page, fill in the profile's saved login. The email
    /// step is advanced (no secret leaves the app for that), the password is
    /// filled, and the user presses Sign In: the app never submits a
    /// password by itself.
    fn maybe_sign_in(&self, web: &WKWebView) {
        let iv = self.ivars();
        // Exact sign-in origin, as the tab's live main frame, right now.
        let request = request_for(web, None, iv.workspace);
        if policy::decide(Capability::Autofill, &request) != Decision::Allow {
            return;
        }
        let Some(expected) = request.origin.as_ref().map(Origin::display) else { return };
        let Some(login) = keychain::read_login(iv.client_id) else { return };
        // Bounded, so a changed password cannot turn into a sign-in loop.
        let key = iv.client_id;
        let allowed = POOL.with_borrow_mut(|p| {
            let tries = p.sign_in_attempts.entry(key).or_default();
            tries.retain(|t| t.elapsed() < Duration::from_secs(600));
            if tries.len() >= 3 || tries.last().is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
                return false;
            }
            tries.push(Instant::now());
            true
        });
        if !allowed {
            return;
        }
        // The secrets go in as call arguments, never into script source, to
        // the main frame (inFrame: nil) of the app's isolated world. The
        // helper re-checks the page's origin before every step.
        let args = NSDictionary::<NSString, AnyObject>::from_retained_objects(
            &[&*util::ns("email"), &*util::ns("password"), &*util::ns("origin")],
            &[util::any(util::ns(&login.email)), util::any(util::ns(&login.password)), util::any(util::ns(&expected))],
        );
        drop(login);
        let world = unsafe { WKContentWorld::defaultClientWorld(self.mtm()) };
        let noop = RcBlock::new(|_r: *mut AnyObject, _e: *mut NSError| {});
        unsafe { web.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(&util::ns(scripts::FILL_CALL), Some(&args), None, &world, Some(&noop)) };
    }
}

fn open_externally(url: &Url) {
    if let Some(u) = util::ns_url(url.as_str()) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let _ = mtm;
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}

// ---------------------------------------------------------------------------
// The pool.
// ---------------------------------------------------------------------------

struct Live {
    view: Retained<ZWebView>,
    _coord: Retained<Coordinator>,
    observer: Retained<Observer>,
}

pub struct Pool {
    stores: HashMap<Uuid, Retained<WKWebsiteDataStore>>,
    live: HashMap<Uuid, Live>,
    lru: Vec<Uuid>,
    active: Option<Uuid>,
    popups: Vec<(Retained<Coordinator>, Retained<ZWebView>)>,
    /// When each awake tab was last in front (or woken).
    shown: HashMap<Uuid, Instant>,
    /// The latest macOS memory-pressure level.
    pressure: usize,
    /// Tabs a page just opened (target=_blank, window.open) whose web view
    /// has not been made yet.
    opened_by_page: HashSet<Uuid>,
    sign_in_attempts: HashMap<Uuid, Vec<Instant>>,
    /// When each tab's page last crashed (see `crash_allows_reload`).
    crashes: HashMap<Uuid, Vec<Instant>>,
    /// Hidden web views starting a download for a profile with no awake tab.
    helpers: Vec<Retained<ZWebView>>,
    bridge: Option<Retained<Bridge>>,
    max_live: usize,
}

thread_local! {
    static POOL: RefCell<Pool> = RefCell::new(Pool {
        stores: HashMap::new(),
        live: HashMap::new(),
        lru: vec![],
        active: None,
        popups: vec![],
        shown: HashMap::new(),
        pressure: PRESSURE_NORMAL,
        opened_by_page: HashSet::new(),
        sign_in_attempts: HashMap::new(),
        crashes: HashMap::new(),
        helpers: vec![],
        bridge: None,
        max_live: base_live_cap(),
    });
}

fn data_store(p: &mut Pool, client: Uuid, ws: Workspace) -> (Uuid, Retained<WKWebsiteDataStore>, bool) {
    let id = store_identifier(client, ws);
    let new = !p.stores.contains_key(&id);
    let store = p.stores.entry(id).or_insert_with(|| unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&util::ns_uuid(id), MainThreadMarker::new().unwrap()) }).clone();
    (id, store, new)
}

/// Start restoring the active profile's session while the window is being
/// built, so its first page does not wait for the cookie store.
pub fn prewarm_active_profile() {
    let Some((client, ws)) = app::store(|s| s.active_tab().map(|(c, t)| (c.id, t.effective_workspace()))) else { return };
    let (store_id, store, new_store) = POOL.with_borrow_mut(|p| data_store(p, client, ws));
    let retention = retention_of(client);
    if new_store {
        session::register(store_id, &store, retention);
    }
    session::hydrate(&store, store_id, retention, || {});
}

/// A profile's "Keep me signed in" choice.
pub fn retention_of(client: Uuid) -> session::Retention {
    app::store(|s| s.client(client).map(|c| c.retention())).unwrap_or_default()
}

/// The user changed a profile's retention: both of its stores follow.
pub fn retention_changed(client: Uuid) {
    let r = retention_of(client);
    for ws in [Workspace::Zoho, Workspace::GoogleSearch] {
        session::set_retention(store_identifier(client, ws), r);
    }
}

fn bridge(p: &mut Pool, mtm: MainThreadMarker) -> Retained<Bridge> {
    p.bridge
        .get_or_insert_with(|| {
            let this = Bridge::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
}

fn configuration(mtm: MainThreadMarker, store: &WKWebsiteDataStore, bridge: &Bridge) -> Retained<WKWebViewConfiguration> {
    unsafe {
        let cfg = WKWebViewConfiguration::new(mtm);
        cfg.setWebsiteDataStore(store);
        cfg.setApplicationNameForUserAgent(Some(&util::ns(application_name_for_user_agent())));
        let prefs = cfg.preferences();
        prefs.setJavaScriptCanOpenWindowsAutomatically(true);
        prefs.setElementFullscreenEnabled(true);
        // The call self-test uses WebKit's fake camera, microphone and
        // screens, so it never touches real devices or raises prompts.
        if util::is_test_build() && std::env::var_os("ZCLIENTS_MOCK_MEDIA").is_some() {
            let _: () = msg_send![&*prefs, _setMockCaptureDevicesEnabled: true];
            // ZCLIENTS_MOCK_PROMPT=1 keeps WebKit asking the UI delegate, so
            // the app's own permission decisions are exercised too.
            let ask = std::env::var_os("ZCLIENTS_MOCK_PROMPT").is_some();
            let _: () = msg_send![&*prefs, _setMockCaptureDevicesPromptEnabled: ask];
        }
        let ucc = cfg.userContentController();
        let world = WKContentWorld::defaultClientWorld(mtm);
        let login_helper = scripts::login_helper();
        for (src, main_only) in [(scripts::CONTEXT_HOOK, false), (scripts::EDITOR_HOOK, false), (login_helper.as_str(), true)] {
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &util::ns(src),
                WKUserScriptInjectionTime::AtDocumentEnd,
                main_only,
                &world,
            );
            ucc.addUserScript(&script);
        }
        let handler = ProtocolObject::from_ref(bridge);
        ucc.addScriptMessageHandler_contentWorld_name(handler, &world, &util::ns(scripts::CONTEXT_HANDLER));
        ucc.addScriptMessageHandler_contentWorld_name(handler, &world, &util::ns(scripts::EDITOR_HANDLER));
        ucc.addScriptMessageHandler_contentWorld_name(handler, &world, &util::ns(scripts::LOGIN_HANDLER));
        cfg
    }
}

/// The web view for `tab`, waking it (signed in, at its saved place) if it
/// is asleep.
pub fn view_for(client_id: Uuid, tab: &Tab) -> Retained<ZWebView> {
    if let Some(v) = POOL.with_borrow_mut(|p| {
        let v = p.live.get(&tab.id).map(|l| l.view.clone());
        if v.is_some() {
            touch(p, tab.id);
        }
        v
    }) {
        return v;
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let ws = tab.effective_workspace();
    let (view, store, store_id, new_store) = POOL.with_borrow_mut(|p| {
        let (store_id, store, new_store) = data_store(p, client_id, ws);
        let bridge = bridge(p, mtm);
        let cfg = configuration(mtm, &store, &bridge);
        let view = new_zweb(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)), &cfg, client_id, ws);
        let fresh = p.opened_by_page.remove(&tab.id);
        let coord = Coordinator::new(mtm, client_id, Some(tab.id), ws, false, fresh, 0);
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*coord)));
            view.setUIDelegate(Some(ProtocolObject::from_ref(&*coord)));
        }
        let observer: Retained<Observer> = {
            let this = Observer::alloc(mtm).set_ivars(tab.id);
            unsafe { msg_send![super(this), init] }
        };
        for key in KVO_KEYS {
            unsafe {
                view.addObserver_forKeyPath_options_context(&observer, &util::ns(key), NSKeyValueObservingOptions::New, std::ptr::null_mut());
            }
        }
        p.live.insert(tab.id, Live { view: view.clone(), _coord: coord, observer });
        touch(p, tab.id);
        (view, store, store_id, new_store)
    });
    let retention = retention_of(client_id);
    if new_store {
        session::register(store_id, &store, retention);
    }
    enforce_cap_keeping(Some(tab.id));
    app::changed(app::TABS);

    // Hold the first load until the profile's cookies are back, otherwise the
    // request races the restore and lands on a sign-in page. Kept on this
    // turn of the run loop: loading a turn later made switches feel quicker,
    // but PDF tabs then went without WebKit's viewer controls (download,
    // zoom) more often in testing.
    let tab = tab.clone();
    let v = view.clone();
    let asked = Instant::now();
    session::hydrate(&store, store_id, retention, move || {
        if util::is_test_build() {
            util::log(format!("PERF tab waited {:.1} ms for its session", asked.elapsed().as_secs_f64() * 1000.0));
        }
        let still_live = POOL.with_borrow(|p| p.live.get(&tab.id).is_some_and(|l| Retained::as_ptr(&l.view) == Retained::as_ptr(&v)));
        if still_live {
            load_tab(&v, &tab);
        }
    });
    view
}

fn load_tab(view: &ZWebView, tab: &Tab) {
    let ws = tab.effective_workspace();
    if let Some(path) = tab.pdf_path.as_deref() {
        // Only a valid PDF in its own private preview folder is shown, and
        // WebKit may read that folder alone.
        let file = std::path::Path::new(path);
        let root = util::pdf_previews_dir();
        match (crate::pdf::validate(file), crate::pdf::preview_dir_of(&root, file)) {
            (Ok(()), Some(dir)) => {
                let file = NSURL::fileURLWithPath(&util::ns(path));
                let dir = NSURL::fileURLWithPath(&util::ns(&dir.to_string_lossy()));
                unsafe { view.loadFileURL_allowingReadAccessToURL(&file, &dir) };
                return;
            }
            (result, _) => {
                util::log(format!("PDF tab not shown: {}", result.err().map(|e| e.to_string()).unwrap_or_else(|| "not in a preview folder".into())));
                return explain_blank(tab.id, LoadError::PdfMissing, tab.pdf_source_url.clone());
            }
        }
    }
    let Some(url) = tab.url.as_deref() else { return };
    // A PDF tab whose preview was cleaned up while the app was closed
    // keeps only its file:// address.
    if url.starts_with("file:") {
        return explain_blank(tab.id, LoadError::PdfMissing, tab.pdf_source_url.clone());
    }
    // Exact place: replay WebKit's own snapshot (history + scroll position)
    // when it was taken on the page the tab remembers.
    if let Some(state) = tabstate::load(tab.id, url) {
        let data = NSData::with_bytes(&state);
        unsafe { view.setInteractionState(Some(&data)) };
        return;
    }
    let allowed = policy::parse(url).is_some_and(|u| policy::is_allowed_web(&u, ws));
    match (allowed, util::ns_url(url)) {
        (true, Some(nsurl)) => {
            let req = NSURLRequest::requestWithURL(&nsurl);
            unsafe { view.loadRequest(&req) };
        }
        _ => explain_blank(tab.id, LoadError::NotAllowed, Some(url.to_string())),
    }
}

/// A tab that has nothing it may show says why, instead of staying blank.
/// Shown on the next turn: `load_tab` can run inside a content refresh.
fn explain_blank(tab: Uuid, kind: LoadError, url: Option<String>) {
    util::log(format!("tab left blank: {kind:?}"));
    util::later(move || ui::show_load_error(tab, kind, url));
}

fn touch(p: &mut Pool, id: Uuid) {
    p.lru.retain(|x| *x != id);
    p.lru.push(id);
    p.shown.insert(id, Instant::now());
}

/// A tab in a call (camera or microphone on, e.g. Google Meet) is never put
/// to sleep: that would hang up.
fn in_call(view: &WKWebView) -> bool {
    use objc2_web_kit::WKMediaCaptureState as S;
    unsafe { view.cameraCaptureState() != S::None || view.microphoneCaptureState() != S::None }
}

/// Camera / microphone in use by a tab (on or muted): `(camera, microphone)`.
pub fn call_state(tab: Uuid) -> Option<(bool, bool)> {
    use objc2_web_kit::WKMediaCaptureState as S;
    let v = live_view(tab)?;
    let (cam, mic) = unsafe { (v.cameraCaptureState() != S::None, v.microphoneCaptureState() != S::None) };
    (cam || mic).then_some((cam, mic))
}

thread_local! {
    static CALL_ACTIVITY: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
}

/// While any tab is in a call, keep the display awake and the app out of
/// App Nap (WebKit holds no sleep assertion for a WebRTC call, so a long
/// meeting spent listening could dim and sleep the screen). Released as
/// soon as the last call ends.
fn refresh_call_state() {
    let calling = POOL.with_borrow(|p| p.live.values().any(|l| in_call(&l.view)));
    CALL_ACTIVITY.with_borrow_mut(|a| match (calling, a.is_some()) {
        (true, false) => {
            use objc2_foundation::NSActivityOptions as O;
            let options = O::UserInitiated | O::IdleDisplaySleepDisabled | O::LatencyCritical;
            *a = Some(NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &util::ns("A call is in progress")));
        }
        (false, true) => {
            if let Some(activity) = a.take() {
                unsafe { NSProcessInfo::processInfo().endActivity(&activity) };
            }
        }
        _ => {}
    });
}

/// How long a background tab may go unseen before Memory Saver puts it to
/// sleep: two hours (Chrome's default), one while memory is tight.
fn idle_limit(pressure: usize) -> Duration {
    // The memory self-test shortens the wait (test builds only).
    if let Some(secs) = std::env::var("ZCLIENTS_IDLE_SECS").ok().filter(|_| util::is_test_build() && !cfg!(test)).and_then(|s| s.parse().ok()) {
        return Duration::from_secs(secs);
    }
    if pressure & (PRESSURE_WARN | PRESSURE_CRITICAL) != 0 { Duration::from_secs(3600) } else { Duration::from_secs(2 * 3600) }
}

/// Memory Saver: background tabs nobody has looked at for a long time go to
/// sleep and give their memory back (a Zoho app holds 0.5 GB or more); they
/// wake at their saved place when shown. Runs every few minutes.
pub fn sleep_idle_tabs() {
    let victims: Vec<Uuid> = POOL.with_borrow(|p| {
        let limit = idle_limit(p.pressure);
        p.live.iter().filter(|(id, l)| Some(**id) != p.active && !in_call(&l.view) && p.shown.get(*id).is_some_and(|t| t.elapsed() > limit)).map(|(id, _)| *id).collect()
    });
    for id in victims {
        sleep_tab(id);
    }
}

fn base_live_cap() -> usize {
    const GB: u64 = 1024 * 1024 * 1024;
    match NSProcessInfo::processInfo().physicalMemory() {
        m if m <= 10 * GB => 10,
        m if m <= 20 * GB => 16,
        _ => 24,
    }
}

// libdispatch's memory-pressure source (part of libSystem).
#[repr(C)]
struct DispatchSourceType {
    _private: [u8; 0],
}
unsafe extern "C" {
    static _dispatch_source_type_memorypressure: DispatchSourceType;
    static _dispatch_main_q: c_void;
    fn dispatch_source_create(kind: *const DispatchSourceType, handle: usize, mask: usize, queue: *const c_void) -> *mut c_void;
    fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
    fn dispatch_source_set_event_handler_f(source: *mut c_void, handler: extern "C" fn(*mut c_void));
    fn dispatch_source_get_data(source: *mut c_void) -> usize;
    fn dispatch_resume(object: *mut c_void);
    fn sysctlbyname(name: *const std::ffi::c_char, old: *mut c_void, old_len: *mut usize, new: *mut c_void, new_len: usize) -> i32;
}
const PRESSURE_NORMAL: usize = 1;
const PRESSURE_WARN: usize = 2;
const PRESSURE_CRITICAL: usize = 4;

/// How many tabs stay awake at a macOS memory-pressure level. A warning
/// can last for hours on an 8 GB Mac with another browser open, and tabs
/// slept for it reload when revisited, so it still keeps 8; only critical
/// pressure cuts down to 3.
fn cap_for_pressure(level: usize) -> usize {
    if level & PRESSURE_CRITICAL != 0 {
        3
    } else if level & PRESSURE_WARN != 0 {
        base_live_cap().min(8)
    } else {
        base_live_cap()
    }
}

fn set_pressure(level: usize) {
    let cap = cap_for_pressure(level);
    let changed = POOL.with_borrow_mut(|p| {
        p.pressure = level;
        std::mem::replace(&mut p.max_live, cap) != cap
    });
    if changed {
        util::log(format!("memory pressure {level}: keeping {cap} tabs awake"));
    }
    enforce_cap();
}

extern "C" fn memory_pressure(source: *mut c_void) {
    set_pressure(unsafe { dispatch_source_get_data(source) });
}

/// The level right now (the dispatch source only reports changes).
fn current_pressure() -> usize {
    let mut level: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    let name = c"kern.memorystatus_vm_pressure_level";
    let ok = unsafe { sysctlbyname(name.as_ptr(), (&mut level as *mut i32).cast(), &mut len, std::ptr::null_mut(), 0) } == 0;
    if ok { level.max(0) as usize } else { PRESSURE_NORMAL }
}

/// When macOS runs short of memory, put background tabs to sleep straight
/// away (and allow more again once it recovers), so keeping extra tabs awake
/// never pushes the Mac into swapping.
pub fn watch_memory_pressure() {
    set_pressure(current_pressure());
    unsafe {
        let source = dispatch_source_create(&_dispatch_source_type_memorypressure, 0, PRESSURE_NORMAL | PRESSURE_WARN | PRESSURE_CRITICAL, &_dispatch_main_q);
        if source.is_null() {
            return;
        }
        dispatch_set_context(source, source);
        dispatch_source_set_event_handler_f(source, memory_pressure);
        dispatch_resume(source);
        // Lives as long as the app.
    }
}

#[cfg(test)]
mod pressure_tests {
    use super::*;

    #[test]
    fn memory_saver_waits_two_hours_or_one_when_memory_is_tight() {
        assert_eq!(idle_limit(PRESSURE_NORMAL), Duration::from_secs(7200));
        assert_eq!(idle_limit(PRESSURE_WARN), Duration::from_secs(3600));
        assert_eq!(idle_limit(PRESSURE_CRITICAL), Duration::from_secs(3600));
    }

    #[test]
    fn a_tab_being_woken_is_never_the_one_put_to_sleep() {
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let (front, calling, new) = (ids[1], ids[2], ids[3]);
        // Critical pressure (cap 3), every older tab in front or in a call:
        // nothing else can go, and the new tab stays.
        let lru = [front, calling, new];
        assert_eq!(pick_victim(&lru, 3, 2, Some(front), Some(new), |id| id == calling), None);
        assert_eq!(pick_victim(&lru, 3, 2, Some(front), None, |id| id == calling), Some(new), "without the guard it was evicted");
        let lru = [ids[0], front, new];
        assert_eq!(pick_victim(&lru, 3, 2, Some(front), Some(new), |_| false), Some(ids[0]));
        assert_eq!(pick_victim(&lru, 3, 3, Some(front), Some(new), |_| false), None, "under the cap");
    }

    #[test]
    fn pressure_lowers_the_awake_tab_count_but_a_warning_keeps_eight() {
        let base = base_live_cap();
        assert!(base >= 10);
        assert_eq!(cap_for_pressure(PRESSURE_NORMAL), base);
        assert_eq!(cap_for_pressure(PRESSURE_WARN), 8);
        assert_eq!(cap_for_pressure(PRESSURE_CRITICAL), 3);
        assert_eq!(cap_for_pressure(PRESSURE_WARN | PRESSURE_CRITICAL), 3);
    }
}

/// Which awake tab goes to sleep when more than `max_live` are awake: the
/// least recently shown one that is not in front, not in a call and not
/// `keep` (the tab being woken right now, which must not be put straight
/// back to sleep when every older tab is in front or in a call).
fn pick_victim(lru: &[Uuid], live: usize, max_live: usize, active: Option<Uuid>, keep: Option<Uuid>, calling: impl Fn(Uuid) -> bool) -> Option<Uuid> {
    if live <= max_live {
        return None;
    }
    lru.iter().copied().find(|id| Some(*id) != active && Some(*id) != keep && !calling(*id))
}

fn enforce_cap() {
    enforce_cap_keeping(None);
}

fn enforce_cap_keeping(keep: Option<Uuid>) {
    loop {
        let victim = POOL.with_borrow(|p| pick_victim(&p.lru, p.live.len(), p.max_live, p.active, keep, |id| p.live.get(&id).is_some_and(|l| in_call(&l.view))));
        match victim {
            Some(id) => sleep_tab(id),
            None => break,
        }
    }
}

/// Put a tab to sleep: save its exact place, then release its web view.
pub fn sleep_tab(id: Uuid) {
    save_state(id);
    release(id);
}

/// Release a tab's web view without saving anything (it is closing, or about
/// to show a different page).
pub fn release(id: Uuid) {
    let live = POOL.with_borrow_mut(|p| {
        p.shown.remove(&id);
        p.lru.retain(|x| *x != id);
        if p.active == Some(id) {
            p.active = None;
        }
        p.live.remove(&id)
    });
    if let Some(l) = live {
        util::later(refresh_call_state);
        unsafe {
            for key in KVO_KEYS {
                l.view.removeObserver_forKeyPath(&l.observer, &util::ns(key));
            }
            l.view.stopLoading();
            l.view.setNavigationDelegate(None);
            l.view.setUIDelegate(None);
            // The script handlers stay: popups opened from this tab share the
            // same content controller, and the bridge holds no web view.
            l.view.removeFromSuperview();
        }
        app::changed(app::TABS);
    }
}

pub fn save_state(id: Uuid) {
    let Some(view) = POOL.with_borrow(|p| p.live.get(&id).map(|l| l.view.clone())) else { return };
    let Some(url) = ns_str_url(unsafe { view.URL() }) else { return };
    let Some(state) = (unsafe { view.interactionState() }) else { return };
    if let Ok(data) = state.downcast::<NSData>() {
        tabstate::save(id, &url, &data.to_vec());
    }
}

pub fn save_all_states() {
    let ids: Vec<Uuid> = POOL.with_borrow(|p| p.live.keys().copied().collect());
    for id in ids {
        save_state(id);
    }
}

/// A profile is being deleted: drop its web views, popups and open stores
/// so WebKit can remove its data (see `cleanup.rs`). Returns its store ids.
pub fn release_client(client: Uuid, tabs: &[Uuid]) -> Vec<Uuid> {
    for t in tabs {
        release(*t);
    }
    POOL.with_borrow_mut(|p| p.helpers.retain(|h| h.ivars().client_id != client));
    let popups: Vec<Retained<Coordinator>> = POOL.with_borrow(|p| p.popups.iter().filter(|(c, _)| c.ivars().client_id == client).map(|(c, _)| c.clone()).collect());
    for c in popups {
        if let Some(w) = c.ivars().popup_window.borrow().clone() {
            w.close();
        }
    }
    [Workspace::Zoho, Workspace::GoogleSearch]
        .into_iter()
        .map(|ws| {
            let id = store_identifier(client, ws);
            POOL.with_borrow_mut(|p| p.stores.remove(&id));
            id
        })
        .collect()
}

pub fn user_agent_suffix_in_use() -> &'static str {
    application_name_for_user_agent()
}

/// Clear a store's cookies, or all its website data, whether or not one of
/// its tabs is open. `done(true)` once WebKit reports it finished.
pub fn clear_website_data(store_id: Uuid, cookies_only: bool, done: impl Fn(bool) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else { return done(false) };
    let store = POOL.with_borrow(|p| p.stores.get(&store_id).cloned()).unwrap_or_else(|| unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&util::ns_uuid(store_id), mtm) });
    let types = unsafe {
        if cookies_only {
            objc2_foundation::NSSet::from_retained_slice(&[objc2::Message::retain(objc2_web_kit::WKWebsiteDataTypeCookies)])
        } else {
            WKWebsiteDataStore::allWebsiteDataTypes(mtm)
        }
    };
    let since = objc2_foundation::NSDate::distantPast();
    let block = RcBlock::new(move || done(true));
    unsafe { store.removeDataOfTypes_modifiedSince_completionHandler(&types, &since, &block) };
}

/// Remove a WebKit data store, then check WebKit no longer lists it.
pub fn remove_data_store(id: Uuid, done: impl FnOnce(Result<(), String>) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else { return done(Err("not on the main thread".into())) };
    let done = std::rc::Rc::new(RefCell::new(Some(done)));
    let block = RcBlock::new(move |err: *mut NSError| {
        if let Some(e) = unsafe { err.as_ref() } {
            let msg = e.localizedDescription().to_string();
            if let Some(d) = done.borrow_mut().take() {
                d(Err(msg));
            }
            return;
        }
        let done = done.clone();
        let verify = RcBlock::new(move |ids: NonNull<NSArray<objc2_foundation::NSUUID>>| {
            let target = util::ns_uuid(id).UUIDString().to_string();
            let still = unsafe { ids.as_ref() }.iter().any(|u| u.UUIDString().to_string().eq_ignore_ascii_case(&target));
            if let Some(d) = done.borrow_mut().take() {
                d(if still { Err("WebKit still lists the store".into()) } else { Ok(()) });
            }
        });
        unsafe { WKWebsiteDataStore::fetchAllDataStoreIdentifiers(&verify, MainThreadMarker::new().unwrap()) };
    });
    unsafe { WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(&util::ns_uuid(id), &block, mtm) };
}

/// A tab is gone for good (or closed): drop what the pool remembers about
/// it, so nothing stale is applied if the id is ever seen again.
pub fn forget_tab(tab: Uuid) {
    POOL.with_borrow_mut(|p| {
        p.opened_by_page.remove(&tab);
        p.crashes.remove(&tab);
    });
}

/// A profile was deleted: its sign-in attempt history goes too.
pub fn forget_client(client: Uuid) {
    POOL.with_borrow_mut(|p| p.sign_in_attempts.remove(&client));
}

/// The tab was opened by a page, so if all it ever loads is a download it
/// closes again.
pub fn mark_opened_by_page(tab: Uuid) {
    POOL.with_borrow_mut(|p| p.opened_by_page.insert(tab));
}

pub fn set_active(id: Option<Uuid>) {
    let previous = POOL.with_borrow_mut(|p| {
        let prev = p.active;
        p.active = id;
        // The tab being left was in view until now: Memory Saver counts
        // from here, not from when it was brought to the front.
        if let Some(prev) = prev.filter(|prev| Some(*prev) != id && p.live.contains_key(prev)) {
            p.shown.insert(prev, Instant::now());
        }
        if let Some(id) = id {
            touch(p, id);
        }
        prev
    });
    if previous != id
        && let Some(prev) = previous
    {
        save_state(prev);
    }
}

pub fn active_view() -> Option<Retained<ZWebView>> {
    POOL.with_borrow(|p| p.active.and_then(|id| p.live.get(&id).map(|l| l.view.clone())))
}

pub fn live_view(id: Uuid) -> Option<Retained<ZWebView>> {
    POOL.with_borrow(|p| p.live.get(&id).map(|l| l.view.clone()))
}

pub fn live_tabs() -> HashSet<Uuid> {
    POOL.with_borrow(|p| p.live.keys().copied().collect())
}

pub fn capture_sessions(wait: bool) {
    session::capture_all(wait);
}

pub struct PageState {
    pub url: String,
    pub title: String,
    pub can_back: bool,
    pub can_forward: bool,
    pub loading: bool,
}

pub fn page_state(id: Uuid) -> Option<PageState> {
    let v = live_view(id)?;
    unsafe {
        Some(PageState {
            url: ns_str_url(v.URL()).unwrap_or_default(),
            title: v.title().map(|t| t.to_string()).unwrap_or_default(),
            can_back: v.canGoBack(),
            can_forward: v.canGoForward(),
            loading: v.isLoading(),
        })
    }
}

pub fn load_url(url: &Url) {
    if let (Some(v), Some(u)) = (active_view(), util::ns_url(url.as_str())) {
        unsafe { v.loadRequest(&NSURLRequest::requestWithURL(&u)) };
    }
}

pub fn go_back() {
    if let Some(v) = active_view() {
        unsafe { v.goBack() };
    }
}

pub fn go_forward() {
    if let Some(v) = active_view() {
        unsafe { v.goForward() };
    }
}

pub fn reload(hard: bool) {
    if let Some(v) = active_view() {
        unsafe {
            if hard {
                v.reloadFromOrigin();
            } else {
                v.reload();
            }
        }
    }
}

pub fn stop() {
    if let Some(v) = active_view() {
        unsafe { v.stopLoading() };
    }
}

/// Zoom the active page; the level is remembered for this profile and site.
pub fn zoom(delta: f64) {
    let Some(v) = active_view() else { return };
    let z = unsafe { if delta == 0.0 { 1.0 } else { (v.pageZoom() + delta).clamp(0.5, 3.0) } };
    unsafe { v.setPageZoom(z) };
    let client = v.ivars().client_id;
    if let Some(host) = ns_str_url(unsafe { v.URL() }).and_then(|u| policy::parse(&u)).and_then(|u| u.host_str().map(str::to_string)) {
        settings::set_zoom(client, &host, z);
    }
}

/// Every awake tab using the camera or microphone: (client, tab, camera, mic).
pub fn tabs_in_calls() -> Vec<(Uuid, Uuid, bool, bool)> {
    use objc2_web_kit::WKMediaCaptureState as S;
    POOL.with_borrow(|p| {
        p.live
            .iter()
            .filter_map(|(id, l)| {
                let (cam, mic) = unsafe { (l.view.cameraCaptureState() != S::None, l.view.microphoneCaptureState() != S::None) };
                (cam || mic).then(|| (l.view.ivars().client_id, *id, cam, mic))
            })
            .collect()
    })
}

/// Turn a tab's camera and microphone off (public WebKit API; the page sees
/// its tracks end, as when Safari's indicator is used).
pub fn stop_media(tab: Uuid) {
    let Some(v) = live_view(tab) else { return };
    use objc2_web_kit::WKMediaCaptureState as S;
    unsafe {
        v.setCameraCaptureState_completionHandler(S::None, None);
        v.setMicrophoneCaptureState_completionHandler(S::None, None);
    }
    util::later(refresh_call_state);
}

/// Is the app holding the display awake for a call right now?
pub fn keeping_awake() -> bool {
    CALL_ACTIVITY.with_borrow(|a| a.is_some())
}

pub fn find(text: &str, backwards: bool, done: impl Fn(bool) + 'static) {
    let Some(v) = active_view() else { return };
    if text.is_empty() {
        return;
    }
    let mtm = MainThreadMarker::new().unwrap();
    unsafe {
        let cfg = WKFindConfiguration::new(mtm);
        cfg.setBackwards(backwards);
        cfg.setWraps(true);
        cfg.setCaseSensitive(false);
        let block = RcBlock::new(move |r: NonNull<WKFindResult>| done(r.as_ref().matchFound()));
        v.findString_withConfiguration_completionHandler(&util::ns(text), Some(&cfg), &block);
    }
}

/// Run JavaScript in the active tab's page and hand back a description of
/// the result (diagnostics only).
pub fn eval_active(js: &str, done: impl Fn(String) + 'static) {
    let Some(v) = active_view() else { return done("no web view".into()) };
    let block = RcBlock::new(move |r: *mut AnyObject, e: *mut NSError| {
        let text = unsafe { r.as_ref() }
            .and_then(|r| r.downcast_ref::<NSString>().map(|s| s.to_string()))
            .or_else(|| unsafe { e.as_ref() }.map(|e| format!("error: {}", e.localizedDescription())))
            .unwrap_or_else(|| "(no result)".into());
        done(text);
    });
    unsafe { v.evaluateJavaScript_completionHandler(&util::ns(js), Some(&block)) };
}

/// Run an async JavaScript function body (it may `await`) in a tab's page,
/// awake or not in front, and hand back its string result (self-tests).
pub fn eval_in_tab_async(tab: Uuid, js: &str, done: impl Fn(String) + 'static) {
    let Some(v) = live_view(tab) else { return done("no web view".into()) };
    let block = RcBlock::new(move |r: *mut AnyObject, e: *mut NSError| {
        let text = unsafe { r.as_ref() }
            .and_then(|r| r.downcast_ref::<NSString>().map(|s| s.to_string()))
            .or_else(|| unsafe { e.as_ref() }.map(|e| format!("error: {}", e.localizedDescription())))
            .unwrap_or_else(|| "(no result)".into());
        done(text);
    });
    let world = unsafe { WKContentWorld::pageWorld(MainThreadMarker::new().unwrap()) };
    unsafe { v.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(&util::ns(js), None, None, &world, Some(&block)) };
}

/// Self-tests: run an async function body in the app's own isolated world
/// of the active page.
pub fn eval_in_client_world(js: &str, done: impl Fn(String) + 'static) {
    let Some(v) = active_view() else { return done("no web view".into()) };
    let block = RcBlock::new(move |r: *mut AnyObject, e: *mut NSError| {
        let text = unsafe { r.as_ref() }
            .and_then(|r| r.downcast_ref::<NSString>().map(|s| s.to_string()))
            .or_else(|| unsafe { e.as_ref() }.map(|e| format!("error: {}", e.localizedDescription())))
            .unwrap_or_else(|| "(no result)".into());
        done(text);
    });
    let world = unsafe { WKContentWorld::defaultClientWorld(MainThreadMarker::new().unwrap()) };
    unsafe { v.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(&util::ns(js), None, None, &world, Some(&block)) };
}

/// WebKit is holding off system sleep for this tab (private, diagnostics).
pub fn has_sleep_disabler(tab: Uuid) -> bool {
    live_view(tab).is_some_and(|v| unsafe {
        let responds: bool = msg_send![&*v, respondsToSelector: sel!(_hasSleepDisabler)];
        responds && msg_send![&*v, _hasSleepDisabler]
    })
}

/// Print the active page with the standard print panel.
pub fn print() {
    let Some(v) = active_view() else { return };
    let info = objc2_app_kit::NSPrintInfo::sharedPrintInfo();
    let op = unsafe { v.printOperationWithPrintInfo(&info) };
    op.setShowsPrintPanel(true);
    op.setShowsProgressPanel(true);
    // WebKit prints blank pages unless the operation's view has a real size.
    if let Some(view) = op.view() {
        view.setFrame(v.bounds());
    }
    match v.window() {
        Some(w) => unsafe { op.runOperationModalForWindow_delegate_didRunSelector_contextInfo(&w, None, None, std::ptr::null_mut()) },
        None => {
            op.runOperation();
        }
    }
}

/// Retry a failed download in its own profile (a click in the app is the
/// user's gesture). Only https addresses, signed in as that profile: through
/// one of its awake tabs, or else a hidden web view on its data store. False
/// when it could not be started (not https, or the profile is gone).
pub fn download_url(client: Option<Uuid>, url: &str) -> bool {
    let Some(u) = policy::parse(url).filter(policy::is_https) else { return false };
    let Some(nsurl) = util::ns_url(u.as_str()) else { return false };
    let view = POOL.with_borrow(|p| {
        let active = p.active.and_then(|id| p.live.get(&id)).map(|l| l.view.clone());
        active
            .filter(|v| client.is_none_or(|c| v.ivars().client_id == c))
            .or_else(|| p.live.values().find(|l| client.is_none_or(|c| l.view.ivars().client_id == c)).map(|l| l.view.clone()))
    });
    if let Some(v) = view {
        start_download(&v, &nsurl);
        return true;
    }
    match client.filter(|c| app::store(|s| s.client(*c).is_some())) {
        Some(c) => {
            download_in_hidden_view(c, nsurl);
            true
        }
        None => false,
    }
}

fn start_download(view: &WKWebView, url: &NSURL) {
    let block = RcBlock::new(|d: NonNull<WKDownload>| downloads::adopt(unsafe { d.as_ref() }));
    unsafe { view.startDownloadUsingRequest_completionHandler(&NSURLRequest::requestWithURL(url), &block) };
}

/// No tab of the profile is awake: a web view that is never shown, on the
/// profile's own store with its session restored, starts the download and
/// is let go a little later (WebKit keeps a download going without its
/// view).
fn download_in_hidden_view(client: Uuid, url: Retained<NSURL>) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let ws = if app::store(|s| s.client(client).is_some_and(|c| c.is_search())) { Workspace::GoogleSearch } else { Workspace::Zoho };
    let (view, store, store_id, new_store) = POOL.with_borrow_mut(|p| {
        let (store_id, store, new_store) = data_store(p, client, ws);
        let bridge = bridge(p, mtm);
        let cfg = configuration(mtm, &store, &bridge);
        let view = new_zweb(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)), &cfg, client, ws);
        p.helpers.push(view.clone());
        (view, store, store_id, new_store)
    });
    let retention = retention_of(client);
    if new_store {
        session::register(store_id, &store, retention);
    }
    session::hydrate(&store, store_id, retention, move || {
        start_download(&view, &url);
        util::after(30.0, move || POOL.with_borrow_mut(|p| p.helpers.retain(|h| !std::ptr::eq(&**h, &*view))));
    });
}

/// Download whatever the active tab is showing (handy for PDFs).
pub fn download_current() {
    // A PDF tab already has the file: save a copy under its own name.
    if let Some((path, title)) = app::store(|s| s.active_tab().and_then(|(_, t)| Some((t.pdf_path.clone()?, t.title.clone())))) {
        downloads::save_copy(std::path::Path::new(&path), &downloads::pdf_name(&title));
        return;
    }
    let Some(v) = active_view() else { return };
    let Some(url) = ns_str_url(unsafe { v.URL() }) else { return };
    if let Some(nsurl) = util::ns_url(&url) {
        let block = RcBlock::new(|d: NonNull<WKDownload>| downloads::adopt(unsafe { d.as_ref() }));
        unsafe { v.startDownloadUsingRequest_completionHandler(&NSURLRequest::requestWithURL(&nsurl), &block) };
    }
}

fn app_window() -> Option<Retained<NSWindow>> {
    NSApplication::sharedApplication(MainThreadMarker::new()?).mainWindow()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_store_id_matches_the_swift_derivation() {
        let client = Uuid::parse_str("EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11").unwrap();
        assert_eq!(store_identifier(client, Workspace::Zoho), client);
        let s = store_identifier(client, Workspace::GoogleSearch);
        let b = s.as_bytes();
        assert_eq!(b[0], 0xEF ^ 0x47);
        assert_eq!(b[7], 0x7E ^ 0x21);
        assert_eq!(b[8..], client.as_bytes()[8..]);
    }

    #[test]
    fn pages_keep_their_editor_shortcuts_and_meet_keeps_cmd_d() {
        use objc2_app_kit::NSEventModifierFlags as M;
        assert!(page_owned_shortcut("d", M::Command, true, false), "Meet mute, whatever the setting");
        assert!(page_owned_shortcut("s", M::Command, false, true), "Writer save");
        assert!(page_owned_shortcut("K", M::Command, false, true), "insert link");
        assert!(page_owned_shortcut("s", M::Command | M::Shift, false, true));
        assert!(!page_owned_shortcut("s", M::Command, false, false), "setting off: the app's ⌘S");
        for (k, m) in [("t", M::Command), ("w", M::Command), ("l", M::Command), ("r", M::Command), ("v", M::Command), ("1", M::Command), ("s", M::Command | M::Control)] {
            assert!(!page_owned_shortcut(k, m, true, true), "browser shortcut {k} stays with the app");
        }
    }

    #[test]
    fn load_errors_worth_showing() {
        assert_eq!(load_error_kind("NSURLErrorDomain", -999), None, "cancelled");
        assert_eq!(load_error_kind("WebKitErrorDomain", 102), None, "became a download / policy");
        assert_eq!(load_error_kind("NSURLErrorDomain", -1009), Some(LoadError::Offline));
        assert_eq!(load_error_kind("NSURLErrorDomain", -1202), Some(LoadError::Certificate));
        assert_eq!(load_error_kind("NSURLErrorDomain", -1012), Some(LoadError::Authentication));
        assert_eq!(load_error_kind("NSURLErrorDomain", -1002), Some(LoadError::Unsupported));
        assert_eq!(load_error_kind("Other", 5), Some(LoadError::Other));
    }

    #[test]
    fn a_second_dialog_within_ten_seconds_offers_to_stop_them() {
        let t0 = Instant::now();
        let mut recent = vec![];
        assert!(!dialog_offers_suppression(&mut recent, t0));
        assert!(dialog_offers_suppression(&mut recent, t0 + Duration::from_secs(3)));
        let mut old = vec![t0];
        assert!(!dialog_offers_suppression(&mut old, t0 + Duration::from_secs(11)), "dialogs far apart are left alone");
    }

    #[test]
    fn a_page_that_keeps_crashing_stops_reloading() {
        let t0 = Instant::now();
        let mut recent = vec![];
        assert!(crash_allows_reload(&mut recent, t0), "first crash reloads");
        assert!(!crash_allows_reload(&mut recent, t0 + Duration::from_secs(20)), "second within a minute shows the error");
        let mut later = vec![t0];
        assert!(crash_allows_reload(&mut later, t0 + Duration::from_secs(61)), "an old crash is forgotten");
        assert_eq!(later.len(), 1);
    }

    #[test]
    fn user_agent_suffix_follows_installed_safari_and_never_invents_a_version() {
        let plist = "<key>CFBundleShortVersionString</key>\n\t<string>26.1</string>";
        assert_eq!(safari_version(plist).as_deref(), Some("26.1"));
        assert_eq!(user_agent_suffix(Some("26.1")), "Version/26.1 Safari/605.1.15");
        assert_eq!(user_agent_suffix(None), "Safari/605.1.15");
        assert_eq!(safari_version("<key>CFBundleShortVersionString</key><string>1.0; evil</string>"), None);
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("@@").is_none());
    }

    #[test]
    fn inline_names_use_mime_when_url_has_none() {
        let u = Url::parse("blob:https://crm.zoho.com/1234").unwrap();
        assert_eq!(inline_filename(&u, None, Some("application/pdf")), "Download.pdf");
        assert_eq!(inline_filename(&u, Some("Invoice 7"), Some("image/png")), "Invoice 7.png");
        let h = Url::parse("https://x.zohocdn.com/files/report.xlsx").unwrap();
        assert_eq!(inline_filename(&h, None, None), "report.xlsx");
    }
}
