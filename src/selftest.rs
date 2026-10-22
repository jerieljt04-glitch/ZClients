//! Keyboard and download self-tests for "ZClients Test" builds only
//! (`ZCLIENTS_SELFTEST=1`). Sends real key events through `NSApp.sendEvent`
//! — the same path a key press takes — and logs what each one did.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSPoint;

use crate::{app, downloads, ui, util, web};

fn press(chars: &str, code: u16, mods: NSEventModifierFlags) {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let window = app.keyWindow().or_else(|| app.mainWindow()).map(|w| w.windowNumber()).unwrap_or(0);
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind, NSPoint::new(0.0, 0.0), mods, 0.0, window, None, &util::ns(chars), &util::ns(chars), false, code,
        );
        if let Some(e) = event {
            app.sendEvent(&e);
        }
    }
}

fn copy_from_outside(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&util::ns(text), unsafe { NSPasteboardTypeString });
}

fn tab_count() -> usize {
    app::store(|s| s.active_client().map(|c| c.tabs.len()).unwrap_or(0))
}

/// `ZCLIENTS_SELFTEST=prep`: only focus a text box in the page and report
/// what ends up in it, so real keystrokes can be sent from outside.
fn log_keys() {
    let block = block2::RcBlock::new(|e: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let ev = unsafe { e.as_ref() };
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let fr = app.keyWindow().and_then(|w| w.firstResponder()).map(|r| r.class().name().to_string_lossy().into_owned());
        util::log(format!(
            "SELFTEST key: {:?} mods={:?} active={} keyWindow={} firstResponder={fr:?}",
            ev.charactersIgnoringModifiers().map(|c| c.to_string()), ev.modifierFlags(), app.isActive(), app.keyWindow().is_some()
        ));
        e.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(objc2_app_kit::NSEventMask::KeyDown, &block) };
    std::mem::forget(monitor);
}

fn prep() {
    log_keys();
    util::after(5.0, || {
        web::eval_active("(() => { const f = document.getElementById('zc-test') || Object.assign(document.body.appendChild(document.createElement('textarea')), {id: 'zc-test'}); f.style.cssText='position:fixed;top:10px;left:10px;width:400px;height:60px;z-index:99999'; f.value=''; f.focus(); return 'focused ' + (document.activeElement === f); })()", |r| util::log(format!("SELFTEST prep: {r}")));
        if let Some(v) = web::active_view() {
            ui::focus_view(&v);
        }
    });
    util::after(18.0, || {
        web::eval_active("(() => { const f = document.getElementById('zc-test'); return f ? f.value : 'no field'; })()", |r| util::log(format!("SELFTEST web field after real keys = {r:?}")));
        util::log(format!("SELFTEST sidebar visible = {}", ui::sidebar_visible()));
    });
    util::after(19.0, || NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None));
}

fn report(step: &str) {
    let tabs = app::store(|s| {
        s.active_client().map(|c| c.tabs.iter().map(|t| format!("{:?}{}", t.display_title(), if t.pdf_path.is_some() { " [pdf]" } else { "" })).collect::<Vec<_>>())
    });
    let files = downloads::with_items(|v| v.iter().map(|i| format!("{} {:?}", i.name, i.state)).collect::<Vec<_>>());
    let mtm = MainThreadMarker::new().unwrap();
    let windows = NSApplication::sharedApplication(mtm).windows().iter().filter(|w| w.isVisible()).count();
    util::log(format!("SELFTEST {step}: tabs={tabs:?} downloads={files:?} visible windows={windows}"));
}

fn find_hud(v: &objc2_app_kit::NSView) -> Option<objc2::rc::Retained<objc2_app_kit::NSView>> {
    for s in v.subviews().iter() {
        if s.class().name().to_string_lossy().contains("PDFHUD") {
            return Some(s);
        }
        if let Some(f) = find_hud(&s) {
            return Some(f);
        }
    }
    None
}

fn press_pdf_download(view: &objc2_app_kit::NSView) -> bool {
    let Some(hud) = find_hud(view) else {
        fn names(v: &objc2_app_kit::NSView, depth: usize, out: &mut Vec<String>) {
            for s in v.subviews().iter() {
                out.push(format!("{}{}", " ".repeat(depth), s.class().name().to_string_lossy()));
                if depth < 3 { names(&s, depth + 1, out); }
            }
        }
        let mut out = vec![];
        names(view, 0, &mut out);
        let url: Option<objc2::rc::Retained<objc2_foundation::NSURL>> = unsafe { objc2::msg_send![view, URL] };
        util::log(format!("SELFTEST no PDF HUD; url={:?} subviews={out:?}", url.and_then(|u| util::url_string(&u))));
        return false;
    };
    let control = util::ns("arrow.down.circle");
    unsafe {
        let _: () = objc2::msg_send![&*hud, _performActionForControl: &*control];
    }
    true
}

/// `ZCLIENTS_SELFTEST=downloads`: a page opens a PDF in a popup (should
/// become a tab), downloads a zip from a hidden frame, and opens a zip in a
/// popup (should download and close the popup).
fn downloads_test() {
    util::after(3.0, || {
        app::add_client("Download Test", "#3366cc", None, None);
        util::after(0.5, || app::address_entered("https://www.zoho.com/"));
    });
    util::after(14.0, || {
        report("loaded");
        web::eval_active(r#"(() => {
            const pdf = '%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 144]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF';
            window.open(URL.createObjectURL(new Blob([pdf], {type: 'application/pdf'})), '_blank', 'width=600,height=500');
            return 'opened pdf popup';
        })()"#, |r| util::log(format!("SELFTEST pdf popup: {r}")));
    });
    util::after(20.0, || {
        report("after pdf popup");
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        util::log(format!(
            "SELFTEST fresh PDF tab has viewer controls: {} (app active={}, window key={})",
            web::active_view().is_some_and(|v| find_hud(&v).is_some()),
            app.isActive(),
            app.keyWindow().is_some()
        ));
        web::eval_active("(() => 'active page: ' + location.protocol)()", |r| util::log(format!("SELFTEST {r}")));
        web::download_current();
    });
    util::after(23.0, || {
        report("after saving the pdf tab");
        app::cycle_tab(-1);
    });
    util::after(25.0, || {
        web::eval_active(r#"(() => {
            const f = document.createElement('iframe');
            f.style.display = 'none';
            f.src = URL.createObjectURL(new Blob(['PK\x05\x06' + '\0'.repeat(18)], {type: 'application/zip'}));
            document.body.appendChild(f);
            return 'hidden frame';
        })()"#, |r| util::log(format!("SELFTEST zip frame: {r}")));
    });
    util::after(29.0, || {
        report("after hidden frame zip");
        web::eval_active(r#"(() => {
            window.open(URL.createObjectURL(new Blob(['PK\x05\x06' + '\0'.repeat(18)], {type: 'application/zip'})), '_blank', 'width=500,height=400');
            return 'zip popup';
        })()"#, |r| util::log(format!("SELFTEST zip popup: {r}")));
    });
    util::after(34.0, || {
        report("after zip popup");
        app::cycle_tab(-1);
        // Like a CRM attachment: the page downloads a PDF as a file.
        web::eval_active(r#"(() => {
            const pdf = '%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 144]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF';
            const a = document.createElement('a');
            a.href = URL.createObjectURL(new Blob([pdf], {type: 'application/pdf'}));
            a.download = 'Quote-1001.pdf';
            document.body.appendChild(a);
            a.click();
            return 'attachment clicked';
        })()"#, |r| util::log(format!("SELFTEST pdf attachment: {r}")));
    });
    util::after(39.0, || {
        report("after pdf attachment");
        app::cycle_tab(-1);
        util::after(0.5, || {
            let shown = downloads::show_pdf(0);
            util::after(0.5, move || report(&format!("after picking it from the list again (shown={shown})")));
        });
    });
    util::after(42.0, || {
        // Press the PDF viewer's own download button, through WebKit.
        let pressed = web::active_view().map(|v| press_pdf_download(&v)).unwrap_or(false);
        util::log(format!("SELFTEST pressed PDF viewer download button: {pressed}"));
    });
    util::after(45.0, || {
        report("after the PDF viewer's download button");
        util::log("SELFTEST done");
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
    });
}

fn ms(t: std::time::Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn active_loading() -> bool {
    app::store(|s| s.active_tab().map(|(_, t)| t.id)).and_then(web::page_state).is_none_or(|p| p.loading || p.url.is_empty())
}

/// Calls `done(ms)` once the active tab has finished loading (or after 15 s).
fn when_loaded(started: std::time::Instant, done: impl Fn(f64) + 'static) {
    let done = std::rc::Rc::new(done);
    fn poll(started: std::time::Instant, done: std::rc::Rc<dyn Fn(f64)>) {
        if !active_loading() || started.elapsed().as_secs() >= 15 {
            done(ms(started));
            return;
        }
        util::after(0.01, move || poll(started, done));
    }
    util::after(0.01, move || poll(started, done));
}

const PERF_PAGES: &[&str] = &[
    "https://www.zoho.com/", "https://www.zoho.com/crm/", "https://www.zoho.com/books/", "https://www.zoho.com/mail/",
    "https://www.zoho.com/workdrive/", "https://www.zoho.com/desk/", "https://www.zoho.com/projects/", "https://www.zoho.com/people/",
    "https://www.zoho.com/sign/", "https://www.zoho.com/analytics/", "https://www.zoho.com/creator/", "https://www.zoho.com/invoice/",
];

/// `ZCLIENTS_SELFTEST=perf`: launch time, then main-thread cost of the tab
/// strip, toolbar, tab switches and state saving with 12 Zoho tabs open.
fn perf_test() {
    util::log(format!("PERF window built at {:.0} ms", util::ms_since_start()));
    util::after(0.5, || {
        app::add_client("Perf", "#884422", None, None);
        let client = app::store(|s| s.active_client_id).unwrap();
        let t0 = std::time::Instant::now();
        app::address_entered(PERF_PAGES[0]);
        when_loaded(t0, |m| util::log(format!("PERF first page loaded in {m:.0} ms ({:.0} ms after launch)", util::ms_since_start())));
        app::mutate(app::ALL, |s| {
            for u in PERF_PAGES[1..].iter().rev() {
                s.add_tab(client, Some(u.to_string()), None, None, false);
            }
        });
    });
    util::after(12.0, || {
        let t = std::time::Instant::now();
        for _ in 0..200 {
            ui::refresh(app::TABS);
        }
        util::log(format!("PERF tab strip refresh: {:.3} ms each (12 tabs)", ms(t) / 200.0));
        let t = std::time::Instant::now();
        for _ in 0..200 {
            ui::refresh(app::TOOLBAR);
        }
        util::log(format!("PERF toolbar refresh: {:.3} ms each", ms(t) / 200.0));
        let t = std::time::Instant::now();
        for _ in 0..200 {
            app::web_state_changed(app::store(|s| s.active_tab().map(|(_, t)| t.id)).unwrap());
        }
        util::log(format!("PERF 200 page-state notifications: {:.3} ms", ms(t)));
        switch_round(0, 0, vec![], vec![]);
    });
}

/// Visits every tab twice in turn, timing the synchronous switch and the
/// time until the page is ready.
fn switch_round(pass: usize, i: usize, mut block: Vec<f64>, ready: Vec<f64>) {
    let n = PERF_PAGES.len();
    if i == n {
        block.sort_by(f64::total_cmp);
        let mut r = ready.clone();
        r.sort_by(f64::total_cmp);
        util::log(format!(
            "PERF pass {pass}: switch blocks main thread median {:.2} ms (max {:.2}); page ready median {:.0} ms (max {:.0}); ready times {:?}",
            block[n / 2], block[n - 1], r[n / 2], r[n - 1], ready.iter().map(|x| *x as i64).collect::<Vec<_>>()
        ));
        if pass == 0 {
            util::after(1.0, || switch_round(1, 0, vec![], vec![]));
            return;
        }
        let t = std::time::Instant::now();
        web::save_all_states();
        util::log(format!("PERF save all tab states: {:.2} ms", ms(t)));
        let t = std::time::Instant::now();
        app::app_resigned_active();
        util::log(format!("PERF app to background (saves everything): {:.2} ms", ms(t)));
        util::log("PERF done");
        util::after(2.0, || NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None));
        return;
    }
    let t = std::time::Instant::now();
    app::select_tab_index(i);
    ui::refresh(app::TABS | app::CONTENT | app::TOOLBAR);
    block.push(ms(t));
    when_loaded(t, move |m| {
        let block = block.clone();
        let mut ready = ready.clone();
        ready.push(m);
        util::after(0.6, move || switch_round(pass, i + 1, block, ready));
    });
}

/// A real mouse click (down + up through `NSApp.sendEvent`, the path a
/// physical click takes) at a point given in CSS pixels from the active
/// page's top-left corner.
fn click_page(x: f64, y: f64, mods: NSEventModifierFlags) {
    let Some(view) = web::active_view() else { return };
    let b = view.bounds();
    let local = NSPoint::new(x, if view.isFlipped() { y } else { b.size.height - y });
    let at = view.convertPoint_toView(local, None);
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let window = view.window().map(|w| w.windowNumber()).unwrap_or(0);
    for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
        let e = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, at, mods, 0.0, window, None, 0, 1, 1.0,
        );
        if let Some(e) = e {
            app.sendEvent(&e);
        }
    }
}

fn eval_log(label: &'static str, js: &str) {
    web::eval_active(js, move |r| util::log(format!("SELFTEST {label}: {r}")));
}

fn tab_summary() -> String {
    app::store(|s| {
        s.active_client()
            .map(|c| {
                let live = web::live_tabs();
                let titles: Vec<String> = c
                    .tabs
                    .iter()
                    .map(|t| {
                        let loaded = web::page_state(t.id).map(|p| p.url).filter(|u| !u.is_empty());
                        format!("{}{} [{}]", if Some(t.id) == c.active_tab_id { "*" } else { "" }, t.url.clone().unwrap_or_default(), match (live.contains(&t.id), loaded) {
                            (true, Some(_)) => "loaded",
                            (true, None) => "awake, not loaded",
                            _ => "asleep",
                        })
                    })
                    .collect();
                format!("{} tabs {:?}", c.tabs.len(), titles)
            })
            .unwrap_or_default()
    })
}

/// `ZCLIENTS_SELFTEST=input`: ⌘A in a text box and on the page, ⌘-click on
/// a plain link and on a link a page script opens itself (Zoho style).
fn input_test() {
    let cmd = NSEventModifierFlags::Command;
    util::after(3.0, || {
        app::add_client("Input Test", "#228844", None, None);
        util::after(0.5, || app::address_entered("https://www.zoho.com/"));
    });
    util::after(14.0, move || {
        if let Some(v) = web::active_view() {
            ui::focus_view(&v);
        }
        eval_log("setup", r#"(() => {
            const t = Object.assign(document.body.appendChild(document.createElement('textarea')), {id: 'zc-t', value: 'hello select all'});
            t.style.cssText = 'position:fixed;top:120px;left:20px;width:300px;height:40px;z-index:2147483647';
            const a = Object.assign(document.body.appendChild(document.createElement('a')), {id: 'zc-a', href: 'https://www.zoho.com/crm/', textContent: 'plain link'});
            a.style.cssText = 'position:fixed;top:20px;left:20px;width:200px;height:30px;display:block;background:#ff0;z-index:2147483647';
            const b = Object.assign(document.body.appendChild(document.createElement('a')), {id: 'zc-b', href: 'https://www.zoho.com/books/', textContent: 'script link'});
            b.style.cssText = 'position:fixed;top:60px;left:20px;width:200px;height:30px;display:block;background:#0ff;z-index:2147483647';
            b.addEventListener('click', e => { e.preventDefault(); if (e.metaKey || e.ctrlKey) { window.open(b.href, '_blank'); } else { location.href = b.href; } });
            addEventListener('keydown', e => { window.__zcKeys = (window.__zcKeys || '') + (e.metaKey ? 'cmd+' : '') + e.key + ' '; }, true);
            addEventListener('keydown', e => { if (e.defaultPrevented) { window.__zcKeys += '(page prevented) '; } }, false);
            t.focus(); t.setSelectionRange(0, 0);
            return 'ready, focused=' + (document.activeElement === t);
        })()"#);
    });
    // ZCLIENTS_REAL_KEYS=1: the ⌘A presses come from outside (System Events),
    // the only way to see what a physical key press does.
    let real = std::env::var_os("ZCLIENTS_REAL_KEYS").is_some();
    log_keys();

    util::after(15.5, move || {
        if !real {
            press("a", 0, cmd);
        }
        util::after(0.6, || eval_log("⌘A in text box", "(() => { const t = document.getElementById('zc-t'); return 'selected ' + (t.selectionEnd - t.selectionStart) + ' of ' + t.value.length + '; page saw keys: ' + (window.__zcKeys || 'none') + '; active=' + (document.activeElement && document.activeElement.id); })()"));
    });
    util::after(17.0, move || {
        eval_log("blur", "(() => { document.activeElement.blur(); getSelection().removeAllRanges(); return 'page focused'; })()");
        util::after(0.4, move || {
            if !real {
                press("a", 0, cmd);
            }
            util::after(0.6, || eval_log("⌘A on page", "(() => 'page selection length ' + getSelection().toString().length)()"));
        });
    });
    util::after(19.0, move || {
        util::log(format!("SELFTEST before ⌘-clicks: {}", tab_summary()));
        click_page(60.0, 35.0, cmd);
    });
    util::after(23.0, move || {
        util::log(format!("SELFTEST after ⌘-click on plain link: {}", tab_summary()));
        click_page(60.0, 75.0, cmd);
    });
    util::after(27.0, || {
        util::log(format!("SELFTEST after ⌘-click on script link: {}", tab_summary()));
    });
    util::after(28.0, || {
        let live = web::live_tabs().len();
        util::log(format!("SELFTEST awake tabs: {live}"));
        util::log("SELFTEST done");
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
    });
}

/// `ZCLIENTS_SELFTEST=find`: logs the find bar and window state twice a
/// second; the keys (⌘F, typing, Return, Esc) come from System Events.
/// `ZCLIENTS_FULLSCREEN=1` puts the window in full screen first.
fn find_test() {
    log_keys();
    util::after(3.0, || {
        app::add_client("Find Test", "#664488", None, None);
        util::after(0.5, || app::address_entered("https://www.zoho.com/"));
    });
    if std::env::var_os("ZCLIENTS_FULLSCREEN").is_some() {
        util::after(6.0, || {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            if let Some(w) = app.mainWindow().or_else(|| app.windows().firstObject()) {
                w.toggleFullScreen(None);
            }
        });
    }
    std::mem::forget(util::every(0.5, || {
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        let w = app.windows().iter().find(|w| w.isVisible() || w.isMiniaturized());
        let state = w.map(|w| {
            let full = w.styleMask().contains(objc2_app_kit::NSWindowStyleMask::FullScreen);
            let fr = w.firstResponder().map(|r| r.class().name().to_string_lossy().into_owned()).unwrap_or_default();
            format!("minimized={} fullscreen={full} visible={} firstResponder={fr}", w.isMiniaturized(), w.isVisible())
        });
        util::log(format!("SELFTEST state t={:.1}s find bar={} {state:?} app hidden={}", util::ms_since_start() / 1000.0, ui::find_visible(), app.isHidden()));
    }));
    util::after(30.0, || {
        util::log("SELFTEST done");
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
    });
}

/// `ZCLIENTS_SELFTEST=memory` (with ZCLIENTS_IDLE_SECS=6): four tabs are
/// opened and each shown once; after the idle wait Memory Saver should leave
/// only the tab in front awake. The script outside measures page memory.
fn memory_test() {
    util::after(1.0, || {
        app::add_client("Memory Test", "#448866", None, None);
        app::address_entered(PERF_PAGES[0]);
    });
    for (i, page) in PERF_PAGES[1..4].iter().enumerate() {
        util::after(6.0 + i as f64 * 5.0, move || app::address_entered_in_new_tab(page));
    }
    util::after(24.0, || {
        app::select_tab_index(0);
        util::log(format!("SELFTEST MEMORY awake before idle: {} (pid {})", web::live_tabs().len(), std::process::id()));
    });
    util::after(40.0, || {
        web::sleep_idle_tabs();
        util::log(format!("SELFTEST MEMORY awake after Memory Saver: {} — {}", web::live_tabs().len(), tab_summary()));
    });
    util::after(52.0, || {
        util::log("SELFTEST done");
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
    });
}

const CALL_START: &str = r#"
const s = await navigator.mediaDevices.getUserMedia({ video: { width: 640, height: 480 }, audio: true });
const a = new RTCPeerConnection(), b = new RTCPeerConnection();
a.onicecandidate = e => e.candidate && b.addIceCandidate(e.candidate);
b.onicecandidate = e => e.candidate && a.addIceCandidate(e.candidate);
b.ontrack = e => { window.__zcRemote = e.streams[0]; };
s.getTracks().forEach(t => a.addTrack(t, s));
const o = await a.createOffer(); await a.setLocalDescription(o); await b.setRemoteDescription(o);
const an = await b.createAnswer(); await b.setLocalDescription(an); await a.setRemoteDescription(an);
window.__zcPc = a; window.__zcStream = s;
return 'call started: ' + s.getTracks().map(t => t.kind + ' ' + t.readyState).join(', ');
"#;

const CALL_STATS: &str = r#"
const r = await window.__zcPc.getStats();
let v = 0, au = 0;
r.forEach(x => {
  if (x.type === 'outbound-rtp' && x.kind === 'video') { v = x.framesEncoded || x.framesSent || 0; }
  if (x.type === 'outbound-rtp' && x.kind === 'audio') { au = x.packetsSent || 0; }
});
const t = window.__zcStream.getTracks().map(t => t.kind + (t.muted ? ' muted' : '') + (t.enabled ? '' : ' disabled') + ' ' + t.readyState).join(', ');
return JSON.stringify({ v, au, t, pc: window.__zcPc.connectionState, vis: document.visibilityState });
"#;

fn call_stats(tab: uuid::Uuid, done: impl Fn(serde_json::Value) + 'static) {
    web::eval_in_tab_async(tab, CALL_STATS, move |r| done(serde_json::from_str(&r).unwrap_or(serde_json::Value::String(r))));
}

/// Measure a call phase: video frames encoded and audio packets sent per
/// second over `secs`, read from WebRTC's own counters.
fn call_phase(tab: uuid::Uuid, label: &'static str, secs: f64) {
    call_stats(tab, move |a| {
        util::after(secs, move || {
            let a = a.clone();
            call_stats(tab, move |b| {
                let f = |k: &str, x: &serde_json::Value| x.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
                util::log(format!(
                    "SELFTEST CALL {label}: video {:.1} fps, audio {:.1} packets/s; tracks [{}] page {} {}; camera={:?} mic={:?} sleep disabler={} tab call icon={:?}",
                    (f("v", &b) - f("v", &a)) / secs,
                    (f("au", &b) - f("au", &a)) / secs,
                    b.get("t").and_then(|v| v.as_str()).unwrap_or("?"),
                    b.get("vis").and_then(|v| v.as_str()).unwrap_or("?"),
                    b.get("pc").and_then(|v| v.as_str()).unwrap_or("?"),
                    web::live_view(tab).map(|v| unsafe { v.cameraCaptureState() }.0),
                    web::live_view(tab).map(|v| unsafe { v.microphoneCaptureState() }.0),
                    web::has_sleep_disabler(tab),
                    web::call_state(tab),
                ));
            });
        });
    });
}

/// `ZCLIENTS_SELFTEST=meet` (with ZCLIENTS_MOCK_MEDIA=1): a WebRTC call with
/// WebKit's fake camera and microphone, measured in front, behind another
/// tab and with the window minimized; then a screen-share request.
fn meet_test() {
    util::after(1.0, || {
        app::add_client("Call Test", "#aa3355", None, None);
        app::address_entered(PERF_PAGES[0]);
    });
    util::after(12.0, || {
        let Some(tab) = app::store(|s| s.active_tab().map(|(_, t)| t.id)) else { return };
        web::eval_in_tab_async(tab, CALL_START, |r| util::log(format!("SELFTEST CALL {r}")));
        util::after(4.0, move || call_phase(tab, "in front", 4.0));
        util::after(9.0, move || {
            app::address_entered_in_new_tab(PERF_PAGES[1]);
            util::after(2.0, move || call_phase(tab, "behind another tab", 5.0));
        });
        util::after(18.0, move || {
            app::select_tab_index(0);
            util::after(1.0, || {
                let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
                if let Some(w) = app.mainWindow().or_else(|| app.windows().firstObject()) {
                    w.miniaturize(None);
                }
            });
            util::after(3.0, move || call_phase(tab, "window minimized", 5.0));
        });
        util::after(28.0, || {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            if let Some(w) = app.windows().iter().find(|w| w.isMiniaturized()) {
                w.deminiaturize(None);
            }
        });
        util::after(30.0, move || {
            // (t = 42 s, after the window is back.) getDisplayMedia needs a
            // click (user activation) first.
            web::eval_active("(() => { const b = Object.assign(document.body.appendChild(document.createElement('button')), {id: 'zc-share', textContent: 'share'}); b.style.cssText = 'position:fixed;top:20px;left:20px;width:200px;height:30px;z-index:2147483647'; b.onclick = () => { window.__zcShare = 'asked (no answer yet)'; return navigator.mediaDevices.getDisplayMedia({video: true}).then(s => { window.__zcShare = 'sharing ' + s.getTracks().map(t => t.label || t.kind).join(','); }, e => { window.__zcShare = 'refused: ' + e.name; }); }; return location.host; })()", |r| util::log(format!("SELFTEST CALL share button on {r}")));
            util::after(0.5, || click_page(60.0, 35.0, NSEventModifierFlags::empty()));
            util::after(5.5, || web::eval_active("(() => String(window.__zcShare))()", |r| util::log(format!("SELFTEST CALL screen sharing: {r}"))));
        });
    });
    // ⌘D comes from outside: at ~50 s on the Zoho page (control: duplicates
    // the tab), at ~60 s on a Meet page (must reach Meet, no new tab).
    util::after(49.0, || util::log(format!("SELFTEST CALL before ⌘D on Zoho: {}", tab_summary())));
    util::after(52.0, || {
        util::log(format!("SELFTEST CALL after ⌘D on Zoho: {}", tab_summary()));
        app::address_entered_in_new_tab("https://meet.google.com/");
    });
    util::after(59.0, || util::log(format!("SELFTEST CALL before ⌘D on Meet: {}", tab_summary())));
    util::after(62.0, || util::log(format!("SELFTEST CALL after ⌘D on Meet: {}", tab_summary())));
    util::after(64.0, || {
        util::log("SELFTEST done");
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
    });
}

pub fn start() {
    if !util::is_test_build() {
        return;
    }
    match std::env::var("ZCLIENTS_SELFTEST").as_deref() {
        Ok("perf") => return perf_test(),
        Ok("prep") => return prep(),
        Ok("downloads") => return downloads_test(),
        Ok("input") => return input_test(),
        Ok("find") => return find_test(),
        Ok("memory") => return memory_test(),
        Ok("meet") => return meet_test(),
        Ok(_) => {}
        Err(_) => return,
    }
    let cmd = NSEventModifierFlags::Command;
    util::after(7.0, move || {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let menu = app.mainMenu();
        util::log(format!("SELFTEST active={} key window={} main menu={} items={}", app.isActive(), app.keyWindow().is_some(), menu.is_some(), menu.as_ref().map(|m| m.numberOfItems()).unwrap_or(0)));
        if let Some(menu) = menu {
            for i in 0..menu.numberOfItems() {
                let item = menu.itemAtIndex(i).unwrap();
                if let Some(sub) = item.submenu() {
                    for j in 0..sub.numberOfItems() {
                        let it = sub.itemAtIndex(j).unwrap();
                        let k = it.keyEquivalent().to_string();
                        if k.eq_ignore_ascii_case("s") || k.eq_ignore_ascii_case("v") || k.eq_ignore_ascii_case("d") {
                            util::log(format!("  {} > {:?} key={k:?} mask={:?} enabled={}", item.title(), it.title().to_string(), it.keyEquivalentModifierMask(), it.isEnabled()));
                        }
                    }
                }
            }
            let e = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown, NSPoint::new(0.0, 0.0), cmd, 0.0, 0, None, &util::ns("s"), &util::ns("s"), false, 1).unwrap();
            util::log(format!("SELFTEST mainMenu.performKeyEquivalent(⌘S) = {}", menu.performKeyEquivalent(&e)));
        }
    });
    util::after(8.0, move || {
        let before = ui::sidebar_visible();
        press("s", 1, cmd);
        util::after(0.5, move || util::log(format!("SELFTEST ⌘S sidebar {before} -> {}", ui::sidebar_visible())));
    });
    util::after(9.0, move || {
        let before = tab_count();
        press("d", 2, cmd);
        util::after(0.8, move || util::log(format!("SELFTEST ⌘D tabs {before} -> {}", tab_count())));
    });
    util::after(10.5, move || {
        copy_from_outside("ZC-PASTE-ADDRESS");
        ui::focus_address();
        ui::set_address("");
        press("v", 9, cmd);
        util::after(0.5, || util::log(format!("SELFTEST ⌘V address field = {:?}", ui::address_text())));
    });
    util::after(12.0, move || {
        copy_from_outside("ZC-PASTE-WEB");
        if let Some(v) = web::active_view() {
            ui::focus_view(&v);
        }
        web::eval_active("(() => { const f = document.getElementById('zc-test') || Object.assign(document.body.appendChild(document.createElement('textarea')), {id: 'zc-test'}); f.value=''; f.focus(); return 'focused ' + (document.activeElement === f); })()", |r| {
            util::log(format!("SELFTEST web field: {r}"));
        });
        util::after(0.6, move || {
            press("v", 9, cmd);
            util::after(0.8, || {
                web::eval_active("(() => { const f = document.getElementById('zc-test'); return f ? f.value : 'no field'; })()", |r| {
                    util::log(format!("SELFTEST ⌘V web field = {r:?}"));
                });
            });
        });
    });
    util::after(16.0, || {
        util::log("SELFTEST done");
        let mtm = MainThreadMarker::new().unwrap();
        NSApplication::sharedApplication(mtm).terminate(None);
    });
}
