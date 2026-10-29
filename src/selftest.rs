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
            kind,
            NSPoint::new(0.0, 0.0),
            mods,
            0.0,
            window,
            None,
            &util::ns(chars),
            &util::ns(chars),
            false,
            code,
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
            ev.charactersIgnoringModifiers().map(|c| c.to_string()),
            ev.modifierFlags(),
            app.isActive(),
            app.keyWindow().is_some()
        ));
        e.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(objc2_app_kit::NSEventMask::KeyDown, &block) };
    std::mem::forget(monitor);
}

fn prep() {
    log_keys();
    util::after(5.0, || {
        web::eval_active(
            "(() => { const f = document.getElementById('zc-test') || Object.assign(document.body.appendChild(document.createElement('textarea')), {id: 'zc-test'}); f.style.cssText='position:fixed;top:10px;left:10px;width:400px;height:60px;z-index:99999'; f.value=''; f.focus(); return 'focused ' + (document.activeElement === f); })()",
            |r| util::log(format!("SELFTEST prep: {r}")),
        );
        if let Some(v) = web::active_view() {
            ui::focus_view(&v);
        }
    });
    util::after(18.0, || {
        web::eval_active("(() => { const f = document.getElementById('zc-test'); return f ? f.value : 'no field'; })()", |r| {
            util::log(format!("SELFTEST web field after real keys = {r:?}"))
        });
        util::log(format!("SELFTEST sidebar visible = {}", ui::sidebar_visible()));
    });
    util::after(19.0, app::quit_without_asking);
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
                if depth < 3 {
                    names(&s, depth + 1, out);
                }
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
        app::add_client("Download Test", "#3366cc", None, app::LoginChange::Keep, Default::default());
        util::after(0.5, || app::address_entered("https://www.zoho.com/"));
    });
    util::after(14.0, || {
        report("loaded");
        web::eval_active(
            r#"(() => {
            const pdf = '%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 144]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF';
            window.open(URL.createObjectURL(new Blob([pdf], {type: 'application/pdf'})), '_blank', 'width=600,height=500');
            return 'opened pdf popup';
        })()"#,
            |r| util::log(format!("SELFTEST pdf popup: {r}")),
        );
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
        web::eval_active(
            r#"(() => {
            const f = document.createElement('iframe');
            f.style.display = 'none';
            f.src = URL.createObjectURL(new Blob(['PK\x05\x06' + '\0'.repeat(18)], {type: 'application/zip'}));
            document.body.appendChild(f);
            return 'hidden frame';
        })()"#,
            |r| util::log(format!("SELFTEST zip frame: {r}")),
        );
    });
    util::after(29.0, || {
        report("after hidden frame zip");
        web::eval_active(
            r#"(() => {
            window.open(URL.createObjectURL(new Blob(['PK\x05\x06' + '\0'.repeat(18)], {type: 'application/zip'})), '_blank', 'width=500,height=400');
            return 'zip popup';
        })()"#,
            |r| util::log(format!("SELFTEST zip popup: {r}")),
        );
    });
    util::after(34.0, || {
        report("after zip popup");
        app::cycle_tab(-1);
        // Like a CRM attachment: the page downloads a PDF as a file.
        web::eval_active(
            r#"(() => {
            const pdf = '%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 144]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF';
            const a = document.createElement('a');
            a.href = URL.createObjectURL(new Blob([pdf], {type: 'application/pdf'}));
            a.download = 'Quote-1001.pdf';
            document.body.appendChild(a);
            a.click();
            return 'attachment clicked';
        })()"#,
            |r| util::log(format!("SELFTEST pdf attachment: {r}")),
        );
    });
    util::after(39.0, || {
        report("after pdf attachment");
        app::cycle_tab(-1);
        util::after(0.5, || {
            let first = downloads::with_items(|v| v.first().map(|i| i.key()));
            let shown = first.is_some_and(downloads::show_pdf);
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
        app::quit_without_asking();
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
    "https://www.zoho.com/",
    "https://www.zoho.com/crm/",
    "https://www.zoho.com/books/",
    "https://www.zoho.com/mail/",
    "https://www.zoho.com/workdrive/",
    "https://www.zoho.com/desk/",
    "https://www.zoho.com/projects/",
    "https://www.zoho.com/people/",
    "https://www.zoho.com/sign/",
    "https://www.zoho.com/analytics/",
    "https://www.zoho.com/creator/",
    "https://www.zoho.com/invoice/",
];

/// `ZCLIENTS_SELFTEST=perf`: launch time, then main-thread cost of the tab
/// strip, toolbar, tab switches and state saving with 12 Zoho tabs open.
fn perf_test() {
    util::log(format!("PERF window built at {:.0} ms", util::ms_since_start()));
    util::after(0.5, || {
        app::add_client("Perf", "#884422", None, app::LoginChange::Keep, Default::default());
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
            block[n / 2],
            block[n - 1],
            r[n / 2],
            r[n - 1],
            ready.iter().map(|x| *x as i64).collect::<Vec<_>>()
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
        util::after(2.0, app::quit_without_asking);
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
        let e = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, at, mods, 0.0, window, None, 0, 1, 1.0);
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
                        format!(
                            "{}{} [{}]",
                            if Some(t.id) == c.active_tab_id { "*" } else { "" },
                            t.url.clone().unwrap_or_default(),
                            match (live.contains(&t.id), loaded) {
                                (true, Some(_)) => "loaded",
                                (true, None) => "awake, not loaded",
                                _ => "asleep",
                            }
                        )
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
        app::add_client("Input Test", "#228844", None, app::LoginChange::Keep, Default::default());
        util::after(0.5, || app::address_entered("https://www.zoho.com/"));
    });
    util::after(14.0, move || {
        if let Some(v) = web::active_view() {
            ui::focus_view(&v);
        }
        eval_log(
            "setup",
            r#"(() => {
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
        })()"#,
        );
    });
    // ZCLIENTS_REAL_KEYS=1: the ⌘A presses come from outside (System Events),
    // the only way to see what a physical key press does.
    let real = std::env::var_os("ZCLIENTS_REAL_KEYS").is_some();
    log_keys();

    util::after(15.5, move || {
        if !real {
            press("a", 0, cmd);
        }
        util::after(0.6, || {
            eval_log(
                "⌘A in text box",
                "(() => { const t = document.getElementById('zc-t'); return 'selected ' + (t.selectionEnd - t.selectionStart) + ' of ' + t.value.length + '; page saw keys: ' + (window.__zcKeys || 'none') + '; active=' + (document.activeElement && document.activeElement.id); })()",
            )
        });
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
        app::quit_without_asking();
    });
}

/// `ZCLIENTS_SELFTEST=find`: logs the find bar and window state twice a
/// second; the keys (⌘F, typing, Return, Esc) come from System Events.
/// `ZCLIENTS_FULLSCREEN=1` puts the window in full screen first.
fn find_test() {
    log_keys();
    util::after(3.0, || {
        app::add_client("Find Test", "#664488", None, app::LoginChange::Keep, Default::default());
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
        app::quit_without_asking();
    });
}

/// `ZCLIENTS_SELFTEST=memory` (with ZCLIENTS_IDLE_SECS=6): four tabs are
/// opened and each shown once; after the idle wait Memory Saver should leave
/// only the tab in front awake. The script outside measures page memory.
fn memory_test() {
    util::after(1.0, || {
        app::add_client("Memory Test", "#448866", None, app::LoginChange::Keep, Default::default());
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
        app::quit_without_asking();
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
        app::add_client("Call Test", "#aa3355", None, app::LoginChange::Keep, Default::default());
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
            web::eval_active(
                "(() => { const b = Object.assign(document.body.appendChild(document.createElement('button')), {id: 'zc-share', textContent: 'share'}); b.style.cssText = 'position:fixed;top:20px;left:20px;width:200px;height:30px;z-index:2147483647'; b.onclick = () => { window.__zcShare = 'asked (no answer yet)'; return navigator.mediaDevices.getDisplayMedia({video: true}).then(s => { window.__zcShare = 'sharing ' + s.getTracks().map(t => t.label || t.kind).join(','); }, e => { window.__zcShare = 'refused: ' + e.name; }); }; return location.host; })()",
                |r| util::log(format!("SELFTEST CALL share button on {r}")),
            );
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
        app::quit_without_asking();
    });
}

fn pasteboard_text() -> String {
    NSPasteboard::generalPasteboard().stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string()).unwrap_or_default()
}

fn active_url() -> String {
    app::store(|s| s.active_tab().map(|(_, t)| t.id)).and_then(web::page_state).map(|p| p.url).unwrap_or_default()
}

/// `ZCLIENTS_SELFTEST=address`: copy part of tab A's address, switch to tab
/// B, click into its page; then paste into B's address bar and click into
/// the page again. ⌘C / ⌘V come from System Events (~16 s and ~24.5 s).
fn address_test() {
    log_keys();
    util::after(2.0, || {
        app::add_client("Address Test", "#336699", None, app::LoginChange::Keep, Default::default());
        app::address_entered("https://www.zoho.com/");
    });
    util::after(6.0, || {
        app::address_entered_in_new_tab("https://www.zoho.com/crm/");
        util::after(4.0, || app::select_tab_index(0));
    });
    util::after(14.0, || {
        ui::click_address();
        util::after(0.4, || util::log(format!("SELFTEST ADDR clicked address bar on tab A ({}): {}", active_url(), ui::address_state())));
        util::after(0.8, || {
            ui::select_in_address(8, 20);
            util::log(format!("SELFTEST ADDR selected part: {}", ui::address_state()));
        });
    });
    util::after(17.0, || {
        util::log(format!("SELFTEST ADDR after ⌘C, clipboard = {:?}", pasteboard_text()));
        ui::click_tab_chip(1);
        util::after(0.6, || util::log(format!("SELFTEST ADDR switched to tab B ({}): {}", active_url(), ui::address_state())));
    });
    util::after(19.0, || {
        click_page(400.0, 400.0, NSEventModifierFlags::empty());
        util::after(3.0, || util::log(format!("SELFTEST ADDR clicked into tab B's page; tab B now at {} ({})", active_url(), tab_summary())));
    });
    util::after(23.5, || {
        ui::click_address();
        util::after(0.4, || util::log(format!("SELFTEST ADDR clicked tab B's address bar: {}", ui::address_state())));
    });
    util::after(25.5, || {
        util::log(format!("SELFTEST ADDR after ⌘V: {}", ui::address_state()));
        click_page(400.0, 400.0, NSEventModifierFlags::empty());
        util::after(3.0, || util::log(format!("SELFTEST ADDR clicked into the page after pasting; tab B now at {} ({})", active_url(), tab_summary())));
    });
    util::after(31.0, || {
        util::log("SELFTEST done");
        app::quit_without_asking();
    });
}

/// `ZCLIENTS_SELFTEST=editor`: ⌘A / ⌘Z / Control-A reach a code editor's
/// own key handler (as in Safari); ⌘A in a plain text box still selects all.
fn editor_test() {
    util::after(2.0, || {
        app::add_client("Editor Test", "#226644", None, app::LoginChange::Keep, Default::default());
        app::address_entered("https://www.zoho.com/");
    });
    util::after(11.0, || {
        web::eval_active(
            r#"(() => { try {
            const ed = document.body.appendChild(document.createElement('div'));
            ed.className = 'CodeMirror';
            ed.style.cssText = 'position:fixed;top:10px;left:10px;width:400px;height:80px;background:#222;z-index:2147483647';
            const ta = ed.appendChild(document.createElement('textarea'));
            ta.style.cssText = 'position:absolute;width:1px;height:1px;opacity:0';
            window.__zcKeys = [];
            ta.addEventListener('keydown', e => { if (e.metaKey) { window.__zcKeys.push((e.shiftKey ? 'shift+' : '') + 'cmd+' + e.key.toLowerCase()); e.preventDefault(); } });
            const plain = document.body.appendChild(document.createElement('textarea'));
            plain.id = 'zc-plain';
            plain.value = 'line one\nline two\nline three';
            plain.style.cssText = 'position:fixed;top:100px;left:10px;width:400px;height:80px;z-index:2147483647';
            ta.focus();
            return 'editor focused: ' + (document.activeElement === ta);
        } catch (e) { return 'setup error: ' + e; } })()"#,
            |r| util::log(format!("SELFTEST EDITOR {r}")),
        );
        if let Some(v) = web::active_view() {
            ui::focus_view(&v);
        }
    });
    util::after(13.0, || {
        press("a", 0, NSEventModifierFlags::Command);
        press("z", 6, NSEventModifierFlags::Command);
        press("z", 6, NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
        press("a", 0, NSEventModifierFlags::Control);
        util::after(1.0, || web::eval_active("JSON.stringify(window.__zcKeys)", |r| util::log(format!("SELFTEST EDITOR code editor received: {r}"))));
    });
    util::after(15.5, || {
        web::eval_active("(() => { const p = document.getElementById('zc-plain'); p.focus(); p.setSelectionRange(0, 0); return 'plain box focused'; })()", |r| {
            util::log(format!("SELFTEST EDITOR {r}"))
        });
        util::after(1.0, || {
            press("a", 0, NSEventModifierFlags::Command);
            util::after(0.8, || {
                web::eval_active(
                    "(() => { const p = document.getElementById('zc-plain'); return 'selected ' + (p.selectionEnd - p.selectionStart) + ' of ' + p.value.length; })()",
                    |r| util::log(format!("SELFTEST EDITOR plain text box after ⌘A: {r}")),
                )
            });
        });
    });
    util::after(20.0, || {
        util::log("SELFTEST done");
        app::quit_without_asking();
    });
}

/// `ZCLIENTS_SELFTEST=address2`: text typed into the address bar never
/// navigates when focus moves — to another tab, into the page — only Return
/// for the same tab does; Esc restores the tab's address.
fn address_rules_test() {
    util::after(2.0, || {
        app::add_client("Address Rules", "#336699", None, app::LoginChange::Keep, Default::default());
        app::address_entered("https://www.zoho.com/");
    });
    util::after(5.0, || {
        app::address_entered_in_new_tab("https://www.zoho.com/crm/");
        util::after(4.0, || app::select_tab_index(0));
    });
    util::after(12.0, || {
        ui::click_address();
        util::after(0.3, || {
            ui::type_in_address("books.zoho.com/half-typed");
            util::log(format!("SELFTEST ADDR2 typed on tab A ({}): {}", active_url(), ui::address_state()));
            ui::click_tab_chip(1);
        });
        util::after(1.5, || util::log(format!("SELFTEST ADDR2 switched to tab B: url={} {}", active_url(), ui::address_state())));
    });
    util::after(16.0, || {
        ui::click_address();
        util::after(0.3, || {
            ui::type_in_address("people.zoho.com/partial");
            click_page(400.0, 400.0, NSEventModifierFlags::empty());
        });
        util::after(3.0, || util::log(format!("SELFTEST ADDR2 clicked into page after typing: url={} {}", active_url(), ui::address_state())));
    });
    util::after(21.0, || {
        ui::click_address();
        util::after(0.3, || {
            ui::type_in_address("desk.zoho.com/escape");
            press("\u{1b}", 53, NSEventModifierFlags::empty());
            util::after(0.5, || util::log(format!("SELFTEST ADDR2 after Esc: url={} {}", active_url(), ui::address_state())));
        });
    });
    util::after(24.0, || {
        // Return arriving for a tab that is no longer in front.
        let stale = app::store(|s| s.active_client().and_then(|c| c.tabs.first().map(|t| t.id)));
        app::address_entered_for(stale, "https://www.zoho.com/books/");
        util::after(3.0, || util::log(format!("SELFTEST ADDR2 stale Return: url={}", active_url())));
    });
    util::after(28.0, || {
        ui::click_address();
        util::after(0.3, || {
            ui::type_in_address("https://www.zoho.com/mail/");
            press("\r", 36, NSEventModifierFlags::empty());
        });
        util::after(4.0, || util::log(format!("SELFTEST ADDR2 Return on the same tab: url={}", active_url())));
    });
    util::after(34.0, || {
        util::log("SELFTEST done");
        app::quit_without_asking();
    });
}

/// `ZCLIENTS_SELFTEST=security` (with `ZCLIENTS_ALERTS=decline`,
/// `ZCLIENTS_MOCK_MEDIA=1 ZCLIENTS_MOCK_PROMPT=1`): drives the hardened
/// paths in the real app and logs what each did.
fn security_test() {
    util::after(2.0, || {
        app::add_client("Security Test", "#aa3355", None, app::LoginChange::Keep, Default::default());
        util::after(0.5, || app::address_entered("https://www.zoho.com/"));
    });
    util::after(11.0, || {
        let inspectable = web::active_view().is_some_and(|v| unsafe { v.isInspectable() });
        util::log(format!("SELFTEST SEC inspectable={inspectable} (developer mode off)"));
        web::eval_active("navigator.userAgent", |r| util::log(format!("SELFTEST SEC userAgent={r}")));
        web::eval_in_client_world("return typeof window.__zcFill", |r| util::log(format!("SELFTEST SEC login helper on www.zoho.com: {r}")));
    });
    util::after(13.0, || {
        let before = tab_count();
        web::eval_active("(() => { window.open('https://www.zoho.com/crm/'); return 'window.open without a click'; })()", |r| util::log(format!("SELFTEST SEC {r}")));
        util::after(2.0, move || util::log(format!("SELFTEST SEC popup without gesture: tabs {before} -> {}", tab_count())));
    });
    util::after(16.0, || {
        let before = downloads::with_items(|v| v.len());
        web::eval_active(
            "(() => { const a = document.createElement('a'); a.href = 'data:application/octet-stream,hello'; a.download = 'nogesture.bin'; document.body.appendChild(a); a.click(); return 'download without a click'; })()",
            |r| util::log(format!("SELFTEST SEC {r}")),
        );
        util::after(2.0, move || util::log(format!("SELFTEST SEC download without gesture: items {before} -> {}", downloads::with_items(|v| v.len()))));
    });
    util::after(19.0, || {
        web::eval_active("(() => { location.href = 'mailto:selftest@example.invalid'; return 'mailto without a click'; })()", |r| util::log(format!("SELFTEST SEC {r}")));
    });
    util::after(21.0, || {
        web::eval_active("(() => { navigator.mediaDevices.getUserMedia({video: true}).then(() => 0, () => 0); return 'camera request from www.zoho.com'; })()", |r| {
            util::log(format!("SELFTEST SEC {r}"))
        });
        util::after(2.0, || {
            for u in web::recent_media_uses() {
                util::log(format!("SELFTEST SEC media decision: {} {} decision={}", u.origin, u.what, u.decision));
            }
        });
    });
    util::after(24.0, || {
        // An HTML file disguised as a PDF must not be shown; a real one is.
        let Some(client) = app::store(|s| s.active_client_id) else { return };
        let dir = crate::pdf::new_preview_dir().unwrap();
        let fake = dir.join("fake.pdf");
        std::fs::write(&fake, b"<html><body>not a pdf</body></html>").unwrap();
        app::open_pdf_tab(client, fake, "fake.pdf".into(), None);
        util::after(2.5, || {
            web::eval_active("location.href", |r| util::log(format!("SELFTEST SEC disguised PDF tab shows: {r}")));
            let src = std::env::temp_dir().join("zc-selftest-real.pdf");
            std::fs::write(&src, b"%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 144]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF").unwrap();
            let client = app::store(|s| s.active_client_id).unwrap();
            let opened = app::open_downloaded_pdf(client, &src, "Real.pdf", None);
            util::after(2.5, move || {
                web::eval_active("location.protocol", move |r| util::log(format!("SELFTEST SEC real PDF tab opened={} protocol={r}", opened.is_some())));
                let path = app::store(|s| s.active_tab().and_then(|(_, t)| t.pdf_path.clone())).unwrap_or_default();
                use std::os::unix::fs::PermissionsExt;
                let p = std::path::Path::new(&path);
                let modes = (
                    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777).unwrap_or(0),
                    p.parent().and_then(|d| std::fs::metadata(d).ok()).map(|m| m.permissions().mode() & 0o777).unwrap_or(0),
                );
                util::log(format!("SELFTEST SEC preview file/dir modes = {:o}/{:o}", modes.0, modes.1));
            });
        });
    });
    util::after(30.0, || {
        // Signed out, every Meet page redirects to Google's marketing site;
        // robots.txt stays a document of the meet.google.com origin.
        app::address_entered_in_new_tab("https://meet.google.com/robots.txt");
        util::after(7.0, || {
            web::eval_active("location.origin", |r| util::log(format!("SELFTEST SEC meet page origin: {r}")));
            web::eval_active(
                "(() => { navigator.mediaDevices.getUserMedia({audio: true, video: true}).then(s => { window.__zcTracks = s.getTracks().length; }, e => { window.__zcTracks = 'denied ' + e.name; }); return 'camera+mic request from meet.google.com'; })()",
                |r| util::log(format!("SELFTEST SEC {r}")),
            );
            util::after(3.0, || {
                web::eval_active("String(window.__zcTracks)", |r| util::log(format!("SELFTEST SEC meet tracks: {r}")));
                for u in web::recent_media_uses().iter().take(2) {
                    util::log(format!("SELFTEST SEC media decision: {} {} decision={}", u.origin, u.what, u.decision));
                }
                util::log(format!("SELFTEST SEC calls={:?} keeping awake={}", web::tabs_in_calls().len(), web::keeping_awake()));
                if let Some((_, tab, _, _)) = web::tabs_in_calls().first().copied() {
                    web::stop_media(tab);
                    util::after(1.5, || util::log(format!("SELFTEST SEC after Stop: calls={} keeping awake={}", web::tabs_in_calls().len(), web::keeping_awake())));
                }
            });
        });
    });
    util::after(46.0, || {
        report("security end");
        util::log("SELFTEST done");
        app::quit_without_asking();
    });
}

/// Walk the window's accessibility tree; log every control and flag any
/// without a name. Then drive the tab strip with the keyboard.
fn a11y_test() {
    util::after(2.0, || {
        app::add_client("A11y Test", "#2255aa", None, app::LoginChange::Keep, Default::default());
        app::address_entered("https://www.zoho.com/");
        util::after(1.0, || app::address_entered_in_new_tab("https://www.zoho.com/crm/"));
        util::after(2.0, || app::address_entered_in_new_tab("https://www.zoho.com/mail/"));
    });
    util::after(9.0, || {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let Some(window) = app.mainWindow().or_else(|| app.windows().iter().find(|w| w.isVisible())) else { return util::log("SELFTEST A11Y no window") };
        let mut out = vec![];
        let mut unlabeled = vec![];
        walk_ax(&window, 0, &mut out, &mut unlabeled);
        for line in &out {
            util::log(format!("SELFTEST A11Y {line}"));
        }
        util::log(format!("SELFTEST A11Y controls={} unlabeled={} {unlabeled:?}", out.len(), unlabeled.len()));
        ui::focus_tab_strip();
        util::after(0.5, || {
            util::log(format!("SELFTEST A11Y focus: {}", first_responder()));
            press("\u{F702}", 123, NSEventModifierFlags::empty());
            util::after(0.4, || {
                util::log(format!("SELFTEST A11Y after ←: {}", first_responder()));
                press("\r", 36, NSEventModifierFlags::empty());
                util::after(0.6, || {
                    util::log(format!("SELFTEST A11Y after Return: active tab index={:?}", app::store(|s| s.active_client().and_then(|c| c.active_index()))));
                    press("\u{F703}", 124, NSEventModifierFlags::Option | NSEventModifierFlags::Shift);
                    util::after(0.6, || {
                        util::log(format!(
                            "SELFTEST A11Y after ⌥⇧→ (move tab): tabs={:?}",
                            app::store(|s| s.active_client().map(|c| c.tabs.iter().map(|t| t.display_title()).collect::<Vec<_>>()))
                        ))
                    });
                });
            });
        });
    });
    util::after(14.0, || {
        util::log("SELFTEST done");
        app::quit_without_asking();
    });
}

fn first_responder() -> String {
    let mtm = MainThreadMarker::new().unwrap();
    NSApplication::sharedApplication(mtm)
        .mainWindow()
        .and_then(|w| w.firstResponder())
        .map(|r| format!("{} {:?}", r.class().name().to_string_lossy(), ax_string(&r, objc2::sel!(accessibilityLabel))))
        .unwrap_or_default()
}

fn responds(el: &objc2::runtime::AnyObject, sel: objc2::runtime::Sel) -> bool {
    unsafe { objc2::msg_send![el, respondsToSelector: sel] }
}

fn ax_attr(el: &objc2::runtime::AnyObject, name: &str) -> Option<objc2::rc::Retained<objc2::runtime::AnyObject>> {
    if !responds(el, objc2::sel!(accessibilityAttributeValue:)) {
        return None;
    }
    unsafe { objc2::msg_send![el, accessibilityAttributeValue: &*util::ns(name)] }
}

fn ax_string(el: &objc2::runtime::AnyObject, sel: objc2::runtime::Sel) -> Option<String> {
    if !responds(el, sel) {
        // Older accessibility objects (table rows) answer only the
        // attribute API.
        let attr = match sel.name().to_str().unwrap_or("") {
            "accessibilityRole" => "AXRole",
            "accessibilityLabel" => "AXDescription",
            "accessibilityTitle" => "AXTitle",
            "accessibilitySubrole" => "AXSubrole",
            _ => return None,
        };
        return ax_attr(el, attr).and_then(|v| v.downcast::<objc2_foundation::NSString>().ok()).map(|s| s.to_string()).filter(|s| !s.is_empty());
    }
    let v: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> = unsafe { objc2::msg_send![el, performSelector: sel] };
    v.and_then(|v| v.downcast::<objc2_foundation::NSString>().ok()).map(|s| s.to_string()).filter(|s| !s.is_empty())
}

fn walk_ax(el: &objc2::runtime::AnyObject, depth: usize, out: &mut Vec<String>, unlabeled: &mut Vec<String>) {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::sel;
    use objc2_foundation::NSArray;
    if depth > 14 {
        return;
    }
    let is_el: bool = (responds(el, sel!(isAccessibilityElement)) && unsafe { msg_send![el, isAccessibilityElement] })
        || (!responds(el, sel!(accessibilityRole)) && ax_attr(el, "AXRole").is_some());
    let role = ax_string(el, sel!(accessibilityRole)).unwrap_or_default();
    if std::env::var_os("ZCLIENTS_AX_ALL").is_some() && depth < 9 {
        out.push(format!("{}<{role}> el={is_el} {:?} {}", " ".repeat(depth), ax_string(el, sel!(accessibilityLabel)), el.class().name().to_string_lossy()));
    }
    if is_el && matches!(role.as_str(), "AXButton" | "AXRadioButton" | "AXCell" | "AXPopUpButton" | "AXTextField" | "AXCheckBox" | "AXTabGroup" | "AXSearchField" | "AXRow") {
        let name = ax_string(el, sel!(accessibilityLabel)).or_else(|| ax_string(el, sel!(accessibilityTitle)));
        let selected: bool = responds(el, sel!(isAccessibilitySelected)) && unsafe { msg_send![el, isAccessibilitySelected] };
        out.push(format!("{}{role} {:?}{}", " ".repeat(depth), name, if selected { " [selected]" } else { "" }));
        if name.is_none() {
            unlabeled.push(format!("{role}/{}", ax_string(el, sel!(accessibilitySubrole)).unwrap_or_default()));
        }
    }
    let kids: Option<Retained<NSArray<objc2::runtime::AnyObject>>> = if responds(el, sel!(accessibilityChildren)) {
        unsafe { msg_send![el, accessibilityChildren] }
    } else {
        ax_attr(el, "AXChildren").and_then(|k| k.downcast::<NSArray>().ok()).map(|a| unsafe { Retained::cast_unchecked(a) })
    };
    if let Some(kids) = kids {
        for k in kids.iter() {
            // Web content is WebKit's own tree; stop at the web view.
            let class = k.class().name().to_string_lossy().into_owned();
            if class.contains("WKWebView") || class.contains("ZWebView") {
                out.push(format!("{}(web content)", " ".repeat(depth + 1)));
                continue;
            }
            walk_ax(&k, depth + 1, out, unlabeled);
        }
    }
}

/// A large file on a public speed-test server, so a download is still
/// running when the test quits.
const SLOW_DOWNLOAD: &str = "https://proof.ovh.net/files/1Gb.dat";

/// Crash a tab's page the way a real crash looks to WebKit: SIGKILL its
/// WebContent process (pid from the private `_webProcessIdentifier`).
fn kill_web_content(tab: uuid::Uuid) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    web::live_view(tab).is_some_and(|v| unsafe {
        let responds: bool = objc2::msg_send![&*v, respondsToSelector: objc2::sel!(_webProcessIdentifier)];
        if !responds {
            return false;
        }
        let pid: i32 = objc2::msg_send![&*v, _webProcessIdentifier];
        pid > 0 && kill(pid, 9) == 0
    })
}

/// `ZCLIENTS_SELFTEST=stability` with ZCLIENTS_MOCK_MEDIA=1 and
/// ZCLIENTS_ALERTS=decline (or =accept for the quit step):
/// U6 closing a tab in a call asks; U1 a download keeps its key when a new
/// one arrives; S1 quitting with a download and a call asks, and on Quit
/// the partial file is deleted (checked by the caller after exit); S2 a
/// page killed twice within a minute shows "stopped working" instead of
/// reloading again.
fn stability_test() {
    let accept = std::env::var("ZCLIENTS_ALERTS").as_deref() == Ok("accept");
    util::after(1.0, || {
        app::add_client("Stability Test", "#2470dd", None, app::LoginChange::Keep, Default::default());
        app::address_entered(PERF_PAGES[0]);
    });
    util::after(12.0, move || {
        let Some((client, call_tab)) = app::store(|s| s.active_tab().map(|(c, t)| (c.id, t.id))) else { return };
        web::eval_in_tab_async(call_tab, CALL_START, |r| util::log(format!("SELFTEST STAB call: {r}")));
        util::after(4.0, move || {
            util::log(format!("SELFTEST STAB U6 in call before close: {:?}", web::call_state(call_tab)));
            app::request_close_tab(client, call_tab);
            util::after(0.5, move || {
                let open = app::store(|s| s.find_tab(call_tab).is_some());
                util::log(format!("SELFTEST STAB U6 tab still open after close was {}: {open} (expected {})", if accept { "accepted" } else { "declined" }, !accept));
            });
        });
        util::after(6.0, move || {
            web::download_url(Some(client), SLOW_DOWNLOAD);
            app::address_entered_in_new_tab(PERF_PAGES[1]);
        });
        util::after(11.0, move || {
            let slow = downloads::with_items(|v| v.iter().find(|i| i.state == downloads::State::Downloading).map(|i| (i.key(), i.name.clone())));
            util::log(format!("SELFTEST STAB slow download running: {slow:?}"));
            let Some((key, name)) = slow else { return };
            downloads::save_bytes(b"inserted while the menu was open".to_vec(), "u1-newer.txt", None);
            let still = downloads::path_of(key).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let top = downloads::with_items(|v| v.first().map(|i| i.name.clone()));
            util::log(format!("SELFTEST STAB U1 newest item {top:?}; key of {name:?} still names {still:?} (ok={})", still.as_deref() == Some(name.as_str())));
            if !accept {
                let asked = matches!(app::should_quit(), app::QuitReply::Cancel);
                util::log(format!(
                    "SELFTEST STAB S1 quit with a download and a call asked and was cancelled: {asked}; still downloading: {}; call: {:?}",
                    downloads::active_count(),
                    web::call_state(call_tab)
                ));
            }
        });
        // S2: the tab in front (the second page) crashes twice.
        util::after(14.0, || {
            let Some(tab) = app::store(|s| s.active_tab().map(|(_, t)| t.id)) else { return };
            util::log(format!("SELFTEST STAB S2 first kill: {}", kill_web_content(tab)));
            util::after(8.0, move || {
                util::log(format!("SELFTEST STAB S2 after first kill: error={:?} loading/url={:?}", ui::load_error_of(tab), web::page_state(tab).map(|p| p.url)));
                util::log(format!("SELFTEST STAB S2 second kill: {}", kill_web_content(tab)));
                util::after(3.0, move || util::log(format!("SELFTEST STAB S2 after second kill: error={:?} (expected Some(Crashed))", ui::load_error_of(tab))));
            });
        });
        util::after(28.0, move || {
            let partial = downloads::with_items(|v| v.iter().find(|i| i.state == downloads::State::Downloading).and_then(|i| i.path.clone()));
            util::log(format!("SELFTEST STAB S1 partial file before quit: {partial:?} exists={}", partial.as_ref().is_some_and(|p| p.exists())));
            util::log("SELFTEST done");
            if accept {
                // Through the real path: asks, cancels, deletes, exits.
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
            } else {
                for p in downloads::cancel_all() {
                    util::log(format!("SELFTEST STAB cancelled; partial left: {}", p.exists()));
                }
                app::quit_without_asking();
            }
        });
    });
}

fn front() -> String {
    app::store(|s| s.active_tab().map(|(c, t)| format!("client={:?} workspace={:?} url={:?}", c.name, t.effective_workspace(), t.url))).unwrap_or_default()
}

/// `ZCLIENTS_SELFTEST=search`: the Google Search workspace keeps a Zoho
/// page in place, blocks an outside site (by script and typed), and sends
/// Meet to a client.
fn search_workspace_test() {
    util::after(1.0, || {
        app::add_client("Search Test", "#2470dd", None, app::LoginChange::Keep, Default::default());
        app::open_search("zoho crm");
    });
    util::after(9.0, || {
        util::log(format!("SELFTEST SEARCH on results: {}", front()));
        app::address_entered("https://www.zoho.com/crm/");
    });
    util::after(18.0, || {
        util::log(format!("SELFTEST SEARCH Zoho page stays in Search (expected client \"Google Search\", GoogleSearch): {}", front()));
        web::eval_active("(() => { location.href = 'https://example.com/'; return 'navigating'; })()", |r| util::log(format!("SELFTEST SEARCH script to example.com: {r}")));
    });
    util::after(22.0, || {
        let tab = app::store(|s| s.active_tab().map(|(_, t)| t.id));
        let page = tab.and_then(web::page_state).map(|p| p.url);
        util::log(format!("SELFTEST SEARCH after script: error={:?} (expected BlockedInSearch) page still {page:?}", tab.and_then(ui::load_error_of)));
        if let Some(t) = tab {
            ui::clear_load_error(t);
        }
        app::address_entered("example.com");
    });
    util::after(24.0, || {
        let tab = app::store(|s| s.active_tab().map(|(_, t)| t.id));
        util::log(format!("SELFTEST SEARCH typed example.com: error={:?} {}", tab.and_then(ui::load_error_of), front()));
        if let Some(t) = tab {
            ui::clear_load_error(t);
        }
        web::eval_active("(() => { location.href = 'https://meet.google.com/'; return 'to meet'; })()", |r| util::log(format!("SELFTEST SEARCH script to Meet: {r}")));
    });
    util::after(30.0, || {
        util::log(format!("SELFTEST SEARCH Meet went to a client (expected \"Search Test\", Zoho): {}", front()));
        util::log("SELFTEST done");
        app::quit_without_asking();
    });
}

/// A point in the active page (CSS pixels from its top-left corner) in
/// global display coordinates (origin top-left of the main display), as
/// CGEvent and System Events use them.
fn page_point_on_screen(x: f64, y: f64) -> (f64, f64) {
    let Some(view) = web::active_view() else { return (0.0, 0.0) };
    let b = view.bounds();
    let local = NSPoint::new(x, if view.isFlipped() { y } else { b.size.height - y });
    let in_window = view.convertPoint_toView(local, None);
    let Some(window) = view.window() else { return (0.0, 0.0) };
    let p = window.convertPointToScreen(in_window);
    let main_height = objc2_app_kit::NSScreen::screens(MainThreadMarker::new().unwrap()).firstObject().map(|s| s.frame().size.height).unwrap_or(0.0);
    (p.x, main_height - p.y)
}

/// The page used by `parity`: a button that opens a right-side panel
/// (transform transition) and, from the same click, a width transition, a
/// keyframe animation, a Web Animations call, a rAF tween and a panel shown
/// from requestIdleCallback. Every time is measured from the click.
const PARITY_PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>Parity</title>
<style>
#b{position:fixed;top:120px;left:40px;width:160px;height:40px}
#p{position:fixed;top:0;right:0;width:300px;height:100%;background:#ccc;transform:translateX(100%);transition:transform .3s ease}
#p.open{transform:none}
#w{position:fixed;top:0;left:0;width:0;height:50px;background:red;transition:width .3s}
#w.open{width:300px}
@keyframes k{from{opacity:0}to{opacity:1}}
#k{position:fixed;top:60px;left:40px;opacity:0}
#k.run{animation:k .3s 1 forwards}
#t{position:fixed;top:200px;right:-200px;width:200px;height:40px;background:blue}
#i{position:fixed;top:260px;right:0;width:200px;height:40px;background:green;display:none}
</style>
<button id=b>Settings</button><div id=p></div><div id=w></div><div id=k>k</div><div id=t></div><div id=i></div>
<script>
var R = {clicks: 0, downs: 0, blurred: false};
var since = function () { return Math.round(performance.now() - R.t0); };
addEventListener('blur', function () { R.blurred = true; });
b.addEventListener('mousedown', function () { R.downs++; });
k.addEventListener('animationend', function () { R.animationend = since(); });
p.addEventListener('transitionend', function (e) { if (e.propertyName === 'transform') { R.transitionend = since(); } });
w.addEventListener('transitionend', function () { R.widthTransitionend = since(); });
b.addEventListener('click', function () {
  if (++R.clicks > 1) { return; }
  R.t0 = performance.now();
  p.classList.add('open'); w.classList.add('open'); k.classList.add('run');
  document.body.animate([{opacity: 1}, {opacity: .99}], {duration: 300}).finished.then(function () { R.waapi = since(); });
  var frames = 0, start = null;
  requestAnimationFrame(function loop(now) {
    if (start === null) { start = now; }
    frames++;
    var f = Math.min(1, (now - start) / 300);
    t.style.right = (-200 + 200 * f) + 'px';
    if (f >= 1 && R.tween === undefined) { R.tween = since(); }
    if (now - start < 1000) { requestAnimationFrame(loop); } else { R.rafPerSec = Math.round(frames * 1000 / (now - start)); }
  });
  if (window.requestIdleCallback) { requestIdleCallback(function () { i.style.display = 'block'; R.idle = since(); }); } else { R.idle = 'unsupported'; }
});
</script>"#;

const PARITY_RESULT: &str =
    "JSON.stringify(Object.assign({}, R, {panel: getComputedStyle(p).transform, tweenRight: t.style.right, hasFocus: document.hasFocus(), visibility: document.visibilityState}))";

/// The parity page's result against the rules: every animation finished
/// within a second of exactly one click, the panel ended open, frames
/// arrived at ≥ 25 a second (≥ 55 outside Low Power Mode) and the page kept
/// the focus. Returns the failures (empty = pass).
fn parity_failures(r: &serde_json::Value, low_power: bool) -> Vec<String> {
    let mut bad = vec![];
    let num = |k: &str| r.get(k).and_then(|v| v.as_f64());
    if r.get("clicks").and_then(|v| v.as_u64()) != Some(1) {
        bad.push(format!("clicks={}", r.get("clicks").unwrap_or(&serde_json::Value::Null)));
    }
    for k in ["transitionend", "widthTransitionend", "animationend", "waapi", "tween"] {
        match num(k) {
            Some(ms) if ms <= 1000.0 => {}
            other => bad.push(format!("{k}={other:?}")),
        }
    }
    if r.get("panel").and_then(|v| v.as_str()) != Some("none") {
        bad.push(format!("panel transform={}", r.get("panel").unwrap_or(&serde_json::Value::Null)));
    }
    let min_fps = if low_power { 25.0 } else { 55.0 };
    if num("rafPerSec").is_none_or(|f| f < min_fps) {
        bad.push(format!("rafPerSec={:?} (need ≥ {min_fps})", num("rafPerSec")));
    }
    if r.get("hasFocus").and_then(|v| v.as_bool()) != Some(true) {
        bad.push("page lost focus".into());
    }
    if r.get("blurred").and_then(|v| v.as_bool()) != Some(false) {
        bad.push("blur fired".into());
    }
    bad
}

/// Where the parity page's button is, in CSS pixels (see `PARITY_PAGE`).
const PARITY_BUTTON: (f64, f64) = (120.0, 140.0);

fn parity_round(label: &'static str, inactive: bool, then: impl FnOnce() + 'static) {
    web::load_test_html(PARITY_PAGE, "https://www.zoho.com/");
    util::after(2.0, move || {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        if inactive {
            // Another app in front, as when the user comes from Cliq or
            // Chrome and clicks straight into a page.
            if let Some(finder) = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(&util::ns("com.apple.finder")).firstObject() {
                app.yieldActivationToApplication(&finder);
                finder.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::empty());
            }
            app.deactivate();
        }
        util::after(1.0, move || {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            let key = app.mainWindow().is_some_and(|w| w.isKeyWindow());
            util::log(format!("SELFTEST PARITY {label}: before the click app active={} window key={key}", app.isActive()));
            // The blur from losing the window is not what is being measured.
            web::eval_active("(() => { R.blurred = false; return 'ready'; })()", |_| {});
            // ZCLIENTS_REAL_CLICK=1: the click comes from outside (a real
            // mouse event posted at the logged screen point), which is the
            // only way the window server activates the app as it would for
            // the user; a click sent through NSApp.sendEvent never does.
            let real = inactive && std::env::var_os("ZCLIENTS_REAL_CLICK").is_some();
            if real {
                let (x, y) = page_point_on_screen(PARITY_BUTTON.0, PARITY_BUTTON.1);
                util::log(format!("SELFTEST PARITY real click at {x:.0} {y:.0}"));
            }
            util::after(if real { 4.0 } else { 0.2 }, move || {
                if !real {
                    click_page(PARITY_BUTTON.0, PARITY_BUTTON.1, NSEventModifierFlags::empty());
                }
                util::after(1.5, move || {
                    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
                    util::log(format!("SELFTEST PARITY {label}: after the click app active={}", app.isActive()));
                    web::eval_active(PARITY_RESULT, move |r| {
                        let low_power = objc2_foundation::NSProcessInfo::processInfo().isLowPowerModeEnabled();
                        let fails = serde_json::from_str(&r).map(|v| parity_failures(&v, low_power)).unwrap_or_else(|e| vec![format!("no result ({e}): {r}")]);
                        let verdict = if fails.is_empty() { "PASS".to_string() } else { format!("FAIL {fails:?}") };
                        util::log(format!("SELFTEST PARITY {label}: {verdict} low power={low_power} {r}"));
                    });
                    util::after(0.5, then);
                });
            });
        });
    });
}

/// `ZCLIENTS_SELFTEST=parity`: in-page UI behaves like Chrome. A real click
/// (down + up through `NSApp.sendEvent`) opens a sliding panel, with the
/// app active and again with another app in front; see `parity_failures`.
fn parity_test() {
    util::after(1.0, || {
        app::add_client("Parity Test", "#2470dd", None, app::LoginChange::Keep, Default::default());
        app::address_entered("https://www.zoho.com/");
    });
    util::after(9.0, || {
        parity_round("active window", false, || {
            parity_round("another app in front", true, || {
                util::log("SELFTEST done");
                app::quit_without_asking();
            })
        })
    });
}

const PRINT_PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>Print</title>
<button id=b style="position:fixed;top:120px;left:40px;width:160px;height:40px" onclick="window.print()">Print</button>
<h1 style="margin-top:200px">Printable report</h1>
<iframe id=f style="width:300px;height:100px" srcdoc="<p>Frame to print</p>"></iframe>"#;

/// The print panel sheet on the main window, if any: logged, then cancelled.
fn take_print_sheet(label: &str) {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let window = app.mainWindow().or_else(|| app.windows().iter().find(|w| w.isVisible()));
    let sheet = window.as_ref().and_then(|w| w.attachedSheet());
    let what = sheet.as_ref().map(|s| {
        let content = s.contentView().map(|v| v.subviews().iter().map(|k| k.class().name().to_string_lossy().into_owned()).collect::<Vec<_>>());
        format!("{} title={:?} content={content:?}", s.class().name().to_string_lossy(), s.title().to_string())
    });
    util::log(format!("SELFTEST PRINT {label}: sheet={what:?}"));
    if let (Some(w), Some(s)) = (window, sheet) {
        w.endSheet_returnCode(&s, objc2_app_kit::NSModalResponseCancel);
    }
}

/// `ZCLIENTS_SELFTEST=print`: window.print() from a page's Print button
/// and from an iframe shows the print panel as a sheet (then cancelled).
fn print_test() {
    util::after(1.0, || {
        app::add_client("Print Test", "#2470dd", None, app::LoginChange::Keep, Default::default());
        app::address_entered("https://www.zoho.com/");
    });
    util::after(9.0, || web::load_test_html(PRINT_PAGE, "https://www.zoho.com/"));
    util::after(11.0, || click_page(120.0, 140.0, NSEventModifierFlags::empty()));
    util::after(13.0, || take_print_sheet("Print button (window.print)"));
    util::after(15.0, || eval_log("PRINT iframe", "(() => { document.getElementById('f').contentWindow.print(); return 'iframe print called'; })()"));
    util::after(17.0, || take_print_sheet("iframe print"));
    util::after(19.0, || {
        util::log("SELFTEST done");
        app::quit_without_asking();
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
        Ok("address") => return address_test(),
        Ok("security") => return security_test(),
        Ok("address2") => return address_rules_test(),
        Ok("editor") => return editor_test(),
        Ok("a11y") => return a11y_test(),
        Ok("stability") => return stability_test(),
        Ok("search") => return search_workspace_test(),
        Ok("parity") => return parity_test(),
        Ok("print") => return print_test(),
        Ok(_) => {}
        Err(_) => return,
    }
    let cmd = NSEventModifierFlags::Command;
    util::after(7.0, move || {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let menu = app.mainMenu();
        util::log(format!(
            "SELFTEST active={} key window={} main menu={} items={}",
            app.isActive(),
            app.keyWindow().is_some(),
            menu.is_some(),
            menu.as_ref().map(|m| m.numberOfItems()).unwrap_or(0)
        ));
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
                NSEventType::KeyDown,
                NSPoint::new(0.0, 0.0),
                cmd,
                0.0,
                0,
                None,
                &util::ns("s"),
                &util::ns("s"),
                false,
                1,
            )
            .unwrap();
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
        web::eval_active(
            "(() => { const f = document.getElementById('zc-test') || Object.assign(document.body.appendChild(document.createElement('textarea')), {id: 'zc-test'}); f.value=''; f.focus(); return 'focused ' + (document.activeElement === f); })()",
            |r| {
                util::log(format!("SELFTEST web field: {r}"));
            },
        );
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
        app::quit_without_asking();
    });
}

#[cfg(test)]
mod tests {
    use super::parity_failures;

    #[test]
    fn parity_passes_only_one_click_that_finished_every_animation() {
        let good = serde_json::json!({"clicks": 1, "blurred": false, "transitionend": 320, "widthTransitionend": 320, "animationend": 320, "waapi": 320, "tween": 330, "rafPerSec": 31, "panel": "none", "hasFocus": true});
        assert!(parity_failures(&good, true).is_empty());
        assert_eq!(parity_failures(&good, false), vec!["rafPerSec=Some(31.0) (need ≥ 55)"], "30 fps only passes in Low Power Mode");
        let swallowed = serde_json::json!({"clicks": 0, "blurred": false, "panel": "matrix(1, 0, 0, 1, 300, 0)", "hasFocus": true});
        let fails = parity_failures(&swallowed, true);
        assert!(fails.contains(&"clicks=0".to_string()) && fails.iter().any(|f| f.starts_with("panel transform")));
        let mut slow = good.clone();
        slow["transitionend"] = 1200.into();
        slow["blurred"] = true.into();
        assert_eq!(parity_failures(&slow, true), vec!["transitionend=Some(1200.0)", "blur fired"]);
    }
}
