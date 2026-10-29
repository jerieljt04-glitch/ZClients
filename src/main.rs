//! ZClients — one Mac app for many signed-in Zoho profiles.
//!
//! Native AppKit + WebKit, driven from Rust through the objc2 bindings.

mod app;
mod apps;
mod cleanup;
mod downloads;
mod keychain;
mod model;
mod pdf;
mod picker;
mod policy;
mod privacy;
#[cfg(test)]
mod proptests;
mod scripts;
mod selftest;
mod session;
mod settings;
mod suggest;
mod switcher;
mod tabstate;
mod theme;
mod ui;
mod util;
mod web;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            let app = NSApplication::sharedApplication(self.mtm());
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            app::load();
            // The window goes up first: restoring the session reads the
            // vault key, and after an update macOS asks for Keychain access
            // right then. Asked before the window existed, the prompt sat
            // over an empty screen. `ui::build` starts the restore on the
            // next turn, before the first page is created, and that page
            // still waits for it.
            ui::build();
            app::start_background_work();
            selftest::start();
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }

        // A recent snapshot of every session and tab for an abrupt quit to
        // fall back on.
        #[unsafe(method(applicationDidResignActive:))]
        fn did_resign_active(&self, _n: &NSNotification) {
            app::app_resigned_active();
        }

        // Quitting (⌘Q, logging out) while a file is
        // downloading or a call is on asks first; see `app::should_quit`.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> objc2_app_kit::NSApplicationTerminateReply {
            use objc2_app_kit::NSApplicationTerminateReply as R;
            match app::should_quit() {
                app::QuitReply::Now => R::TerminateNow,
                app::QuitReply::Cancel => R::TerminateCancel,
                app::QuitReply::Later => R::TerminateLater,
            }
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            app::app_will_quit();
        }

        // Closing the window only hides it (see `ui` windowShouldClose).
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            false
        }

        // Clicking the Dock icon with the window hidden brings it back.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, visible: bool) -> bool {
            if !visible || !ui::window_visible() {
                ui::show_window();
            }
            true
        }

        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        fn secure_restorable(&self, _app: &NSApplication) -> bool {
            true
        }
    }
);

fn main() {
    util::mark_start();
    // `ZClients --diagnose`: print what macOS currently allows this app.
    if std::env::args().any(|a| a == "--diagnose") {
        let b = objc2_app_kit::NSPasteboard::generalPasteboard().accessBehavior();
        let name = match b.0 {
            0 => "default (never asked)",
            1 => "ask",
            2 => "always allow",
            3 => "always deny",
            _ => "unknown",
        };
        println!("paste from other apps: {name} ({})", b.0);
        return;
    }
    let mtm = MainThreadMarker::new().expect("must start on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let delegate: Retained<AppDelegate> = {
        let this = AppDelegate::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
