//! The new-tab page: a searchable dropdown of every Zoho app.
//!
//! Focusing the field opens the list (grouped by category). Typing filters it
//! with the forgiving search in `apps.rs`; ↑/↓ move, ↩ opens, Esc closes.
//! If nothing matches, ↩ hands the text to the address bar instead.
#![allow(unused_unsafe)]

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSControl, NSControlTextEditingDelegate, NSEvent, NSFont, NSResponder, NSScrollView, NSSearchField, NSSearchFieldDelegate, NSTableColumn, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewStyle, NSTextField, NSTextFieldDelegate, NSTextView, NSView,
};
use objc2_foundation::{NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::apps::{self, ZOHO_APPS};
use crate::{app, ui, util};

const HEADER_H: f64 = 26.0;
const ROW_H: f64 = 40.0;
const MAX_LIST_H: f64 = 460.0;

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Header(&'static str),
    App(usize),
    /// "No app matches" — ↩ searches instead.
    Nothing,
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

define_class!(
    /// The page itself: flipped, lays out its controls, and a click on empty
    /// space closes the dropdown.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct PickerPage;

    impl PickerPage {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(layout))]
        fn do_layout(&self) {
            unsafe {
                let _: () = msg_send![super(self), layout];
            }
            layout(self);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            set_open(false);
        }
    }
);

define_class!(
    /// The search field; gaining focus opens the dropdown.
    #[unsafe(super(NSSearchField, NSTextField, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct PickerField;

    impl PickerField {
        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                util::later(|| set_open(true));
            }
            ok
        }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct PickerCtl;

    unsafe impl NSObjectProtocol for PickerCtl {}

    impl PickerCtl {
        #[unsafe(method(toggle:))]
        fn toggle(&self, _s: Option<&AnyObject>) {
            let open = with(|p| p.open.get()).unwrap_or(false);
            set_open(!open);
            if !open {
                focus();
            }
        }

        #[unsafe(method(rowClicked:))]
        fn row_clicked(&self, _s: Option<&AnyObject>) {
            let row = with(|p| p.table.clickedRow()).unwrap_or(-1);
            if row >= 0 {
                commit(Some(row as usize));
            }
        }
    }

    unsafe impl NSControlTextEditingDelegate for PickerCtl {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _n: &NSNotification) {
            let q = with(|p| p.field.stringValue().to_string()).unwrap_or_default();
            filter(&q);
            set_open(true);
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _control: &NSControl, _tv: &NSTextView, cmd: Sel) -> bool {
            handle_command(cmd)
        }
    }

    unsafe impl NSTextFieldDelegate for PickerCtl {}
    unsafe impl NSSearchFieldDelegate for PickerCtl {}

    unsafe impl NSTableViewDataSource for PickerCtl {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn rows(&self, _t: &NSTableView) -> NSInteger {
            with(|p| p.rows.borrow().len() as NSInteger).unwrap_or(0)
        }
    }

    unsafe impl NSTableViewDelegate for PickerCtl {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(&self, table: &NSTableView, _c: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            row_view(table, row)
        }

        #[unsafe(method(tableView:heightOfRow:))]
        fn height(&self, _t: &NSTableView, row: NSInteger) -> f64 {
            match row_at(row) {
                Some(Row::Header(_)) => HEADER_H,
                _ => ROW_H,
            }
        }

        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group(&self, _t: &NSTableView, row: NSInteger) -> bool {
            matches!(row_at(row), Some(Row::Header(_)))
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select(&self, _t: &NSTableView, row: NSInteger) -> bool {
            matches!(row_at(row), Some(Row::App(_)))
        }
    }
);

struct Picker {
    page: Retained<PickerPage>,
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
    field: Retained<PickerField>,
    chevron: Retained<NSButton>,
    hint: Retained<NSTextField>,
    panel: Retained<NSView>,
    scroll: Retained<NSScrollView>,
    table: Retained<NSTableView>,
    _ctl: Retained<PickerCtl>,
    rows: RefCell<Vec<Row>>,
    open: Cell<bool>,
}

thread_local! {
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&Picker) -> R) -> Option<R> {
    PICKER.with_borrow(|p| p.as_ref().map(f))
}

fn row_at(row: NSInteger) -> Option<Row> {
    (row >= 0).then(|| with(|p| p.rows.borrow().get(row as usize).copied()).flatten()).flatten()
}

/// Build the new-tab page for the profile called `client_name`.
pub fn build(client_name: &str) -> Retained<NSView> {
    let mtm = mtm();
    let ctl: Retained<PickerCtl> = {
        let this = PickerCtl::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
    let target: &AnyObject = &ctl;

    let page: Retained<PickerPage> = {
        let this = PickerPage::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 800.0, 600.0)] }
    };

    let title = ui::label("Open a Zoho app", 24.0, true);
    title.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    let subtitle = ui::label(&format!("Opens signed in as “{client_name}”."), 13.0, false);
    subtitle.setTextColor(Some(&NSColor::secondaryLabelColor()));
    subtitle.setAlignment(objc2_app_kit::NSTextAlignment::Center);

    let field: Retained<PickerField> = {
        let this = PickerField::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 400.0, 32.0)] }
    };
    field.setPlaceholderString(Some(&util::ns("Choose or type a Zoho app — CRM, Books, invoice, chat…")));
    field.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    field.setSendsSearchStringImmediately(true);
    unsafe { field.setDelegate(Some(ProtocolObject::from_ref(&*ctl))) };

    let chevron = ui::sym_button("chevron.down", "Show all Zoho apps", sel!(toggle:));
    unsafe { chevron.setTarget(Some(target)) };

    let hint = ui::label(&format!("{} Zoho apps  ·  ↑ ↓ to choose  ·  ↩ to open  ·  Esc to close", ZOHO_APPS.len()), 11.5, false);
    hint.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    hint.setAlignment(objc2_app_kit::NSTextAlignment::Center);

    let panel = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    panel.setWantsLayer(true);
    if let Some(layer) = panel.layer() {
        layer.setBackgroundColor(Some(&NSColor::controlBackgroundColor().CGColor()));
        layer.setCornerRadius(12.0);
        layer.setBorderWidth(1.0);
        layer.setBorderColor(Some(&NSColor::separatorColor().CGColor()));
        layer.setMasksToBounds(true);
    }

    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &util::ns("app"));
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setStyle(NSTableViewStyle::Inset);
    table.setBackgroundColor(&NSColor::clearColor());
    table.setColumnAutoresizingStyle(objc2_app_kit::NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle);
    table.setRefusesFirstResponder(true);
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*ctl)));
        table.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
        table.setTarget(Some(target));
        table.setAction(Some(sel!(rowClicked:)));
    }
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    scroll.setDocumentView(Some(&table));
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    panel.addSubview(&scroll);

    for v in [&*title as &NSView, &subtitle, &field, &chevron, &hint, &panel] {
        page.addSubview(v);
    }
    panel.setHidden(true);

    let picker = Picker { page: page.clone(), title, subtitle, field, chevron, hint, panel, scroll, table, _ctl: ctl, rows: RefCell::new(vec![]), open: Cell::new(false) };
    PICKER.with_borrow_mut(|p| *p = Some(picker));
    filter("");
    Retained::into_super(page)
}

/// Put the cursor in the search field, which opens the list.
pub fn focus() {
    with(|p| {
        if let Some(w) = p.page.window() {
            w.makeFirstResponder(Some(&p.field));
        }
    });
}

pub fn is_showing() -> bool {
    with(|p| p.page.window().is_some()).unwrap_or(false)
}

fn filter(query: &str) {
    let q = query.trim();
    let mut rows = vec![];
    if q.is_empty() {
        let mut last = "";
        for (i, a) in ZOHO_APPS.iter().enumerate() {
            if a.category != last {
                rows.push(Row::Header(a.category));
                last = a.category;
            }
            rows.push(Row::App(i));
        }
    } else {
        rows.extend(apps::search(q).into_iter().map(Row::App));
        if rows.is_empty() {
            rows.push(Row::Nothing);
        }
    }
    let first_app = rows.iter().position(|r| matches!(r, Row::App(_)));
    with(|p| {
        p.rows.replace(rows);
        p.table.reloadData();
        match first_app.filter(|_| !q.is_empty()) {
            Some(i) => select(p, i),
            None => {
                unsafe { p.table.deselectAll(None) };
                p.table.scrollRowToVisible(0);
            }
        }
        p.page.setNeedsLayout(true);
    });
}

fn select(p: &Picker, row: usize) {
    p.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
    p.table.scrollRowToVisible(row as NSInteger);
}

fn set_open(open: bool) {
    with(|p| {
        if p.open.replace(open) == open {
            return;
        }
        p.panel.setHidden(!open);
        p.hint.setHidden(open);
        if open && p.table.selectedRow() < 0 {
            p.table.scrollRowToVisible(0);
        }
        if let Some(img) = ui::symbol(if open { "chevron.up" } else { "chevron.down" }) {
            p.chevron.setImage(Some(&img));
        }
        p.page.setNeedsLayout(true);
    });
}

/// Move the highlight to the next selectable app row in `dir`.
fn step(dir: isize) {
    with(|p| {
        let rows = p.rows.borrow();
        let n = rows.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = p.table.selectedRow();
        if i < 0 {
            i = if dir > 0 { -1 } else { n };
        }
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if matches!(rows[i as usize], Row::App(_)) {
                break;
            }
        }
        drop(rows);
        select(p, i as usize);
    });
}

fn commit(row: Option<usize>) {
    let (target, query) = with(|p| {
        let row = row.or_else(|| usize::try_from(p.table.selectedRow()).ok());
        let target = row.and_then(|r| p.rows.borrow().get(r).copied());
        (target, p.field.stringValue().to_string())
    })
    .unwrap_or((None, String::new()));
    match target {
        Some(Row::App(i)) => util::later(move || app::open_app(i)),
        _ if !query.trim().is_empty() => {
            // Not an app: treat it as an address or a search.
            let q = query.clone();
            util::later(move || app::address_entered(&q));
        }
        _ => {}
    }
}

fn handle_command(cmd: Sel) -> bool {
    if cmd == sel!(moveDown:) {
        set_open(true);
        step(1);
        true
    } else if cmd == sel!(moveUp:) {
        step(-1);
        true
    } else if cmd == sel!(insertNewline:) {
        commit(None);
        true
    } else if cmd == sel!(cancelOperation:) {
        let open = with(|p| p.open.get()).unwrap_or(false);
        if open {
            set_open(false);
        } else {
            with(|p| p.field.setStringValue(&util::ns("")));
            filter("");
        }
        true
    } else {
        false
    }
}

fn row_view(table: &NSTableView, row: NSInteger) -> Option<Retained<NSView>> {
    let r = row_at(row)?;
    // The inset table style pads each side, so size rows to the column.
    let width = table.tableColumns().firstObject().map(|c| c.width()).unwrap_or(table.bounds().size.width) - 4.0;
    let mtm = mtm();
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, width, ROW_H));
    match r {
        Row::Header(name) => {
            let l = ui::label(&name.to_uppercase(), 10.5, true);
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
            l.setFrame(rect(10.0, 4.0, width - 20.0, 16.0));
            v.addSubview(&l);
        }
        Row::App(i) => {
            let a = &ZOHO_APPS[i];
            let b = ui::badge(a.color, None, &a.initials(), 26.0);
            b.setFrame(rect(8.0, 7.0, 26.0, 26.0));
            v.addSubview(&b);
            let name = ui::label(&a.full_name(), 13.5, true);
            name.setFrame(rect(44.0, 11.0, width * 0.5, 18.0));
            v.addSubview(&name);
            let cat = ui::label(a.category, 11.5, false);
            cat.setTextColor(Some(&NSColor::secondaryLabelColor()));
            cat.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            cat.setFrame(rect(width * 0.5, 12.0, width * 0.5 - 14.0, 16.0));
            v.addSubview(&cat);
        }
        Row::Nothing => {
            let l = ui::label("No Zoho app matches — press ↩ to search or open it as an address", 12.5, false);
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
            l.setFrame(rect(12.0, 11.0, width - 24.0, 18.0));
            v.addSubview(&l);
        }
    }
    // Labels are flipped-agnostic: lay them out in the view's own coordinates
    // (an NSView is not flipped, so convert y from the top).
    for sub in v.subviews().iter() {
        let f = sub.frame();
        let h = if matches!(r, Row::Header(_)) { HEADER_H } else { ROW_H };
        sub.setFrame(rect(f.origin.x, h - f.origin.y - f.size.height, f.size.width, f.size.height));
    }
    Some(v)
}

fn layout(page: &PickerPage) {
    let b = page.bounds().size;
    with(|p| {
        let cw = (b.width - 64.0).min(640.0);
        let x0 = (b.width - cw) / 2.0;
        p.title.setFrame(rect(x0, 70.0, cw, 32.0));
        p.subtitle.setFrame(rect(x0, 106.0, cw, 18.0));
        p.field.setFrame(rect(x0, 142.0, cw - 40.0, 32.0));
        p.chevron.setFrame(rect(x0 + cw - 32.0, 144.0, 28.0, 28.0));
        p.hint.setFrame(rect(x0, 186.0, cw, 16.0));
        let content: f64 = p.rows.borrow().iter().map(|r| if matches!(r, Row::Header(_)) { HEADER_H } else { ROW_H }).sum::<f64>() + 16.0;
        let h = content.min(MAX_LIST_H).min(b.height - 200.0).max(60.0);
        p.panel.setFrame(rect(x0, 182.0, cw, h));
        p.scroll.setFrame(rect(0.0, 0.0, cw, h));
        if let Some(col) = p.table.tableColumns().firstObject() {
            col.setWidth(cw - 44.0);
        }
    });
}
