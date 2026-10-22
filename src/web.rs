//! The WebKit layer: one `WKWebView` per awake tab, one persistent data store
//! per profile, and the delegates that decide what every page may do.
//!
//! Lightness comes from here: only a few tabs are kept awake (6 on Macs with
//! 10 GB of memory or less, 8 up to 20 GB, otherwise 10), and fewer whenever
//! macOS reports memory pressure. Going back to an awake tab is instant; a
//! tab that goes to sleep saves its exact place first and wakes up there,
//! still signed in.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSBackingStoreType, NSEvent, NSMenu, NSMenuItem, NSOpenPanel, NSResponder,
    NSTextField, NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueObservingOptions, NSNotification, NSNumber,
    NSObject, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKContentWorld, WKDownload, WKFindConfiguration, WKFindResult, WKFrameInfo, WKMediaCaptureType, WKNavigation,
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse,
    WKNavigationResponsePolicy, WKNavigationType, WKOpenPanelParameters, WKPermissionDecision, WKScriptMessage,
    WKScriptMessageHandler, WKSecurityOrigin, WKUIDelegate, WKUserContentController, WKUserScript,
    WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use url::Url;
use uuid::Uuid;

use crate::model::{Tab, Workspace};
use crate::{app, downloads, keychain, policy, scripts, session, tabstate, ui, util};

const KVO_KEYS: &[&str] = &["title", "URL", "canGoBack", "canGoForward", "loading"];

fn safari_user_agent() -> &'static str {
    static UA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    UA.get_or_init(build_user_agent)
}

fn build_user_agent() -> String {
    // Zoho and Google serve their full desktop experience to Safari. The
    // version tracks the Safari installed on this Mac.
    let version = std::fs::read_to_string("/Applications/Safari.app/Contents/Info.plist")
        .ok()
        .and_then(|plist| {
            let i = plist.find("<key>CFBundleShortVersionString</key>")?;
            let rest = &plist[i..];
            let a = rest.find("<string>")? + 8;
            let b = rest[a..].find("</string>")?;
            Some(rest[a..a + b].to_string())
        })
        .unwrap_or_else(|| "18.6".into());
    format!("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{version} Safari/605.1.15")
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
            menu_first(event) || unsafe { msg_send![super(self), performKeyEquivalent: event] }
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

fn menu_first(event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    if !flags.intersects(objc2_app_kit::NSEventModifierFlags::Command | objc2_app_kit::NSEventModifierFlags::Control) {
        return false;
    }
    let Some(mtm) = MainThreadMarker::new() else { return false };
    NSApplication::sharedApplication(mtm).mainMenu().is_some_and(|m| m.performKeyEquivalent(event))
}

impl ZWebView {
    fn decorate_menu(&self, menu: &NSMenu) {
        let target = self.ivars().context.borrow().clone();
        // A target older than a few seconds belongs to an earlier right-click.
        let Some(t) = target.filter(|t| t.at.is_some_and(|at| at.elapsed() < Duration::from_secs(5))) else { return };
        let ws = self.ivars().workspace;
        let ok = |u: &Option<String>| u.as_deref().and_then(policy::parse).is_some_and(|u| policy::is_safe_download(&u, ws));
        let mut items: Vec<(&str, String)> = vec![];
        if ok(&t.link) {
            items.push(("Download Linked File", t.link.clone().unwrap()));
        }
        if ok(&t.image) {
            items.push(("Save Image to Downloads", t.image.clone().unwrap()));
        }
        if ok(&t.media) {
            items.push(("Save Video to Downloads", t.media.clone().unwrap()));
        }
        if items.is_empty() {
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
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &util::ns(title), Some(sel!(zcDownloadContext:)), &util::ns(""))
            };
            unsafe { item.setTarget(Some(self)) };
            item.setTag(i as isize);
            menu.insertItem_atIndex(&item, i as isize);
            targets.push((url.clone(), t.name.clone()));
        }
        menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), items.len() as isize);
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
    });
    let web: Retained<ZWebView> = unsafe { msg_send![super(this), initWithFrame: frame, configuration: cfg] };
    unsafe {
        web.setCustomUserAgent(Some(&util::ns(safari_user_agent())));
        web.setAllowsBackForwardNavigationGestures(true);
        web.setAllowsMagnification(true);
        // Lets Safari's Web Inspector attach (Develop menu) — the quickest way
        // to see why a CRM widget misbehaves.
        web.setInspectable(true);
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
        &[util::any(util::ns(url)), util::any(NSNumber::new_i64(64 * 1024 * 1024))],
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
        match get("data").and_then(|b64| decode_base64(&b64)) {
            Some(bytes) => {
                let name = inline_filename(&parsed2, name.as_deref(), get("type").as_deref());
                downloads::save_bytes(bytes, &name, Some(client));
            }
            None => downloads::record_failure(&fallback),
        }
    });
    unsafe {
        web.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
            &util::ns(scripts::READ_INLINE_BODY),
            Some(&args),
            None,
            &web.client_world(),
            Some(&block),
        )
    };
}

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
            if name == scripts::CONTEXT_HANDLER {
                let t = ContextTarget { link: get("link"), image: get("image"), media: get("media"), name: get("name"), at: Some(Instant::now()) };
                *web.ivars().context.borrow_mut() = Some(t);
            } else if name == scripts::LOGIN_HANDLER {
                // Only Zoho's own sign-in page may hand us a password.
                let frame = unsafe { message.frameInfo() };
                let origin = unsafe { frame.securityOrigin() };
                let host = unsafe { origin.host() }.to_string();
                let url = policy::parse(&format!("{}://{host}/", unsafe { origin.protocol() }));
                if !unsafe { frame.isMainFrame() } || !url.is_some_and(|u| policy::is_zoho_sign_in(&u)) {
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
        fn observe(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&NSDictionary>, _ctx: *mut c_void) {
            // Never act inline: KVO can fire from inside our own WebKit calls.
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
        fn decide_response(&self, _web: &WKWebView, response: &WKNavigationResponse, handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>) {
            handler.call((self.response_policy(response),));
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, web: &WKWebView, _nav: Option<&WKNavigation>) {
            self.ivars().external_probe.set(false);
            if ns_str_url(unsafe { web.URL() }).is_some_and(|u| u != "about:blank") {
                self.ivars().fresh.set(false);
            }
            self.state_changed(web);
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web: &WKWebView, _nav: Option<&WKNavigation>) {
            self.state_changed(web);
            if let Some(win) = self.ivars().popup_window.borrow().as_ref() {
                if let Some(t) = unsafe { web.title() } {
                    win.setTitle(&t);
                }
            }
            self.maybe_sign_in(web);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, web: &WKWebView, _nav: Option<&WKNavigation>, _e: &NSError) {
            self.ivars().external_probe.set(false);
            self.state_changed(web);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, web: &WKWebView, _nav: Option<&WKNavigation>, _e: &NSError) {
            self.ivars().external_probe.set(false);
            self.state_changed(web);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_died(&self, web: &WKWebView) {
            let ok = unsafe { web.URL() }.and_then(|u| util::url_string(&u)).and_then(|s| policy::parse(&s))
                .is_some_and(|u| policy::is_allowed_web(&u, self.ivars().workspace));
            if ok {
                unsafe { web.reload() };
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
        fn js_alert(&self, _web: &WKWebView, message: &NSString, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn()>) {
            if self.trusted(frame) {
                run_alert(&origin_title(frame), &message.to_string(), &["OK"], None);
            }
            done.call(());
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn js_confirm(&self, _web: &WKWebView, message: &NSString, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn(Bool)>) {
            let ok = self.trusted(frame) && run_alert(&origin_title(frame), &message.to_string(), &["OK", "Cancel"], None).0 == 0;
            done.call((Bool::new(ok),));
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn js_prompt(&self, _web: &WKWebView, prompt: &NSString, default: Option<&NSString>, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn(*mut NSString)>) {
            if !self.trusted(frame) {
                return done.call((std::ptr::null_mut(),));
            }
            let (button, text) = run_alert(&origin_title(frame), &prompt.to_string(), &["OK", "Cancel"], Some(default.map(|d| d.to_string()).unwrap_or_default()));
            match (button, text) {
                (0, Some(t)) => {
                    let s = util::ns(&t);
                    done.call((Retained::as_ptr(&s) as *mut NSString,));
                }
                _ => done.call((std::ptr::null_mut(),)),
            }
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media_permission(&self, _web: &WKWebView, origin: &WKSecurityOrigin, _frame: &WKFrameInfo, _kind: WKMediaCaptureType, decide: &block2::DynBlock<dyn Fn(WKPermissionDecision)>) {
            let allowed = origin_allowed(origin, self.ivars().workspace);
            decide.call((if allowed { WKPermissionDecision::Prompt } else { WKPermissionDecision::Deny },));
        }

        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn open_panel(&self, web: &WKWebView, params: &WKOpenPanelParameters, frame: &WKFrameInfo, done: &block2::DynBlock<dyn Fn(*mut NSArray<NSURL>)>) {
            if !self.trusted(frame) {
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

fn origin_allowed(origin: &WKSecurityOrigin, ws: Workspace) -> bool {
    unsafe { policy::is_allowed_origin(&origin.protocol().to_string(), &origin.host().to_string(), origin.port() as i64, ws) }
}

/// Dialogs are titled with the origin really asking, so a frame cannot
/// borrow a Zoho name for a spoofed prompt.
fn origin_title(frame: &WKFrameInfo) -> String {
    let host = unsafe { frame.securityOrigin().host() }.to_string();
    if host.is_empty() { "Untrusted page".into() } else { host }
}

/// Modal alert. Returns the index of the button pressed and, for prompts, the
/// text entered.
pub fn run_alert(title: &str, message: &str, buttons: &[&str], input: Option<String>) -> (usize, Option<String>) {
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
    fn new(mtm: MainThreadMarker, client: Uuid, tab: Option<Uuid>, ws: Workspace, popup_trusted: bool, fresh: bool) -> Retained<Self> {
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
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Trust comes from the frame's *live* security origin, never from the
    /// URL that created it: a subframe that started on Zoho and was moved
    /// elsewhere still reports the Zoho request URL.
    fn trusted(&self, frame: &WKFrameInfo) -> bool {
        origin_allowed(&*unsafe { frame.securityOrigin() }, self.ivars().workspace)
    }

    fn trusted_opt(&self, frame: Option<Retained<WKFrameInfo>>) -> bool {
        frame.is_some_and(|f| self.trusted(&f))
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

    fn action_policy(&self, _web: &WKWebView, action: &WKNavigationAction) -> WKNavigationActionPolicy {
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

        if unsafe { action.shouldPerformDownload() } && source_trusted && policy::is_safe_download(&url, iv.workspace) {
            return WKNavigationActionPolicy::Download;
        }

        // mailto:, tel: and similar from a Zoho page go to the Mac's own app.
        if matches!(url.scheme(), "mailto" | "tel" | "facetime" | "sms" | "zoommtg" | "msteams") && source_trusted {
            open_externally(&url);
            return WKNavigationActionPolicy::Cancel;
        }

        // PDF tabs (including old previews kept by the Swift build).
        if url.scheme() == "file" {
            let dir = util::pdf_previews_dir();
            let ok = url.to_file_path().is_ok_and(|p| p.starts_with(&dir));
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

    fn response_policy(&self, nav: &WKNavigationResponse) -> WKNavigationResponsePolicy {
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
            return if is_file { WKNavigationResponsePolicy::Download } else { WKNavigationResponsePolicy::Allow };
        }

        let url = ns_str_url(response.URL()).and_then(|s| policy::parse(&s));
        let allowed_page = url.as_ref().is_some_and(|u| {
            policy::is_allowed_web(u, iv.workspace) || u.scheme() == "file" || u.scheme() == "blob" || u.as_str() == "about:blank"
        });
        let probe_was_user = iv.external_user.replace(false);
        iv.external_probe.set(false);

        if is_file {
            return WKNavigationResponsePolicy::Download;
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
            return WKNavigationResponsePolicy::Download;
        }
        // An ordinary outside website the user clicked through to (a lead's
        // homepage, a help article): open it in the default browser.
        if probe_was_user {
            if let Some(u) = url {
                open_externally(&u);
            }
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

        if let Some(u) = &url {
            if unsafe { action.shouldPerformDownload() } && source_trusted && policy::is_safe_download(u, iv.workspace) {
                // The page's own request, so a POSTed download keeps its body.
                let block = RcBlock::new(|d: NonNull<WKDownload>| downloads::adopt(unsafe { d.as_ref() }));
                unsafe { web.startDownloadUsingRequest_completionHandler(&request, &block) };
                return None;
            }
            // ⌘-click, target=_blank and plain window.open(url) to a page
            // this app shows: a new tab, like any browser.
            if policy::is_allowed_web(u, iv.workspace) && !sized && is_get && iv.tab_id.is_some() {
                let (client, ws, u) = (iv.client_id, iv.workspace, u.clone());
                util::later(move || app::open_new_tab_from_page(client, ws, u));
                return None;
            }
            // A link to an outside website: the default browser.
            if policy::is_https(u) && user_link && !policy::is_allowed_web(u, iv.workspace) {
                open_externally(u);
                return None;
            }
        }

        // Everything else a trusted page opens is a real popup window that
        // keeps `window.opener`: widget dialogs, OAuth "Connect" flows, Zoho
        // Sign and payment pages. Returning no web view here (as the old
        // build did) breaks those, because the page can never talk to its
        // popup.
        if !source_trusted && !iv.popup_trusted {
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
        let coord = Coordinator::new(mtm, iv.client_id, None, iv.workspace, true, true);
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

    /// On Zoho's sign-in page, sign the profile back in with its saved login.
    fn maybe_sign_in(&self, web: &WKWebView) {
        let iv = self.ivars();
        if iv.workspace != Workspace::Zoho {
            return;
        }
        let Some(url) = ns_str_url(unsafe { web.URL() }).and_then(|s| policy::parse(&s)) else { return };
        if !policy::is_zoho_sign_in(&url) {
            return;
        }
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
        let js = format!("window.__zcFill && window.__zcFill({}, {})", scripts::js_string(&login.email), scripts::js_string(&login.password));
        let world = unsafe { WKContentWorld::defaultClientWorld(self.mtm()) };
        let noop = RcBlock::new(|_r: *mut AnyObject, _e: *mut NSError| {});
        unsafe { web.evaluateJavaScript_inFrame_inContentWorld_completionHandler(&util::ns(&js), None, &world, Some(&noop)) };
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
    /// Tabs a page just opened (target=_blank, window.open) whose web view
    /// has not been made yet.
    opened_by_page: HashSet<Uuid>,
    sign_in_attempts: HashMap<Uuid, Vec<Instant>>,
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
        opened_by_page: HashSet::new(),
        sign_in_attempts: HashMap::new(),
        bridge: None,
        max_live: base_live_cap(),
    });
}

fn data_store(p: &mut Pool, client: Uuid, ws: Workspace) -> (Uuid, Retained<WKWebsiteDataStore>) {
    let id = store_identifier(client, ws);
    let store = p
        .stores
        .entry(id)
        .or_insert_with(|| unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&util::ns_uuid(id), MainThreadMarker::new().unwrap()) })
        .clone();
    (id, store)
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
        let prefs = cfg.preferences();
        prefs.setJavaScriptCanOpenWindowsAutomatically(true);
        prefs.setElementFullscreenEnabled(true);
        let ucc = cfg.userContentController();
        let world = WKContentWorld::defaultClientWorld(mtm);
        for (src, main_only) in [(scripts::CONTEXT_HOOK, false), (scripts::LOGIN_HELPER, true)] {
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
    let (view, store, store_id) = POOL.with_borrow_mut(|p| {
        let (store_id, store) = data_store(p, client_id, ws);
        let bridge = bridge(p, mtm);
        let cfg = configuration(mtm, &store, &bridge);
        let view = new_zweb(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)), &cfg, client_id, ws);
        let fresh = p.opened_by_page.remove(&tab.id);
        let coord = Coordinator::new(mtm, client_id, Some(tab.id), ws, false, fresh);
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
        (view, store, store_id)
    });
    enforce_cap();
    app::changed(app::TABS);

    // Hold the first load until the profile's cookies are back, otherwise the
    // request races the restore and lands on a sign-in page. Do not push the
    // load to a later run-loop turn to make switches feel quicker: WebKit then
    // never adds its PDF viewer controls (download, zoom) to the view.
    let tab = tab.clone();
    let v = view.clone();
    session::hydrate(&store, store_id, move || {
        let still_live = POOL.with_borrow(|p| p.live.get(&tab.id).is_some_and(|l| Retained::as_ptr(&l.view) == Retained::as_ptr(&v)));
        if still_live {
            load_tab(&v, &tab);
        }
    });
    view
}

fn load_tab(view: &ZWebView, tab: &Tab) {
    let ws = tab.effective_workspace();
    if let Some(path) = tab.pdf_path.as_deref().filter(|p| std::path::Path::new(p).exists()) {
        let file = NSURL::fileURLWithPath(&util::ns(path));
        let dir = NSURL::fileURLWithPath(&util::ns(&util::pdf_previews_dir().to_string_lossy()));
        unsafe { view.loadFileURL_allowingReadAccessToURL(&file, &dir) };
        return;
    }
    let Some(url) = tab.url.as_deref() else { return };
    // Exact place: replay WebKit's own snapshot (history + scroll position)
    // when it was taken on the page the tab remembers.
    if let Some(state) = tabstate::load(tab.id, url) {
        let data = NSData::with_bytes(&state);
        unsafe { view.setInteractionState(Some(&data)) };
        return;
    }
    let allowed = policy::parse(url).is_some_and(|u| policy::is_allowed_web(&u, ws));
    if let (true, Some(nsurl)) = (allowed, util::ns_url(url)) {
        let req = NSURLRequest::requestWithURL(&nsurl);
        unsafe { view.loadRequest(&req) };
    }
}

fn touch(p: &mut Pool, id: Uuid) {
    p.lru.retain(|x| *x != id);
    p.lru.push(id);
}

fn base_live_cap() -> usize {
    const GB: u64 = 1024 * 1024 * 1024;
    match NSProcessInfo::processInfo().physicalMemory() {
        m if m <= 10 * GB => 6,
        m if m <= 20 * GB => 8,
        _ => 10,
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
/// can last for hours on an 8 GB Mac with a browser open, so it keeps the
/// old 4 rather than punishing every switch; critical keeps only 2.
fn cap_for_pressure(level: usize) -> usize {
    if level & PRESSURE_CRITICAL != 0 {
        2
    } else if level & PRESSURE_WARN != 0 {
        base_live_cap().min(4)
    } else {
        base_live_cap()
    }
}

fn set_pressure(level: usize) {
    let cap = cap_for_pressure(level);
    let changed = POOL.with_borrow_mut(|p| std::mem::replace(&mut p.max_live, cap) != cap);
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
        let source = dispatch_source_create(
            &_dispatch_source_type_memorypressure,
            0,
            PRESSURE_NORMAL | PRESSURE_WARN | PRESSURE_CRITICAL,
            &_dispatch_main_q,
        );
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
    fn pressure_lowers_the_awake_tab_count_but_a_warning_keeps_four() {
        let base = base_live_cap();
        assert!(base >= 6);
        assert_eq!(cap_for_pressure(PRESSURE_NORMAL), base);
        assert_eq!(cap_for_pressure(PRESSURE_WARN), 4);
        assert_eq!(cap_for_pressure(PRESSURE_CRITICAL), 2);
        assert_eq!(cap_for_pressure(PRESSURE_WARN | PRESSURE_CRITICAL), 2);
    }
}

fn enforce_cap() {
    loop {
        let victim = POOL.with_borrow(|p| {
            if p.live.len() <= p.max_live {
                return None;
            }
            p.lru.iter().copied().find(|id| Some(*id) != p.active)
        });
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
        p.lru.retain(|x| *x != id);
        if p.active == Some(id) {
            p.active = None;
        }
        p.live.remove(&id)
    });
    if let Some(l) = live {
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

/// Forget a profile entirely: web views, both data stores, cookie backup.
pub fn delete_client(client: Uuid, tabs: &[Uuid]) {
    for t in tabs {
        release(*t);
        tabstate::delete(*t);
    }
    let popups: Vec<Retained<Coordinator>> = POOL.with_borrow(|p| {
        p.popups.iter().filter(|(c, _)| c.ivars().client_id == client).map(|(c, _)| c.clone()).collect()
    });
    for c in popups {
        if let Some(w) = c.ivars().popup_window.borrow().clone() {
            w.close();
        }
    }
    for ws in [Workspace::Zoho, Workspace::GoogleSearch] {
        let id = store_identifier(client, ws);
        POOL.with_borrow_mut(|p| p.stores.remove(&id));
        session::forget(id);
        // The store can only be removed once no web view uses it.
        util::after(0.5, move || {
            let block = RcBlock::new(move |err: *mut NSError| {
                if let Some(e) = unsafe { err.as_ref() } {
                    util::log(format!("could not remove data store {id}: {}", e.localizedDescription()));
                }
            });
            unsafe { WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(&util::ns_uuid(id), &block, MainThreadMarker::new().unwrap()) };
        });
    }
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
        if let Some(id) = id {
            touch(p, id);
        }
        prev
    });
    if previous != id {
        if let Some(prev) = previous {
            save_state(prev);
        }
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

fn open_stores() -> Vec<(Uuid, Retained<WKWebsiteDataStore>)> {
    POOL.with_borrow(|p| p.stores.iter().map(|(id, s)| (*id, s.clone())).collect())
}

pub fn capture_sessions(wait: bool) {
    session::capture_all(open_stores(), wait);
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

pub fn zoom(delta: f64) {
    if let Some(v) = active_view() {
        unsafe {
            let z = if delta == 0.0 { 1.0 } else { (v.pageZoom() + delta).clamp(0.5, 3.0) };
            v.setPageZoom(z);
        }
    }
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

#[allow(dead_code)]
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
