//! "Switch to Client…" (⇧⌘O): a small searchable list of every client, so
//! moving between clients never needs the mouse. Typing filters by name
//! (accents and case ignored, names that start with the text first); ↑/↓
//! move, ↩ switches, Esc or clicking elsewhere closes.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSControl, NSControlTextEditingDelegate, NSPanel, NSScrollView, NSSearchField, NSSearchFieldDelegate, NSTableColumn, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewStyle, NSTextFieldDelegate, NSTextView, NSView, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};
use uuid::Uuid;

use crate::{app, model, theme, ui, util};

const W: f64 = 360.0;
const ROW_H: f64 = 40.0;
const MAX_ROWS: usize = 9;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

/// The clients matching `query`, best first: names starting with it, then
/// names containing it, each in sidebar order. An empty query lists all.
pub fn matches(clients: &[(Uuid, String)], query: &str) -> Vec<Uuid> {
    let q = model::fold(query.trim());
    if q.is_empty() {
        return clients.iter().map(|c| c.0).collect();
    }
    let folded: Vec<(Uuid, String)> = clients.iter().map(|(id, n)| (*id, model::fold(n))).collect();
    let starts = folded.iter().filter(|(_, n)| n.starts_with(&q) || n.split_whitespace().any(|w| w.starts_with(&q)));
    let contains = folded.iter().filter(|(_, n)| !(n.starts_with(&q) || n.split_whitespace().any(|w| w.starts_with(&q))) && n.contains(&q));
    starts.chain(contains).map(|(id, _)| *id).collect()
}

struct Switcher {
    panel: Retained<NSPanel>,
    field: Retained<NSSearchField>,
    table: Retained<NSTableView>,
    scroll: Retained<NSScrollView>,
    rows: RefCell<Vec<Uuid>>,
    selected: Cell<usize>,
}

thread_local! {
    static SWITCHER: RefCell<Option<Switcher>> = const { RefCell::new(None) };
    static DELEGATE: RefCell<Option<Retained<Delegate>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    impl Delegate {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _n: &NSNotification) {
            refilter();
        }

        #[unsafe(method(rowClicked:))]
        fn row_clicked(&self, _s: Option<&AnyObject>) {
            let row = SWITCHER.with_borrow(|s| s.as_ref().map(|s| s.table.clickedRow())).unwrap_or(-1);
            if row >= 0 {
                choose(row as usize);
            }
        }
    }

    unsafe impl NSControlTextEditingDelegate for Delegate {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _c: &NSControl, _tv: &NSTextView, cmd: Sel) -> objc2::runtime::Bool {
            let handled = if cmd == sel!(moveDown:) {
                step(1);
                true
            } else if cmd == sel!(moveUp:) {
                step(-1);
                true
            } else if cmd == sel!(insertNewline:) {
                let at = SWITCHER.with_borrow(|s| s.as_ref().map(|s| s.selected.get())).unwrap_or(0);
                choose(at);
                true
            } else if cmd == sel!(cancelOperation:) {
                close();
                true
            } else {
                false
            };
            objc2::runtime::Bool::new(handled)
        }
    }

    unsafe impl NSTextFieldDelegate for Delegate {}
    unsafe impl NSSearchFieldDelegate for Delegate {}

    unsafe impl NSTableViewDataSource for Delegate {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn rows(&self, _t: &NSTableView) -> NSInteger {
            SWITCHER.with_borrow(|s| s.as_ref().map(|s| s.rows.borrow().len() as NSInteger)).unwrap_or(0)
        }
    }

    unsafe impl NSTableViewDelegate for Delegate {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(&self, table: &NSTableView, _col: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            let id = SWITCHER.with_borrow(|s| s.as_ref().and_then(|s| s.rows.borrow().get(row as usize).copied()));
            let width = table.bounds().size.width;
            id.and_then(|id| app::store(|s| s.client(id).map(|c| ui::client_row(c, width, false))))
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        // Clicking anywhere else closes it, like Spotlight.
        #[unsafe(method(windowDidResignKey:))]
        fn resigned(&self, _n: &NSNotification) {
            util::later(close);
        }
    }
);

fn delegate() -> Retained<Delegate> {
    DELEGATE.with_borrow_mut(|d| {
        d.get_or_insert_with(|| {
            let this = Delegate::alloc(mtm()).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

fn all_clients() -> Vec<(Uuid, String)> {
    app::store(|s| s.ordered().iter().map(|c| (c.id, c.name.clone())).collect())
}

/// Open the switcher over the main window (or bring it back to the front).
pub fn show(parent: &objc2_app_kit::NSWindow) {
    if SWITCHER.with_borrow(|s| s.as_ref().map(|s| s.panel.makeKeyAndOrderFront(None)).is_some()) {
        return;
    }
    let mtm = mtm();
    let d = delegate();
    let h = theme::FIELD_H + theme::GRID * 4.0 + ROW_H * MAX_ROWS as f64;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect(0.0, 0.0, W, h),
        NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(&util::ns("Switch to Client"));
    panel.setTitlebarAppearsTransparent(true);
    panel.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
    panel.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    let body = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, W, h));
    panel.setContentView(Some(&body));

    let field = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(theme::GRID * 3.0, h - theme::FIELD_H - theme::GRID * 3.0, W - theme::GRID * 6.0, theme::FIELD_H));
    field.setPlaceholderString(Some(&util::ns("Switch to client…")));
    field.setFont(Some(&theme::font(theme::Text::Body)));
    field.setAccessibilityLabel(Some(&util::ns("Switch to client")));
    unsafe { field.setDelegate(Some(ProtocolObject::from_ref(&*d))) };
    body.addSubview(&field);

    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, W, ROW_H * MAX_ROWS as f64));
    let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &util::ns("client"));
    column.setWidth(W - theme::GRID * 4.0);
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setRowHeight(ROW_H);
    table.setStyle(NSTableViewStyle::SourceList);
    table.setAccessibilityLabel(Some(&util::ns("Clients")));
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*d)));
        table.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        table.setTarget(Some(&d));
        table.setAction(Some(sel!(rowClicked:)));
    }
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, W, ROW_H * MAX_ROWS as f64));
    scroll.setDocumentView(Some(&table));
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    body.addSubview(&scroll);

    // Centred on the main window, a little below its top.
    let pf = parent.frame();
    panel.setFrameOrigin(NSPoint::new(pf.origin.x + (pf.size.width - W) / 2.0, pf.origin.y + pf.size.height - h - 120.0));
    SWITCHER.with_borrow_mut(|s| *s = Some(Switcher { panel: panel.clone(), field: field.clone(), table, scroll, rows: RefCell::new(vec![]), selected: Cell::new(0) }));
    refilter();
    panel.makeKeyAndOrderFront(None);
    panel.makeFirstResponder(Some(&field));
}

fn refilter() {
    let query = SWITCHER.with_borrow(|s| s.as_ref().map(|s| s.field.stringValue().to_string())).unwrap_or_default();
    let rows = matches(&all_clients(), &query);
    // Start on the client after the one in front: switching is the point.
    let active = app::store(|s| s.active_client_id);
    let start = if query.trim().is_empty() { rows.iter().position(|r| Some(*r) != active).unwrap_or(0) } else { 0 };
    SWITCHER.with_borrow(|s| {
        let Some(s) = s else { return };
        s.rows.replace(rows);
        s.table.reloadData();
        s.selected.set(start);
        select(s, start);
    });
}

fn select(s: &Switcher, i: usize) {
    if s.rows.borrow().is_empty() {
        return;
    }
    s.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(i), false);
    s.table.scrollRowToVisible(i as NSInteger);
    let _ = &s.scroll;
}

fn step(dir: isize) {
    SWITCHER.with_borrow(|s| {
        let Some(s) = s else { return };
        let n = s.rows.borrow().len();
        if n == 0 {
            return;
        }
        let i = (s.selected.get() as isize + dir).clamp(0, n as isize - 1) as usize;
        s.selected.set(i);
        select(s, i);
    });
}

fn choose(i: usize) {
    let id = SWITCHER.with_borrow(|s| s.as_ref().and_then(|s| s.rows.borrow().get(i).copied()));
    close();
    if let Some(id) = id {
        util::later(move || app::select_client(id));
    }
}

fn close() {
    if let Some(s) = SWITCHER.with_borrow_mut(|s| s.take()) {
        s.panel.orderOut(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_finds_clients_by_any_word_ignoring_accents() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let clients = vec![(a, "Acme Corp".to_string()), (b, "Café Rouge".to_string()), (c, "Bistro Acmé".to_string())];
        assert_eq!(matches(&clients, ""), vec![a, b, c]);
        assert_eq!(matches(&clients, "acme"), vec![a, c], "both start a word");
        assert_eq!(matches(&clients, "cafe"), vec![b]);
        assert_eq!(matches(&clients, "rou"), vec![b]);
        assert_eq!(matches(&clients, "orp"), vec![a], "inside a word comes last");
        assert!(matches(&clients, "zzz").is_empty());
    }
}
