//! The Email workspace's new-tab page: pick a webmail (Gmail, Outlook, Zoho
//! Mail...) or type any email web address. The choice loads in that tab,
//! with the Email workspace's own sign-ins.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSAccessibility, NSAutoresizingMaskOptions, NSBezelStyle, NSButton, NSResponder, NSTextField, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use url::Url;

use crate::{app, policy, theme, ui, util};

/// (button title, address). Outlook has a work/school and a personal home.
pub const PROVIDERS: &[(&str, &str)] = &[
    ("Gmail", "https://mail.google.com/"),
    ("Outlook (work or school)", "https://outlook.office.com/mail/"),
    ("Outlook.com", "https://outlook.live.com/mail/"),
    ("Zoho Mail", "https://mail.zoho.com/"),
    ("Yahoo Mail", "https://mail.yahoo.com/"),
    ("iCloud Mail", "https://www.icloud.com/mail/"),
];

const WIDTH: f64 = 460.0;
const BUTTON_H: f64 = 32.0;
const GAP: f64 = 10.0;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

define_class!(
    /// The column of controls, laid out top-down.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    struct MailColumn;

    impl MailColumn {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    /// Flipped, and keeps its column centred as the window resizes.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    struct MailPage;

    impl MailPage {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(layout))]
        fn do_layout(&self) {
            unsafe {
                let _: () = msg_send![super(self), layout];
            }
            let w = self.bounds().size.width;
            if let Some(column) = self.subviews().firstObject() {
                let f = column.frame();
                column.setFrameOrigin(NSPoint::new(((w - f.size.width) / 2.0).max(16.0).floor(), 72.0));
            }
        }

        #[unsafe(method(zcPickMail:))]
        fn pick_mail(&self, sender: Option<&NSButton>) {
            let Some((_, address)) = sender.and_then(|b| PROVIDERS.get(b.tag() as usize)) else { return };
            if let Some(u) = policy::parse(address) {
                open(u);
            }
        }

        #[unsafe(method(zcTypedMail:))]
        fn typed_mail(&self, sender: Option<&AnyObject>) {
            let field = sender.and_then(|s| s.downcast_ref::<NSTextField>()).map(|f| f.stringValue().to_string());
            let field = field.or_else(|| field_text(self));
            if let Some(u) = field.as_deref().and_then(typed_address) {
                open(u);
            } else {
                beep();
            }
        }
    }
);

fn beep() {
    unsafe extern "C" {
        fn NSBeep();
    }
    // SAFETY: AppKit's alert sound, main thread.
    unsafe { NSBeep() };
}

fn field_text(page: &MailPage) -> Option<String> {
    let column = page.subviews().firstObject()?;
    column.subviews().iter().find_map(|v| v.downcast::<NSTextField>().ok().filter(|f| f.isEditable()).map(|f| f.stringValue().to_string()))
}

/// What the address box accepts: a web address (https added when left out).
pub fn typed_address(text: &str) -> Option<Url> {
    policy::normalized_http_url(text).filter(|u| u.host_str().is_some_and(|h| h.contains('.')))
}

fn open(url: Url) {
    util::later(move || app::open_in_active_tab(url));
}

pub fn build() -> Retained<NSView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let page: Retained<MailPage> = unsafe { msg_send![MailPage::alloc(mtm), initWithFrame: rect(0.0, 0.0, 800.0, 600.0)] };
    let column: Retained<MailColumn> = unsafe { msg_send![MailColumn::alloc(mtm), initWithFrame: rect(0.0, 0.0, WIDTH, 10.0)] };

    let mut y = 0.0;
    let title = ui::text("Open your email", theme::Text::LargeTitle, theme::Weight::Bold);
    title.setFrame(rect(0.0, 0.0, WIDTH, 40.0));
    column.addSubview(&title);
    y += 44.0;
    let hint = ui::text("Your work email, signed in separately from every client. Links in your messages open here, any website.", theme::Text::Body, theme::Weight::Regular);
    hint.setTextColor(Some(&theme::secondary_text()));
    hint.setFrame(rect(0.0, y, WIDTH, 40.0));
    if let Some(cell) = hint.cell() {
        cell.setWraps(true);
    }
    column.addSubview(&hint);
    y += 52.0;

    let half = (WIDTH - GAP) / 2.0;
    for (i, (name, _)) in PROVIDERS.iter().enumerate() {
        let b = unsafe { NSButton::buttonWithTitle_target_action(&util::ns(name), Some(&page), Some(sel!(zcPickMail:)), mtm) };
        b.setBezelStyle(NSBezelStyle::Push);
        b.setTag(i as isize);
        let x = if i % 2 == 0 { 0.0 } else { half + GAP };
        b.setFrame(rect(x, y, half, BUTTON_H));
        b.setToolTip(Some(&util::ns(PROVIDERS[i].1)));
        column.addSubview(&b);
        if i % 2 == 1 {
            y += BUTTON_H + GAP;
        }
    }
    if PROVIDERS.len() % 2 == 1 {
        y += BUTTON_H + GAP;
    }
    y += 14.0;

    let other = ui::text("Another email service: type its web address", theme::Text::Footnote, theme::Weight::Regular);
    other.setTextColor(Some(&theme::secondary_text()));
    other.setFrame(rect(0.0, y, WIDTH, 18.0));
    column.addSubview(&other);
    y += 22.0;
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, y, WIDTH - 90.0, 26.0));
    field.setPlaceholderString(Some(&util::ns("mail.example.com")));
    unsafe {
        field.setTarget(Some(&page));
        field.setAction(Some(sel!(zcTypedMail:)));
    }
    field.setAccessibilityLabel(Some(&util::ns("Email web address")));
    column.addSubview(&field);
    let go = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Open"), Some(&page), Some(sel!(zcTypedMail:)), mtm) };
    go.setBezelStyle(NSBezelStyle::Push);
    go.setFrame(rect(WIDTH - 80.0, y - 2.0, 80.0, 30.0));
    column.addSubview(&go);
    y += 30.0;

    column.setFrame(rect(0.0, 72.0, WIDTH, y));
    page.addSubview(&column);
    page.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    Retained::into_super(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_are_https_and_the_box_takes_bare_addresses() {
        for (name, address) in PROVIDERS {
            let u = policy::parse(address).unwrap();
            assert!(policy::is_https(&u), "{name}");
            assert!(policy::is_allowed_web(&u, crate::model::Workspace::Mail));
        }
        assert_eq!(typed_address("mail.example.com").map(|u| u.to_string()), Some("https://mail.example.com/".into()));
        assert!(typed_address("hello").is_none());
        assert!(typed_address("").is_none());
    }
}
