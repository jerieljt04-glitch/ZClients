//! Keyboard self-test for "ZClients Test" builds only
//! (`ZCLIENTS_SELFTEST=1`). Sends real key events through `NSApp.sendEvent`
//! — the same path a key press takes — and logs what each one did.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSPoint;

use crate::{app, ui, util, web};

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

pub fn start() {
    if !util::is_test_build() {
        return;
    }
    match std::env::var("ZCLIENTS_SELFTEST").as_deref() {
        Ok("prep") => return prep(),
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
