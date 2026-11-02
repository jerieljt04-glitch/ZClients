//! The native window: sidebar of profiles, toolbar, tab strip, launcher,
//! find bar and the add/customise sheet. Plain AppKit views with manual
//! layout — no web view is spent on the app's own chrome.
#![allow(unused_unsafe)]

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSColor, NSColorWell, NSControl, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSFont, NSImage, NSImageView, NSLineBreakMode, NSMenu, NSMenuDelegate,
    NSMenuItem, NSPasteboardItem, NSPasteboardWriting, NSPopUpButton, NSResponder, NSScrollView, NSSearchField, NSSearchFieldDelegate, NSSecureTextField, NSTableColumn,
    NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTableViewDropOperation, NSTableViewStyle, NSTextField, NSTextFieldDelegate, NSTextView,
    NSUserInterfaceItemIdentification, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask, NSWindowTitleVisibility, NSWorkspace,
};
use objc2_foundation::{NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSURL};
use uuid::Uuid;

use crate::model::{self, SortMode};
use crate::{app, downloads, keychain, theme, util, web};

use theme::{FIND_H, SIDEBAR_W, TAB_STRIP_H as TAB_H, TOOLBAR_H as TOP};

const K_ROOT: u8 = 1;
const K_SIDEBAR: u8 = 2;
const K_TOOLBAR: u8 = 3;
const K_TABS: u8 = 4;
const K_PLAIN: u8 = 6;
const K_FIND: u8 = 7;

const T_SWATCH: u8 = 1;
const T_CHIP: u8 = 2;

const CLIENT_DRAG_TYPE: &str = "com.zclients.client";

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("UI runs on the main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)))
}

// ---------------------------------------------------------------------------
// View classes.
// ---------------------------------------------------------------------------

define_class!(
    /// A flipped container whose children are laid out by `layout()` below.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = u8]
    struct FlexView;

    impl FlexView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(layout))]
        fn do_layout(&self) {
            unsafe {
                let _: () = msg_send![super(self), layout];
            }
            layout(*self.ivars(), self);
        }

        // Light/dark (or increased contrast) changed for the window.
        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            unsafe {
                let _: () = msg_send![super(self), viewDidChangeEffectiveAppearance];
            }
            if *self.ivars() == K_ROOT {
                util::later(reapply_layer_colors);
            }
        }

        // The toolbar and tab strip sit in the title bar area, so empty
        // space there drags the window like a normal title bar.
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn can_move_window(&self) -> bool {
            matches!(*self.ivars(), K_TOOLBAR | K_TABS)
        }
    }
);

fn flex(kind: u8) -> Retained<FlexView> {
    let this = FlexView::alloc(mtm()).set_ivars(kind);
    unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 100.0, 100.0)] }
}

pub struct ChipIvars {
    client: Uuid,
    tab: Uuid,
    down: Cell<Option<NSPoint>>,
    dragged: Cell<bool>,
    /// What the chip currently shows; it is only rebuilt when this changes.
    rendered: RefCell<String>,
    /// What VoiceOver reads: the title plus any state (selected is separate).
    ax_label: RefCell<String>,
    ax_selected: Cell<bool>,
    /// The pointer is over the chip (subtle fill, close button shown).
    hover: Cell<bool>,
    closable: Cell<bool>,
    /// Draw the 1 pt divider on the right (both neighbours unselected).
    divider: Cell<bool>,
    /// The state `style_chip` last drew (bits: selected, hover, closable,
    /// divider; 0xff = draw again).
    styled: Cell<u8>,
}

define_class!(
    /// The address bar, behaving like a browser's: the first click selects
    /// the whole address (so ⌘C copies all of it); a drag or a second click
    /// places the cursor as usual. Its right-click menu adds Paste and Go.
    #[unsafe(super(NSTextField, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct AddressField;

    impl AddressField {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let was_editing = self.currentEditor().is_some();
            unsafe {
                let _: () = msg_send![super(self), mouseDown: event];
            }
            if !was_editing
                && let Some(editor) = self.currentEditor()
                    && editor.selectedRange().length == 0 {
                        unsafe { editor.selectAll(None) };
                    }
        }

        // The field editor asks its delegate (this field) for its menu.
        #[unsafe(method_id(textView:menu:forEvent:atIndex:))]
        fn editor_menu(&self, _tv: &NSTextView, menu: &NSMenu, _event: &NSEvent, _index: usize) -> Option<Retained<NSMenu>> {
            let paste_at = unsafe { menu.indexOfItemWithTarget_andAction(None, Some(sel!(paste:))) };
            let at = if paste_at >= 0 { paste_at + 1 } else { 0 };
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm()), &util::ns("Paste and Go"), Some(sel!(pasteAndGo:)), &util::ns(""))
            };
            unsafe { item.setTarget(Some(&target())) };
            item.setEnabled(clipboard_text().is_some());
            menu.insertItem_atIndex(&item, at);
            Some(objc2::Message::retain(menu))
        }
    }
);

define_class!(
    /// The address field's cell: its text (drawn and edited) starts after
    /// the icon instead of under it.
    #[unsafe(super(objc2_app_kit::NSTextFieldCell, objc2_app_kit::NSActionCell, objc2_app_kit::NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct AddressCell;

    impl AddressCell {
        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect(&self, bounds: NSRect) -> NSRect {
            let r: NSRect = unsafe { msg_send![super(self), drawingRectForBounds: bounds] };
            NSRect::new(NSPoint::new(r.origin.x + theme::ADDRESS_ICON_INSET, r.origin.y), NSSize::new((r.size.width - theme::ADDRESS_ICON_INSET).max(0.0), r.size.height))
        }
    }
);

/// The clipboard's text, if it holds any.
fn clipboard_text() -> Option<String> {
    objc2_app_kit::NSPasteboard::generalPasteboard().stringForType(unsafe { objc2_app_kit::NSPasteboardTypeString }).map(|s| s.to_string()).filter(|s| !s.trim().is_empty())
}

define_class!(
    /// One tab in the tab strip. Click selects, drag reorders, middle-click
    /// closes, right-click shows the tab menu.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ChipIvars]
    struct TabChip;

    impl TabChip {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().down.set(Some(event.locationInWindow()));
            self.ivars().dragged.set(false);
            app::select_tab(self.ivars().client, self.ivars().tab);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(start) = self.ivars().down.get() else { return };
            let p = event.locationInWindow();
            if (p.x - start.x).abs() > 6.0 && !self.ivars().dragged.get() {
                self.ivars().dragged.set(true);
                self.setAlphaValue(0.6);
            }
            if self.ivars().dragged.get() {
                show_drop_marker(self.ivars().tab, p);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.setAlphaValue(1.0);
            hide_drop_marker();
            if self.ivars().dragged.replace(false) {
                // Only a release over the tab strip moves the tab; letting
                // go over the page or the toolbar puts it back.
                let p = event.locationInWindow();
                if let Some((target, _)) = drop_target(p) {
                    app::move_tab_over(self.ivars().client, self.ivars().tab, target);
                }
            }
            self.ivars().down.set(None);
        }

        // Hover: a subtle fill and the close button, as in Safari.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            unsafe {
                let _: () = msg_send![super(self), updateTrackingAreas];
            }
            if self.trackingAreas().is_empty() {
                use objc2_app_kit::NSTrackingAreaOptions as O;
                let area = unsafe {
                    objc2_app_kit::NSTrackingArea::initWithRect_options_owner_userInfo(
                        <objc2_app_kit::NSTrackingArea as objc2::AllocAnyThread>::alloc(),
                        self.bounds(),
                        O::MouseEnteredAndExited | O::ActiveInKeyWindow | O::InVisibleRect,
                        Some(self),
                        None,
                    )
                };
                self.addTrackingArea(&area);
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _e: &NSEvent) {
            self.ivars().hover.set(true);
            style_chip(self);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            self.ivars().hover.set(false);
            style_chip(self);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, _event: &NSEvent) {
            app::request_close_tab(self.ivars().client, self.ivars().tab);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let menu = self.tab_menu();
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }

        // ---- keyboard (Full Keyboard Access) ----------------------------

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeKeyView))]
        fn can_become_key_view(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            self.setKeyboardFocusRingNeedsDisplayInRect(self.bounds());
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.setKeyboardFocusRingNeedsDisplayInRect(self.bounds());
            true
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {
            let path = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 7.0, 7.0);
            path.fill();
        }

        #[unsafe(method(focusRingMaskBounds))]
        fn focus_ring_mask_bounds(&self) -> NSRect {
            self.bounds()
        }

        /// ←/→ move between tabs, Space/Return selects, ⌥⇧←/→ moves the
        /// tab, Delete closes it, ⇧F10 or the menu key opens its menu.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let (client, tab) = (self.ivars().client, self.ivars().tab);
            let chars = event.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
            let mods = event.modifierFlags() & (NSEventModifierFlags::Option | NSEventModifierFlags::Shift | NSEventModifierFlags::Command | NSEventModifierFlags::Control);
            let reorder = mods == NSEventModifierFlags::Option | NSEventModifierFlags::Shift;
            match chars.as_str() {
                "\u{F702}" if reorder => app::move_tab(client, tab, -1),
                "\u{F703}" if reorder => app::move_tab(client, tab, 1),
                "\u{F702}" => focus_chip_offset(tab, -1),
                "\u{F703}" => focus_chip_offset(tab, 1),
                " " | "\r" => app::select_tab(client, tab),
                "\u{7f}" | "\u{F728}" => app::request_close_tab(client, tab),
                _ => unsafe {
                    let _: () = msg_send![super(self), keyDown: event];
                },
            }
        }

        // ---- accessibility ---------------------------------------------

        #[unsafe(method(isAccessibilityElement))]
        fn is_ax_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn ax_role(&self) -> Retained<objc2_foundation::NSString> {
            util::ns("AXRadioButton")
        }

        #[unsafe(method_id(accessibilitySubrole))]
        fn ax_subrole(&self) -> Retained<objc2_foundation::NSString> {
            util::ns("AXTabButton")
        }

        #[unsafe(method_id(accessibilityRoleDescription))]
        fn ax_role_description(&self) -> Retained<objc2_foundation::NSString> {
            util::ns("tab")
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn ax_label(&self) -> Retained<objc2_foundation::NSString> {
            util::ns(&self.ivars().ax_label.borrow())
        }

        #[unsafe(method(isAccessibilitySelected))]
        fn ax_selected(&self) -> bool {
            self.ivars().ax_selected.get()
        }

        #[unsafe(method_id(accessibilityValue))]
        fn ax_value(&self) -> Retained<objc2_foundation::NSNumber> {
            objc2_foundation::NSNumber::new_bool(self.ivars().ax_selected.get())
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn ax_press(&self) -> bool {
            app::select_tab(self.ivars().client, self.ivars().tab);
            true
        }

        #[unsafe(method(accessibilityPerformShowMenu))]
        fn ax_show_menu(&self) -> bool {
            let menu = self.tab_menu();
            unsafe { menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, self.bounds().size.height), Some(self)) };
            true
        }
    }
);

impl TabChip {
    fn tab_menu(&self) -> Retained<NSMenu> {
        MENU_TAB.set(Some((self.ivars().client, self.ivars().tab)));
        let menu = NSMenu::new(mtm());
        add_item(&menu, "Move Tab Left", sel!(tabMenuLeft:), "", 0);
        add_item(&menu, "Move Tab Right", sel!(tabMenuRight:), "", 0);
        add_item(&menu, "Duplicate Tab", sel!(tabMenuDuplicate:), "", 0);
        menu.addItem(&NSMenuItem::separatorItem(mtm()));
        add_item(&menu, "Close Tab", sel!(tabMenuClose:), "", 0);
        menu
    }
}

/// Move keyboard focus to the chip `offset` places from `tab`'s.
fn focus_chip_offset(tab: Uuid, offset: isize) {
    with_ui(|u| {
        let chips = u.chips.borrow();
        let Some(i) = chips.iter().position(|c| c.ivars().tab == tab) else { return };
        let j = (i as isize + offset).clamp(0, chips.len() as isize - 1) as usize;
        if let Some(c) = chips.get(j) {
            if c.isHidden() {
                app::select_tab(c.ivars().client, c.ivars().tab);
            }
            u.window.makeFirstResponder(Some(c));
        }
    });
}

pub struct TileIvars {
    kind: u8,
    index: usize,
}

define_class!(
    /// A clickable rounded tile: launcher apps, colour swatches and the
    /// toolbar's profile chip.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TileIvars]
    struct Tile;

    impl Tile {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            self.setAlphaValue(0.7);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.setAlphaValue(1.0);
            self.activate();
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeKeyView))]
        fn can_become_key_view(&self) -> bool {
            true
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {
            let r = self.layer().map(|l| l.cornerRadius()).unwrap_or(6.0);
            objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), r, r).fill();
        }

        #[unsafe(method(focusRingMaskBounds))]
        fn focus_ring_mask_bounds(&self) -> NSRect {
            self.bounds()
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let chars = event.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
            if chars == " " || chars == "\r" {
                self.activate();
            } else {
                unsafe {
                    let _: () = msg_send![super(self), keyDown: event];
                }
            }
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_ax_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn ax_role(&self) -> Retained<objc2_foundation::NSString> {
            util::ns("AXButton")
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn ax_label(&self) -> Retained<objc2_foundation::NSString> {
            let (kind, index) = (self.ivars().kind, self.ivars().index);
            util::ns(&match kind {
                T_SWATCH => format!("{} colour", model::PROFILE_COLORS.get(index).map(|c| c.0).unwrap_or("Client")),
                _ => app::store(|s| s.active_client().map(|c| if c.is_search() { "Google Search workspace".to_string() } else { format!("Client {}, customise", c.name) })).unwrap_or_default(),
            })
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn ax_press(&self) -> bool {
            self.activate();
            true
        }
    }
);

impl Tile {
    fn activate(&self) {
        let (kind, index) = (self.ivars().kind, self.ivars().index);
        util::later(move || match kind {
            T_SWATCH => sheet_pick_swatch(index),
            _ => customise_active(),
        });
    }
}

fn tile(kind: u8, index: usize, frame: NSRect) -> Retained<Tile> {
    let this = Tile::alloc(mtm()).set_ivars(TileIvars { kind, index });
    let t: Retained<Tile> = unsafe { msg_send![super(this), initWithFrame: frame] };
    t.setWantsLayer(true);
    t
}

// ---------------------------------------------------------------------------
// Actions: every button, menu item, the table and the main window delegate.
// ---------------------------------------------------------------------------

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct Actions;

    unsafe impl NSObjectProtocol for Actions {}

    impl Actions {
        #[unsafe(method(newTab:))] fn a_new_tab(&self, _s: Option<&AnyObject>) { app::new_tab(); }
        #[unsafe(method(closeTab:))] fn a_close_tab(&self, _s: Option<&AnyObject>) { app::close_active_tab(); }
        #[unsafe(method(reopenTab:))] fn a_reopen(&self, _s: Option<&AnyObject>) { app::reopen_closed_tab(); }
        #[unsafe(method(duplicateTab:))] fn a_dup(&self, _s: Option<&AnyObject>) { app::duplicate_tab(); }
        #[unsafe(method(back:))] fn a_back(&self, _s: Option<&AnyObject>) { web::go_back(); }
        #[unsafe(method(forward:))] fn a_forward(&self, _s: Option<&AnyObject>) { web::go_forward(); }
        #[unsafe(method(reload:))] fn a_reload(&self, _s: Option<&AnyObject>) { web::reload(false); }
        #[unsafe(method(forceReload:))] fn a_force_reload(&self, _s: Option<&AnyObject>) { web::reload(true); }
        #[unsafe(method(reloadOrStop:))]
        fn a_reload_stop(&self, _s: Option<&AnyObject>) {
            let loading = app::store(|s| s.active_tab().map(|(_, t)| t.id)).and_then(web::page_state).is_some_and(|p| p.loading);
            if loading { web::stop() } else { web::reload(false) }
        }
        #[unsafe(method(home:))] fn a_home(&self, _s: Option<&AnyObject>) { app::go_home(); }
        #[unsafe(method(openLocation:))] fn a_location(&self, _s: Option<&AnyObject>) { focus_address(); }
        #[unsafe(method(nextTab:))] fn a_next(&self, _s: Option<&AnyObject>) { app::cycle_tab(1); }
        #[unsafe(method(prevTab:))] fn a_prev(&self, _s: Option<&AnyObject>) { app::cycle_tab(-1); }
        #[unsafe(method(selectTabN:))] fn a_tab_n(&self, s: Option<&NSMenuItem>) { if let Some(s) = s { app::select_tab_index(s.tag() as usize); } }
        #[unsafe(method(lastTab:))] fn a_last(&self, _s: Option<&AnyObject>) { app::select_last_tab(); }
        #[unsafe(method(find:))] fn a_find(&self, _s: Option<&AnyObject>) { show_find(true); }
        #[unsafe(method(findNext:))] fn a_find_next(&self, _s: Option<&AnyObject>) { run_find(false); }
        #[unsafe(method(findPrev:))] fn a_find_prev(&self, _s: Option<&AnyObject>) { run_find(true); }
        #[unsafe(method(findDone:))] fn a_find_done(&self, _s: Option<&AnyObject>) { show_find(false); }
        #[unsafe(method(zoomIn:))] fn a_zoom_in(&self, _s: Option<&AnyObject>) { web::zoom(0.1); }
        #[unsafe(method(zoomOut:))] fn a_zoom_out(&self, _s: Option<&AnyObject>) { web::zoom(-0.1); }
        #[unsafe(method(zoomReset:))] fn a_zoom_reset(&self, _s: Option<&AnyObject>) { web::zoom(0.0); }
        #[unsafe(method(zcToggleSidebar:))] fn a_sidebar(&self, _s: Option<&AnyObject>) { toggle_sidebar(); }
        #[unsafe(method(search:))] fn a_search(&self, _s: Option<&AnyObject>) { app::open_search(""); }
        #[unsafe(method(meet:))] fn a_meet(&self, _s: Option<&AnyObject>) { app::open_meet(); }
        /// Commands that have nothing to act on are greyed out instead of
        /// silently doing nothing (Back on the launcher, Print in the
        /// Search workspace's empty tab, Customise for Search...).
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            let Some(action) = item.action() else { return true.into() };
            let name = action.name().to_str().unwrap_or("");
            // Menus that enable their items by hand say so here too: an
            // auto-enabling menu asks this method and ignores setEnabled.
            match name {
                "clientUp:" | "clientDown:" => {
                    let dir = if name == "clientUp:" { -1 } else { 1 };
                    MENU_CLIENT.get().is_some_and(|id| !is_filtering() && app::store(|s| s.can_move_client(id, dir)))
                }
                "pasteAndGo:" => clipboard_text().is_some(),
                _ => menu_enabled(name, item.tag(), &MenuContext::now()),
            }
        }

        #[unsafe(method(nextClient:))] fn a_next_client(&self, _s: Option<&AnyObject>) { app::cycle_client(1); }
        #[unsafe(method(prevClient:))] fn a_prev_client(&self, _s: Option<&AnyObject>) { app::cycle_client(-1); }
        #[unsafe(method(selectClientN:))] fn a_client_n(&self, s: Option<&NSMenuItem>) { if let Some(s) = s { app::select_client_number(s.tag() as usize); } }
        #[unsafe(method(switchClient:))]
        fn a_switch_client(&self, _s: Option<&AnyObject>) {
            if let Some(w) = with_ui(|u| u.window.clone()) {
                crate::switcher::show(&w);
            }
        }
        #[unsafe(method(addClient:))] fn a_add_client(&self, _s: Option<&AnyObject>) { show_client_sheet(None); }
        #[unsafe(method(customise:))] fn a_customise(&self, _s: Option<&AnyObject>) { customise_active(); }
        #[unsafe(method(printPage:))] fn a_print(&self, _s: Option<&AnyObject>) { web::print(); }
        #[unsafe(method(downloadPage:))] fn a_download_page(&self, _s: Option<&AnyObject>) { web::download_current(); }
        #[unsafe(method(about:))]
        fn a_about(&self, _s: Option<&AnyObject>) {
            unsafe { NSApplication::sharedApplication(mtm()).orderFrontStandardAboutPanel(None) };
        }

        #[unsafe(method(pasteAndGo:))]
        fn a_paste_and_go(&self, _s: Option<&AnyObject>) {
            let Some(text) = clipboard_text() else { return };
            let text = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
            let tab = with_ui(|u| u.address_tab.get()).flatten();
            leave_address(false);
            util::later(move || app::address_entered_for(tab, &text));
        }

        #[unsafe(method(filterChanged:))]
        fn a_filter(&self, sender: Option<&NSSearchField>) {
            if let Some(f) = sender {
                with_ui(|u| *u.filter.borrow_mut() = f.stringValue().to_string().trim().to_lowercase());
                app::changed(app::SIDEBAR);
            }
        }

        #[unsafe(method(findEntered:))] fn a_find_entered(&self, _s: Option<&AnyObject>) { run_find(false); }

        #[unsafe(method(downloads:))]
        fn a_downloads(&self, sender: Option<&NSButton>) {
            let Some(button) = sender else { return };
            let menu = NSMenu::new(mtm());
            unsafe { menu.setAutoenablesItems(false) };
            let mut any = false;
            let now = std::time::SystemTime::now();
            downloads::with_items(|items| {
                for item in items {
                    if any {
                        menu.addItem(&NSMenuItem::separatorItem(mtm()));
                    }
                    any = true;
                    // Tagged with the item's key, never its index: a download
                    // that starts while the menu is open would shift every
                    // index and the click would hit another file.
                    let i = item.key() as isize;
                    let mark = match item.state {
                        downloads::State::Done => "✓",
                        downloads::State::Downloading => "↓",
                        downloads::State::Failed => "⚠︎",
                    };
                    let open = add_item(&menu, &format!("{mark}  {}", item.name), sel!(downloadItem:), "", i);
                    open.setEnabled(item.state == downloads::State::Done);
                    // Size, percentage and when it finished, on a quieter
                    // line under the name.
                    let detail = downloads::status_line(item, now);
                    if !detail.is_empty() {
                        let d = add_item(&menu, &format!("      {detail}"), sel!(noop:), "", 0);
                        d.setEnabled(false);
                        d.setAttributedTitle(Some(&attributed(&format!("      {detail}"), &theme::digits(theme::Text::Footnote), &theme::secondary_text())));
                    }
                    if item.state == downloads::State::Downloading {
                        add_item(&menu, "      Cancel", sel!(cancelDownload:), "", i);
                    }
                    if item.state == downloads::State::Failed && downloads::retry_url(item.key()).is_some() {
                        add_item(&menu, "      Retry", sel!(retryDownload:), "", i);
                    }
                    if item.path.is_some() && item.state == downloads::State::Done {
                        add_item(&menu, "      Show in Finder", sel!(revealDownload:), "", i);
                    }
                }
            });
            if !any {
                add_item(&menu, "No downloads yet", sel!(noop:), "", 0).setEnabled(false);
            }
            menu.addItem(&NSMenuItem::separatorItem(mtm()));
            add_item(&menu, "Open Downloads Folder", sel!(openDownloadsFolder:), "", 0);
            add_item(&menu, "Clear Finished", sel!(clearDownloads:), "", 0);
            let at = NSPoint::new(0.0, button.bounds().size.height + 4.0);
            unsafe { menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(button)) };
        }

        #[unsafe(method(noop:))] fn a_noop(&self, _s: Option<&AnyObject>) {}
        #[unsafe(method(cancelDownload:))] fn a_cancel_download(&self, s: Option<&NSMenuItem>) { if let Some(s) = s { downloads::cancel(s.tag() as usize); } }
        #[unsafe(method(retryDownload:))]
        fn a_retry_download(&self, s: Option<&NSMenuItem>) {
            let Some(key) = s.map(|s| s.tag() as usize) else { return };
            if let Some((client, url)) = downloads::retry_url(key)
                && !web::download_url(client, &url)
            {
                let who = client.map(web::profile_name).unwrap_or_else(|| "its client".into());
                downloads::set_note(key, &format!("Open a tab of {who} to retry"));
            }
        }

        #[unsafe(method(settings:))] fn a_settings(&self, _s: Option<&AnyObject>) { crate::privacy::show(); }
        #[unsafe(method(exportDiagnostics:))] fn a_diagnostics(&self, _s: Option<&AnyObject>) { crate::privacy::export_diagnostics(); }
        #[unsafe(method(showMainWindow:))] fn a_show_window(&self, _s: Option<&AnyObject>) { show_window(); }
        #[unsafe(method(moveTabLeft:))] fn a_move_left(&self, _s: Option<&AnyObject>) { if let Some((c, t)) = app::store(|s| s.active_tab().map(|(c, t)| (c.id, t.id))) { app::move_tab(c, t, -1); } }
        #[unsafe(method(moveTabRight:))] fn a_move_right(&self, _s: Option<&AnyObject>) { if let Some((c, t)) = app::store(|s| s.active_tab().map(|(c, t)| (c.id, t.id))) { app::move_tab(c, t, 1); } }
        #[unsafe(method(focusTabs:))]
        fn a_focus_tabs(&self, _s: Option<&AnyObject>) {
            with_ui(|u| {
                if let Some(c) = u.chips.borrow().iter().find(|c| c.ivars().ax_selected.get()) {
                    u.window.makeFirstResponder(Some(c));
                }
            });
        }

        /// The All Tabs menu: every tab of the profile, type-to-search, the
        /// active one ticked.
        #[unsafe(method(tabOverflow:))]
        fn a_tab_overflow(&self, _s: Option<&AnyObject>) {
            let menu = NSMenu::new(mtm());
            let tabs = app::store(|s| s.active_client().map(|c| c.tabs.iter().map(|t| (t.display_title(), Some(t.id) == c.active_tab_id)).collect::<Vec<_>>())).unwrap_or_default();
            for (i, (title, active)) in tabs.iter().enumerate() {
                let item = add_item(&menu, title, sel!(selectTabN:), "", i as isize);
                item.setState(if *active { NSControlStateValueOn } else { NSControlStateValueOff });
            }
            let Some((view, at)) = with_ui(|u| {
                let anchor: Retained<NSView> = if u.tab_overflow.isHidden() { Retained::into_super(u.tabs.clone()) } else { Retained::into_super(Retained::into_super(u.tab_overflow.clone())) };
                let h = anchor.bounds().size.height;
                (anchor, NSPoint::new(0.0, h + 2.0))
            }) else { return };
            unsafe { menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(&view)) };
        }

        #[unsafe(method(mediaMenu:))]
        fn a_media_menu(&self, sender: Option<&NSButton>) {
            let Some(button) = sender else { return };
            let menu = NSMenu::new(mtm());
            unsafe { menu.setAutoenablesItems(false) };
            let calls = web::tabs_in_calls();
            MEDIA_TABS.with_borrow_mut(|m| *m = calls.iter().map(|(c, t, _, _)| (*c, *t)).collect());
            for (i, (client, tab, cam, mic)) in calls.iter().enumerate() {
                let (profile, title) = app::store(|s| (s.client(*client).map(|c| c.name.clone()).unwrap_or_default(), s.find_tab(*tab).map(|t| t.display_title()).unwrap_or_default()));
                let what = match (cam, mic) { (true, true) => "Camera and microphone", (true, false) => "Camera", _ => "Microphone" };
                add_item(&menu, &format!("{what} on in “{title}” ({profile})"), sel!(noop:), "", 0).setEnabled(false);
                add_item(&menu, "    Show Tab", sel!(mediaShow:), "", i as isize);
                add_item(&menu, "    Stop Camera and Microphone", sel!(mediaStop:), "", i as isize);
            }
            if web::keeping_awake() {
                menu.addItem(&NSMenuItem::separatorItem(mtm()));
                add_item(&menu, "Keeping the display awake while a call is on", sel!(noop:), "", 0).setEnabled(false);
            }
            menu.addItem(&NSMenuItem::separatorItem(mtm()));
            add_item(&menu, "Privacy Settings…", sel!(settings:), "", 0);
            let at = NSPoint::new(0.0, button.bounds().size.height + 4.0);
            unsafe { menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(button)) };
        }

        #[unsafe(method(mediaShow:))]
        fn a_media_show(&self, s: Option<&NSMenuItem>) {
            let Some(i) = s.map(|s| s.tag() as usize) else { return };
            if let Some((c, t)) = MEDIA_TABS.with_borrow(|m| m.get(i).copied()) {
                app::show_tab(c, t);
            }
        }

        #[unsafe(method(mediaStop:))]
        fn a_media_stop(&self, s: Option<&NSMenuItem>) {
            let Some(i) = s.map(|s| s.tag() as usize) else { return };
            if let Some((_, t)) = MEDIA_TABS.with_borrow(|m| m.get(i).copied()) {
                web::stop_media(t);
            }
        }

        #[unsafe(method(retryLoad:))]
        fn a_retry(&self, _s: Option<&AnyObject>) {
            let Some(tab) = app::store(|s| s.active_tab().map(|(_, t)| t.id)) else { return };
            let failed = LOAD_ERRORS.with_borrow_mut(|m| m.remove(&tab)).and_then(|(_, url)| url);
            refresh(app::CONTENT);
            match failed.and_then(|u| crate::policy::parse(&u)) {
                Some(u) => web::load_url(&u),
                None => web::reload(false),
            }
        }

        #[unsafe(method(errorOpenInBrowser:))]
        fn a_error_browser(&self, _s: Option<&AnyObject>) {
            let Some(tab) = app::store(|s| s.active_tab().map(|(_, t)| t.id)) else { return };
            let url = LOAD_ERRORS.with_borrow(|m| m.get(&tab).and_then(|(_, u)| u.clone()));
            if let Some(u) = url.as_deref().and_then(crate::policy::parse).filter(crate::policy::is_https).and_then(|u| util::ns_url(u.as_str())) {
                NSWorkspace::sharedWorkspace().openURL(&u);
            }
        }

        /// A PDF tab whose preview is gone: fetch the document again (it
        /// opens as a new tab when it arrives) and close the empty one.
        #[unsafe(method(errorRedownload:))]
        fn a_error_redownload(&self, _s: Option<&AnyObject>) {
            let Some((client, tab)) = app::store(|s| s.active_tab().map(|(c, t)| (c.id, t.id))) else { return };
            let url = LOAD_ERRORS.with_borrow(|m| m.get(&tab).and_then(|(_, u)| u.clone()));
            if let Some(u) = url {
                let _ = web::download_url(Some(client), &u);
            }
            util::later(move || app::close_tab(client, tab));
        }

        #[unsafe(method(errorDismiss:))]
        fn a_error_dismiss(&self, _s: Option<&AnyObject>) {
            if let Some(tab) = app::store(|s| s.active_tab().map(|(_, t)| t.id)) {
                clear_load_error(tab);
            }
        }

        #[unsafe(method(errorCloseTab:))]
        fn a_error_close(&self, _s: Option<&AnyObject>) {
            app::close_active_tab();
        }

        #[unsafe(method(showDownloads:))]
        fn a_show_downloads(&self, _s: Option<&AnyObject>) {
            if let Some(b) = with_ui(|u| u.downloads_button.clone()) {
                unsafe { let _: () = msg_send![self, downloads: &*b]; }
            }
        }

        #[unsafe(method(downloadItem:))]
        fn a_download_item(&self, s: Option<&NSMenuItem>) {
            let Some(key) = s.map(|s| s.tag() as usize) else { return };
            // PDFs, pictures, video and audio open as a tab here, never in
            // Preview or another app.
            if downloads::show_preview(key) {
                return;
            }
            if let Some(path) = downloads::path_of(key) {
                NSWorkspace::sharedWorkspace().openURL(&NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy())));
            }
        }

        #[unsafe(method(revealDownload:))]
        fn a_reveal(&self, s: Option<&NSMenuItem>) {
            let Some(key) = s.map(|s| s.tag() as usize) else { return };
            if let Some(path) = downloads::path_of(key) {
                let url = NSURL::fileURLWithPath(&util::ns(&path.to_string_lossy()));
                NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
            }
        }

        #[unsafe(method(openDownloadsFolder:))]
        fn a_open_downloads(&self, _s: Option<&AnyObject>) {
            NSWorkspace::sharedWorkspace().openURL(&NSURL::fileURLWithPath(&util::ns(&util::downloads_dir().to_string_lossy())));
        }

        #[unsafe(method(clearDownloads:))] fn a_clear_downloads(&self, _s: Option<&AnyObject>) { downloads::clear_finished(); }

        #[unsafe(method(sortMenu:))]
        fn a_sort_menu(&self, sender: Option<&NSButton>) {
            let Some(button) = sender else { return };
            let menu = sort_menu();
            let at = NSPoint::new(0.0, button.bounds().size.height + 4.0);
            unsafe { menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(button)) };
        }

        #[unsafe(method(setSort:))]
        fn a_set_sort(&self, s: Option<&NSMenuItem>) {
            if let Some(mode) = s.and_then(|s| SortMode::ALL.get(s.tag() as usize)) {
                app::set_sort(*mode);
            }
        }

        #[unsafe(method(tabClose:))]
        fn a_tab_close(&self, s: Option<&NSButton>) {
            let Some(i) = s.map(|s| s.tag() as usize) else { return };
            if let Some((c, t)) = app::store(|st| st.active_client().and_then(|c| c.tabs.get(i).map(|t| (c.id, t.id)))) {
                app::request_close_tab(c, t);
            }
        }

        #[unsafe(method(tabMenuLeft:))] fn a_tm_left(&self, _s: Option<&AnyObject>) { if let Some((c, t)) = MENU_TAB.get() { app::move_tab(c, t, -1); } }
        #[unsafe(method(tabMenuRight:))] fn a_tm_right(&self, _s: Option<&AnyObject>) { if let Some((c, t)) = MENU_TAB.get() { app::move_tab(c, t, 1); } }
        #[unsafe(method(tabMenuClose:))] fn a_tm_close(&self, _s: Option<&AnyObject>) { if let Some((c, t)) = MENU_TAB.get() { app::request_close_tab(c, t); } }
        #[unsafe(method(tabMenuDuplicate:))]
        fn a_tm_dup(&self, _s: Option<&AnyObject>) {
            if let Some((c, t)) = MENU_TAB.get() {
                app::select_tab(c, t);
                util::later(app::duplicate_tab);
            }
        }

        #[unsafe(method(clientCustomise:))] fn a_cm_customise(&self, _s: Option<&AnyObject>) { if let Some(id) = MENU_CLIENT.get() { show_client_sheet(Some(id)); } }
        #[unsafe(method(clientUp:))] fn a_cm_up(&self, _s: Option<&AnyObject>) { if let Some(id) = MENU_CLIENT.get() { app::move_client(id, -1); } }
        #[unsafe(method(clientDown:))] fn a_cm_down(&self, _s: Option<&AnyObject>) { if let Some(id) = MENU_CLIENT.get() { app::move_client(id, 1); } }
        #[unsafe(method(clientDelete:))]
        fn a_cm_delete(&self, _s: Option<&AnyObject>) {
            if let Some(id) = MENU_CLIENT.get() {
                util::later(move || app::confirm_delete_client(id));
            }
        }

        #[unsafe(method(sheetSave:))] fn a_sheet_save(&self, _s: Option<&AnyObject>) { sheet_finish(true); }
        #[unsafe(method(dismissEmpty:))]
        fn a_dismiss_empty(&self, _s: Option<&AnyObject>) {
            EMPTY_DISMISSED.set(true);
            refresh(app::CONTENT);
        }
        #[unsafe(method(sheetCancel:))] fn a_sheet_cancel(&self, _s: Option<&AnyObject>) { sheet_finish(false); }
        #[unsafe(method(sheetDelete:))]
        fn a_sheet_delete(&self, _s: Option<&AnyObject>) {
            let client = SHEET.with_borrow(|s| s.as_ref().and_then(|s| s.client));
            sheet_finish(false);
            if let Some(id) = client {
                util::later(move || app::confirm_delete_client(id));
            }
        }
        #[unsafe(method(sheetChangeLogin:))]
        fn a_sheet_change(&self, _s: Option<&AnyObject>) {
            sheet_set_login_mode(LoginMode::Replace);
        }

        #[unsafe(method(sheetRemoveLogin:))]
        fn a_sheet_remove(&self, _s: Option<&AnyObject>) {
            sheet_set_login_mode(LoginMode::Remove);
        }

        #[unsafe(method(sheetKeepToggled:))]
        fn a_sheet_keep(&self, _s: Option<&AnyObject>) {
            SHEET.with_borrow(|s| {
                if let Some(s) = s {
                    s.retention.setEnabled(s.keep_signed_in.state() == NSControlStateValueOn);
                }
            });
        }
    }

    unsafe impl NSWindowDelegate for Actions {
        // The red close button hides the window, like Chrome or Mail: tabs,
        // calls and downloads go on, and the Dock icon (or Window ›
        // ZClients) brings it back. ⌘Q quits, asking first during a
        // download or a call (`app::should_quit`).
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _w: &NSWindow) -> bool {
            hide_window();
            false
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            util::later(|| unsafe { NSApplication::sharedApplication(mtm()).terminate(None) });
        }
    }

    unsafe impl NSTableViewDataSource for Actions {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn rows(&self, _t: &NSTableView) -> NSInteger {
            with_ui(|u| u.rows.borrow().len() as NSInteger).unwrap_or(0)
        }

        #[unsafe(method_id(tableView:pasteboardWriterForRow:))]
        fn writer(&self, _t: &NSTableView, row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            drag_writer(row)
        }

        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate_drop(&self, table: &NSTableView, _info: &ProtocolObject<dyn NSDraggingInfo>, row: NSInteger, op: NSTableViewDropOperation) -> NSDragOperation {
            if op == NSTableViewDropOperation::On {
                unsafe { table.setDropRow_dropOperation(row, NSTableViewDropOperation::Above) };
            }
            // Nothing may land above the pinned Search row.
            if row < 1 { NSDragOperation::None } else { NSDragOperation::Move }
        }

        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept_drop(&self, _t: &NSTableView, info: &ProtocolObject<dyn NSDraggingInfo>, row: NSInteger, _op: NSTableViewDropOperation) -> Bool {
            let pb = unsafe { info.draggingPasteboard() };
            let Some(text) = pb.stringForType(&util::ns(CLIENT_DRAG_TYPE)) else { return Bool::NO };
            let Ok(id) = Uuid::parse_str(&text.to_string()) else { return Bool::NO };
            let rows = with_ui(|u| u.rows.borrow().clone()).unwrap_or_default();
            let from = rows.iter().position(|r| *r == id);
            // Dropping "above row r": land on the client at r when moving up,
            // or on the one just before r when moving down.
            let target = match from {
                Some(f) if (row as usize) > f => rows.get(row as usize - 1).copied(),
                _ => rows.get(row as usize).copied().or(rows.last().copied()),
            };
            if let Some(t) = target {
                util::later(move || app::move_client_over(id, t));
            }
            Bool::YES
        }
    }

    unsafe impl NSControlTextEditingDelegate for Actions {
        /// Esc in the find bar closes it and puts the cursor back in the
        /// page. Without this, Esc first cleared the text and then fell
        /// through to the window, which in full screen meant leaving full
        /// screen (the app seemed to shrink away) and never closed the bar.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, control: &NSControl, _tv: &NSTextView, cmd: Sel) -> Bool {
            let is_find = with_ui(|u| std::ptr::eq(control as *const NSControl, Retained::as_ptr(&u.find_field).cast())).unwrap_or(false);
            if is_find && cmd == sel!(cancelOperation:) {
                show_find(false);
                return Bool::YES;
            }
            // The address bar only acts on Return. It used to act whenever
            // editing ended, so clicking into a page (even in another tab)
            // loaded whatever the bar held: the "tab restarted" bug.
            let is_address = with_ui(|u| std::ptr::eq(control as *const NSControl, Retained::as_ptr(&u.address).cast())).unwrap_or(false);
            if is_address && (cmd == sel!(insertNewline:) || cmd == sel!(insertNewlineIgnoringFieldEditor:)) {
                let text = control.stringValue().to_string();
                // The tab the text was typed for; if another tab is in front
                // by the time this runs, nothing happens.
                let tab = with_ui(|u| u.address_tab.get()).flatten();
                leave_address(false);
                util::later(move || app::address_entered_for(tab, &text));
                return Bool::YES;
            }
            if is_address && cmd == sel!(cancelOperation:) {
                leave_address(true);
                return Bool::YES;
            }
            Bool::NO
        }

        // Typing in the address bar: the lock or globe described the page's
        // address, not the text being typed.
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, n: &objc2_foundation::NSNotification) {
            let object = n.object();
            with_ui(|u| {
                if object.as_deref().is_some_and(|o| std::ptr::eq(o as *const AnyObject, Retained::as_ptr(&u.address).cast())) {
                    u.address_icon.setImage(theme::symbol(TYPING_GLYPH.0, theme::Icon::Tab).as_deref());
                    u.address_icon.setToolTip(Some(&util::ns(TYPING_GLYPH.1)));
                }
            });
        }
    }

    unsafe impl NSTextFieldDelegate for Actions {}
    unsafe impl NSSearchFieldDelegate for Actions {}

    unsafe impl NSTableViewDelegate for Actions {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(&self, table: &NSTableView, _col: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            row_view(table, row)
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, _n: &NSNotification) {
            if SUPPRESS_SELECTION.get() {
                return;
            }
            let row = with_ui(|u| u.table.selectedRow()).unwrap_or(-1);
            if let Some(id) = row_client(row) {
                util::later(move || app::select_client(id));
            }
        }
    }

    unsafe impl NSMenuDelegate for Actions {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            menu.removeAllItems();
            let row = with_ui(|u| u.table.clickedRow()).unwrap_or(-1);
            let Some(id) = row_client(row) else { return };
            MENU_CLIENT.set(Some(id));
            let search = id == model::GOOGLE_SEARCH_ID;
            if !search {
                add_item(menu, "Customise…", sel!(clientCustomise:), "", 0);
                menu.addItem(&NSMenuItem::separatorItem(mtm()));
                let up = add_item(menu, "Move Up", sel!(clientUp:), "", 0);
                up.setEnabled(!is_filtering() && app::store(|s| s.can_move_client(id, -1)));
                let down = add_item(menu, "Move Down", sel!(clientDown:), "", 0);
                down.setEnabled(!is_filtering() && app::store(|s| s.can_move_client(id, 1)));
            }
            let sort = NSMenuItem::new(mtm());
            sort.setTitle(&util::ns("Sort Clients By"));
            sort.setSubmenu(Some(&sort_menu()));
            menu.addItem(&sort);
            if !search {
                menu.addItem(&NSMenuItem::separatorItem(mtm()));
                add_item(menu, "Delete Client…", sel!(clientDelete:), "", 0);
            }
        }
    }
);

thread_local! {
    static ACTIONS: OnceCell<Retained<Actions>> = const { OnceCell::new() };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static SUPPRESS_SELECTION: Cell<bool> = const { Cell::new(false) };
    static MENU_CLIENT: Cell<Option<Uuid>> = const { Cell::new(None) };
    static MENU_TAB: Cell<Option<(Uuid, Uuid)>> = const { Cell::new(None) };
    static SHEET: RefCell<Option<Sheet>> = const { RefCell::new(None) };
    static MEDIA_TABS: RefCell<Vec<(Uuid, Uuid)>> = const { RefCell::new(Vec::new()) };
    /// Tabs whose last load failed: why, and the address that failed.
    static LOAD_ERRORS: RefCell<std::collections::HashMap<Uuid, (web::LoadError, Option<String>)>> = RefCell::new(std::collections::HashMap::new());
}

/// What the menus need to know to enable their commands.
#[derive(Clone, Debug, Default)]
pub struct MenuContext {
    /// A page (or PDF) is on screen in the tab in front.
    pub page: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// The client in front is a real client (not the Search workspace).
    pub real_client: bool,
    pub tabs: usize,
    pub clients: usize,
    pub real_clients: usize,
    /// A Meet meeting is open, or a Meet page is in front: no new meeting.
    pub meet_locked: bool,
}

impl MenuContext {
    fn now() -> MenuContext {
        let (tab, url, real_client, tabs, clients, real_clients) = app::store(|s| {
            let (tab, url) = s.active_tab().map(|(_, t)| (Some(t.id), t.url.is_some())).unwrap_or((None, false));
            let real = s.active_client().is_some_and(|c| !c.is_search());
            let tabs = s.active_client().map_or(0, |c| c.tabs.len());
            (tab, url, real, tabs, s.clients.len(), s.client_ids_in_order().len())
        });
        let state = tab.and_then(web::page_state);
        MenuContext {
            page: url && state.is_some(),
            can_back: state.as_ref().is_some_and(|p| p.can_back),
            can_forward: state.as_ref().is_some_and(|p| p.can_forward),
            real_client,
            tabs,
            clients,
            real_clients,
            meet_locked: app::meet_locked(),
        }
    }
}

/// Whether the menu command with this selector name (and tag) can do
/// anything right now. Unknown commands stay enabled.
pub fn menu_enabled(action: &str, tag: isize, c: &MenuContext) -> bool {
    match action {
        "back:" => c.can_back,
        "forward:" => c.can_forward,
        "reload:" | "forceReload:" | "reloadOrStop:" | "printPage:" | "downloadPage:" | "find:" | "findNext:" | "findPrev:" | "zoomIn:" | "zoomOut:" | "zoomReset:" => c.page,
        "customise:" => c.real_client,
        "moveTabLeft:" | "moveTabRight:" | "nextTab:" | "prevTab:" | "lastTab:" => c.tabs > 1,
        "selectTabN:" => (tag as usize) < c.tabs,
        "nextClient:" | "prevClient:" | "switchClient:" => c.clients > 1,
        "selectClientN:" => (tag as usize) < c.real_clients,
        "meet:" => !c.meet_locked,
        _ => true,
    }
}

/// A page failed to load: show a clear explanation with Try Again over it.
pub fn show_load_error(tab: Uuid, kind: web::LoadError, failing_url: Option<String>) {
    LOAD_ERRORS.with_borrow_mut(|m| m.insert(tab, (kind, failing_url)));
    if app::store(|s| s.active_tab().map(|(_, t)| t.id)) == Some(tab) {
        refresh(app::CONTENT);
    }
}

/// The network came back: every tab showing "You're offline" tries again
/// by itself, the way Safari does.
pub fn network_restored() {
    let offline: Vec<(Uuid, Option<String>)> =
        LOAD_ERRORS.with_borrow_mut(|m| m.extract_if(|_, (kind, _)| *kind == web::LoadError::Offline).map(|(tab, (_, url))| (tab, url)).collect());
    // Pages that loaded during the outage without the app's offline screen
    // (Zoho's own error, a service worker's offline copy).
    let stale: Vec<Uuid> = web::take_loaded_offline().into_iter().filter(|t| !offline.iter().any(|(o, _)| o == t)).collect();
    if offline.is_empty() && stale.is_empty() {
        return;
    }
    util::log(format!("retrying {} offline tab(s), reloading {} loaded while offline", offline.len(), stale.len()));
    for (tab, url) in offline {
        web::reload_tab_at(tab, url.as_deref().and_then(crate::policy::parse).as_ref());
    }
    for tab in stale {
        web::reload_tab_at(tab, None);
    }
    refresh(app::CONTENT);
}

/// Self-tests: the error screen a tab is showing, if any.
pub fn load_error_of(tab: Uuid) -> Option<web::LoadError> {
    LOAD_ERRORS.with_borrow(|m| m.get(&tab).map(|(k, _)| *k))
}

/// A tab closed or went away: its error screen (if any) goes with it.
pub fn forget_tab(tab: Uuid) {
    LOAD_ERRORS.with_borrow_mut(|m| m.remove(&tab));
}

pub fn clear_load_error(tab: Uuid) {
    if LOAD_ERRORS.with_borrow_mut(|m| m.remove(&tab)).is_some() {
        refresh(app::CONTENT);
    }
}

fn load_error_text(kind: web::LoadError, url: Option<&str>) -> (String, String) {
    let host = url.and_then(crate::policy::parse).and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "the page".into());
    match kind {
        web::LoadError::Crashed => {
            ("This page stopped working".into(), format!("{host} closed unexpectedly more than once, so ZClients stopped reloading it. Try Again to load it once more."))
        }
        web::LoadError::PdfMissing => (
            "This file is no longer available".into(),
            if url.is_some_and(|u| u.starts_with("https:")) {
                format!("Its preview copy was removed or is damaged. Re-download fetches it again from {host}.")
            } else {
                "Its preview copy was removed or is damaged. Download it again from where you found it.".into()
            },
        ),
        web::LoadError::NotAllowed => {
            ("This address can't be opened in ZClients".into(), format!("ZClients only shows Zoho and Google Meet pages. {host} can open in your default browser instead."))
        }
        web::LoadError::BlockedInSearch => (
            "This page can't be opened here".into(),
            format!("The Google Search workspace opens only Google Search and Zoho pages, so {host} was blocked. To visit it, use a browser."),
        ),
        web::LoadError::Offline => ("You're offline".into(), format!("ZClients can't reach {host}. Check your internet connection, then try again.")),
        web::LoadError::Certificate => {
            ("This connection isn't private".into(), format!("{host}'s certificate couldn't be verified, so the page was not opened and nothing was sent to it."))
        }
        web::LoadError::Authentication => ("Sign-in needed".into(), format!("{host} asked for a username and password that weren't given.")),
        web::LoadError::Unsupported => ("Can't open this address".into(), "This kind of address can't be shown in ZClients.".into()),
        web::LoadError::Other => ("The page couldn't be opened".into(), format!("Something went wrong loading {host}. Try again in a moment.")),
    }
}

/// What the error screen's buttons do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorAction {
    TryAgain,
    OpenInBrowser,
    Redownload,
    CloseTab,
    /// Close the error screen; the page underneath is still there.
    Dismiss,
}

impl ErrorAction {
    fn title(self) -> &'static str {
        match self {
            ErrorAction::TryAgain => "Try Again",
            ErrorAction::OpenInBrowser => "Open in Browser",
            ErrorAction::Redownload => "Re-download",
            ErrorAction::CloseTab => "Close Tab",
            ErrorAction::Dismiss => "Back to Search",
        }
    }

    fn selector(self) -> Sel {
        match self {
            ErrorAction::TryAgain => sel!(retryLoad:),
            ErrorAction::OpenInBrowser => sel!(errorOpenInBrowser:),
            ErrorAction::Redownload => sel!(errorRedownload:),
            ErrorAction::CloseTab => sel!(errorCloseTab:),
            ErrorAction::Dismiss => sel!(errorDismiss:),
        }
    }
}

/// The primary and (optional) secondary button for an error. Opening in
/// the browser is offered only for an https address.
pub fn error_actions(kind: web::LoadError, url: Option<&str>) -> (ErrorAction, Option<ErrorAction>) {
    use web::LoadError as E;
    let https = url.is_some_and(|u| crate::policy::parse(u).is_some_and(|u| crate::policy::is_https(&u)));
    let browser = https.then_some(ErrorAction::OpenInBrowser);
    match kind {
        E::PdfMissing if https => (ErrorAction::Redownload, Some(ErrorAction::CloseTab)),
        E::PdfMissing => (ErrorAction::CloseTab, None),
        E::NotAllowed if https => (ErrorAction::OpenInBrowser, Some(ErrorAction::CloseTab)),
        E::NotAllowed => (ErrorAction::CloseTab, None),
        // Deliberately no Open in Browser: the Search workspace sends
        // nothing elsewhere.
        E::BlockedInSearch => (ErrorAction::Dismiss, None),
        E::Crashed | E::Unsupported | E::Other => (ErrorAction::TryAgain, browser),
        E::Offline | E::Certificate | E::Authentication => (ErrorAction::TryAgain, None),
    }
}

fn error_symbol(kind: web::LoadError) -> &'static str {
    match kind {
        web::LoadError::Offline => "wifi.slash",
        web::LoadError::Certificate | web::LoadError::Authentication => "lock.slash",
        web::LoadError::BlockedInSearch => "hand.raised",
        _ => "exclamationmark.triangle",
    }
}

/// A centred message screen: an SF Symbol, a heading, at most 460 pt of
/// secondary text and up to two buttons (the first is the default). Used
/// for load errors and the first-run empty state.
fn message_screen(frame: NSRect, symbol_name: &str, title: &str, body: &str, buttons: &[(&str, Sel)]) -> Retained<FlexView> {
    let v = flex(K_PLAIN);
    v.setFrame(frame);
    set_layer_color(&v, &theme::window_background(), 0.0);
    let w = frame.size.width;
    let top = (frame.size.height * 0.28).round();
    let mut y = top;
    if let Some(img) = theme::symbol(symbol_name, theme::Icon::Hero) {
        let iv = NSImageView::imageViewWithImage(&img, mtm());
        iv.setContentTintColor(Some(&theme::secondary_text()));
        iv.setFrame(rect((w - 56.0) / 2.0, y, 56.0, 48.0));
        iv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxXMargin);
        v.addSubview(&iv);
        y += 48.0 + theme::GRID * 3.0;
    }
    let t = text(title, theme::Text::Title2, theme::Weight::Regular);
    t.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    t.setFrame(rect(theme::SHEET_MARGIN, y, w - theme::SHEET_MARGIN * 2.0, 30.0));
    t.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    v.addSubview(&t);
    y += 30.0 + theme::GRID * 2.0;
    let text_w = theme::MESSAGE_MAX_W.min(w - theme::SHEET_MARGIN * 2.0);
    let b = NSTextField::wrappingLabelWithString(&util::ns(body), mtm());
    b.setFont(Some(&theme::font(theme::Text::Body)));
    b.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    b.setTextColor(Some(&theme::secondary_text()));
    let text_h = b.sizeThatFits(NSSize::new(text_w, 400.0)).height.ceil().max(20.0);
    b.setFrame(rect((w - text_w) / 2.0, y, text_w, text_h));
    b.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxXMargin);
    v.addSubview(&b);
    y += text_h + theme::GRID * 5.0;
    let made: Vec<Retained<NSButton>> = buttons
        .iter()
        .enumerate()
        .map(|(i, (title, action))| {
            let button = unsafe { NSButton::buttonWithTitle_target_action(&util::ns(title), Some(&target()), Some(*action), mtm()) };
            if i == 0 {
                button.setKeyEquivalent(&util::ns("\r"));
            }
            button.sizeToFit();
            button
        })
        .collect();
    let widths: Vec<f64> = made.iter().map(|b| b.frame().size.width.max(theme::BUTTON_W + 22.0)).collect();
    let total: f64 = widths.iter().sum::<f64>() + theme::GRID * 3.0 * (made.len().saturating_sub(1)) as f64;
    let mut x = (w - total) / 2.0;
    // The default button sits on the right, as in a Mac alert.
    for (button, bw) in made.iter().zip(widths.iter()).rev() {
        button.setFrame(rect(x, y, *bw, theme::BUTTON_H));
        button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxXMargin);
        v.addSubview(button);
        x += bw + theme::GRID * 3.0;
    }
    v.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    v
}

fn error_overlay(kind: web::LoadError, url: Option<&str>, frame: NSRect) -> Retained<NSView> {
    let (title, body) = load_error_text(kind, url);
    let (primary, secondary) = error_actions(kind, url);
    let mut buttons = vec![(primary.title(), primary.selector())];
    if let Some(s) = secondary {
        buttons.push((s.title(), s.selector()));
    }
    let v = message_screen(frame, error_symbol(kind), &title, &body, &buttons);
    v.setIdentifier(Some(&util::ns("zcLoadError")));
    Retained::into_super(v)
}

/// Ask which profile something should open in.
pub fn pick_profile(question: &str, profiles: &[(Uuid, String)]) -> Option<Uuid> {
    let mtm = mtm();
    let alert = objc2_app_kit::NSAlert::new(mtm);
    alert.setMessageText(&util::ns(question));
    alert.setInformativeText(&util::ns("Each client has its own sign-in. The Google Search workspace has none."));
    alert.addButtonWithTitle(&util::ns("Open"));
    alert.addButtonWithTitle(&util::ns("Cancel"));
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(0.0, 0.0, 260.0, 26.0), false);
    for (_, name) in profiles {
        popup.addItemWithTitle(&util::ns(name));
    }
    popup.setAccessibilityLabel(Some(&util::ns("Client")));
    alert.setAccessoryView(Some(&popup));
    if alert.runModal() != objc2_app_kit::NSAlertFirstButtonReturn {
        return None;
    }
    profiles.get(popup.indexOfSelectedItem().max(0) as usize).map(|p| p.0)
}

fn drag_writer(row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
    let id = row_client(row)?;
    if id == model::GOOGLE_SEARCH_ID || is_filtering() {
        return None;
    }
    let item = NSPasteboardItem::new();
    item.setString_forType(&util::ns(&id.to_string()), &util::ns(CLIENT_DRAG_TYPE));
    Some(ProtocolObject::from_retained(item))
}

fn row_view(table: &NSTableView, row: NSInteger) -> Option<Retained<NSView>> {
    let id = row_client(row)?;
    let width = table.bounds().size.width;
    app::store(|s| s.client(id).map(|c| client_row(c, width, s.active_client_id == Some(id))))
}

fn actions() -> Retained<Actions> {
    ACTIONS.with(|a| {
        a.get_or_init(|| {
            let this = Actions::alloc(mtm()).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

fn target() -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(actions()))
}

fn add_item(menu: &NSMenu, title: &str, action: Sel, key: &str, tag: isize) -> Retained<NSMenuItem> {
    let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&util::ns(title), Some(action), &util::ns(key)) };
    unsafe { item.setTarget(Some(&target())) };
    item.setTag(tag);
    item
}

fn sort_menu() -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm());
    let current = app::store(|s| s.sort_mode);
    for (i, mode) in SortMode::ALL.iter().enumerate() {
        let item = add_item(&menu, mode.label(), sel!(setSort:), "", i as isize);
        item.setState(if *mode == current { NSControlStateValueOn } else { NSControlStateValueOff });
    }
    menu
}

// ---------------------------------------------------------------------------
// The window.
// ---------------------------------------------------------------------------

struct Ui {
    window: Retained<NSWindow>,
    root: Retained<FlexView>,
    sidebar: Retained<NSVisualEffectView>,
    side: Retained<FlexView>,
    side_title: Retained<NSTextField>,
    sort_button: Retained<NSButton>,
    filter_field: Retained<NSSearchField>,
    table: Retained<NSTableView>,
    table_scroll: Retained<NSScrollView>,
    count_label: Retained<NSTextField>,
    add_button: Retained<NSButton>,
    toolbar: Retained<FlexView>,
    chip: RefCell<Option<Retained<Tile>>>,
    chip_key: RefCell<String>,
    /// Shows or hides the client list; first in the toolbar, before the
    /// client's name.
    sidebar_button: Retained<NSButton>,
    back: Retained<NSButton>,
    forward: Retained<NSButton>,
    reload: Retained<NSButton>,
    home: Retained<NSButton>,
    address: Retained<NSTextField>,
    /// Lock for https, globe for anything else, just before the address.
    address_icon: Retained<NSImageView>,
    right_buttons: Vec<Retained<NSButton>>,
    downloads_button: Retained<NSButton>,
    tabs: Retained<FlexView>,
    /// Where a dragged tab will land (see `show_drop_marker`).
    drop_marker: Retained<NSView>,
    /// The colour the tab strip is washed with now.
    tab_tint: RefCell<Option<String>>,
    chips: RefCell<Vec<Retained<TabChip>>>,
    tab_plus: Retained<NSButton>,
    tab_overflow: Retained<NSButton>,
    media_button: Retained<NSButton>,
    /// The toolbar's Google Meet button; locked while a meeting is open.
    meet_button: Retained<NSButton>,
    find: Retained<FlexView>,
    find_field: Retained<NSSearchField>,
    find_status: Retained<NSTextField>,
    find_done: Retained<NSButton>,
    separator: Retained<NSView>,
    /// The 2 pt accent line that grows while the page in front loads.
    progress: Retained<NSView>,
    /// How far along that line is (0..1), and whether it is showing.
    progress_value: Cell<f64>,
    progress_fading: Cell<bool>,
    content: Retained<NSView>,
    launcher: RefCell<Option<(Uuid, Retained<NSView>)>>,
    sidebar_visible: Cell<bool>,
    find_visible: Cell<bool>,
    /// The tab whose address the address bar shows.
    address_tab: Cell<Option<Uuid>>,
    rows: RefCell<Vec<Uuid>>,
    filter: RefCell<String>,
}

fn with_ui<R>(f: impl FnOnce(&Ui) -> R) -> Option<R> {
    UI.with_borrow(|u| u.as_ref().map(f))
}

fn row_client(row: NSInteger) -> Option<Uuid> {
    if row < 0 {
        return None;
    }
    with_ui(|u| u.rows.borrow().get(row as usize).copied()).flatten()
}

fn is_filtering() -> bool {
    with_ui(|u| !u.filter.borrow().is_empty()).unwrap_or(false)
}

/// A one-line label in one of the app's text styles (see `theme.rs`).
pub(crate) fn text(s: &str, style: theme::Text, weight: theme::Weight) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&util::ns(s), mtm());
    l.setFont(Some(&theme::font_weight(style, weight)));
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l
}

/// A small-caps section label (CLIENTS, CRM…) with a little tracking.
pub(crate) fn section_label(s: &str) -> Retained<NSTextField> {
    use objc2_foundation::{NSAttributedString, NSDictionary, NSNumber, NSString};
    let l = text(s, theme::Text::Caption, theme::Weight::Semibold);
    let keys: [&NSString; 3] = unsafe { [objc2_app_kit::NSFontAttributeName, objc2_app_kit::NSForegroundColorAttributeName, objc2_app_kit::NSKernAttributeName] };
    let values: [Retained<AnyObject>; 3] =
        [util::any(theme::font_weight(theme::Text::Caption, theme::Weight::Semibold)), util::any(theme::secondary_text()), util::any(NSNumber::new_f64(theme::CAPTION_TRACKING))];
    let attrs = NSDictionary::from_retained_objects(&keys, &values);
    let attributed = unsafe { NSAttributedString::initWithString_attributes(<NSAttributedString as objc2::AllocAnyThread>::alloc(), &util::ns(s), Some(&attrs)) };
    l.setAttributedStringValue(&attributed);
    l
}

/// A label at an exact point size: only for initials drawn on a badge,
/// whose size follows the badge, not the text styles.
pub(crate) fn label(text: &str, size: f64, bold: bool) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&util::ns(text), mtm());
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    l.setFont(Some(&font));
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l
}

/// SF Symbols images, made once each: every tab chip's close button and the
/// toolbar's reload/stop icon used to build a fresh image on each refresh.
/// Toolbar-size symbols; every icon goes through `theme::symbol`, so one
/// weight and scale is used everywhere.
pub(crate) fn symbol(name: &str) -> Option<Retained<NSImage>> {
    theme::symbol(name, theme::Icon::Toolbar)
}

pub(crate) fn sym_button(name: &str, tip: &str, action: Sel) -> Retained<NSButton> {
    sym_button_sized(name, tip, action, theme::Icon::Toolbar)
}

pub(crate) fn sym_button_sized(name: &str, tip: &str, action: Sel, icon: theme::Icon) -> Retained<NSButton> {
    let image = theme::symbol(name, icon).unwrap_or_default();
    let b = unsafe { NSButton::buttonWithImage_target_action(&image, Some(&target()), Some(action), mtm()) };
    // A rounded background appears under the pointer, like toolbar items.
    b.setBezelStyle(objc2_app_kit::NSBezelStyle::Toolbar);
    b.setBordered(true);
    b.setShowsBorderOnlyWhileMouseInside(true);
    b.setToolTip(Some(&util::ns(tip)));
    set_ax_label(&b, tip.split(" (").next().unwrap_or(tip));
    b.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    b
}

/// Name a button for VoiceOver. AppKit reads a button's name from its cell
/// (otherwise from the image: an SF Symbol's own name, e.g. "refresh"), so
/// the label goes on both.
pub(crate) fn set_ax_label(b: &NSButton, text: &str) {
    let label = util::ns(text);
    b.setAccessibilityLabel(Some(&label));
    if let Some(cell) = b.cell() {
        cell.setAccessibilityLabel(Some(&label));
    }
}

/// Text in one font and colour, for menu items and labels that need more
/// than the default look.
fn attributed(text: &str, font: &NSFont, color: &NSColor) -> Retained<objc2_foundation::NSAttributedString> {
    use objc2_foundation::{NSAttributedString, NSDictionary, NSString};
    let keys: [&NSString; 2] = unsafe { [objc2_app_kit::NSFontAttributeName, objc2_app_kit::NSForegroundColorAttributeName] };
    let values: [Retained<AnyObject>; 2] = [util::any(objc2::Message::retain(font)), util::any(objc2::Message::retain(color))];
    let attrs = NSDictionary::from_retained_objects(&keys, &values);
    unsafe { NSAttributedString::initWithString_attributes(<NSAttributedString as objc2::AllocAnyThread>::alloc(), &util::ns(text), Some(&attrs)) }
}

thread_local! {
    /// Every layer whose colour was copied from an AppKit colour: a CGColor
    /// is a snapshot, so these are re-resolved when the appearance or the
    /// accent colour changes (see `reapply_layer_colors`).
    static LAYER_COLORS: RefCell<std::collections::HashMap<(usize, bool), Painted>> = RefCell::new(std::collections::HashMap::new());
    /// How many entries there were after the last sweep of dead views.
    static LAYER_SWEPT: Cell<usize> = const { Cell::new(64) };
}

/// A view, the colour its layer was given, and whether that is its border.
type Painted = (objc2::rc::Weak<NSView>, Retained<NSColor>, bool);

pub(crate) fn set_layer_color(view: &NSView, color: &NSColor, radius: f64) {
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setCornerRadius(radius);
    }
    remember_paint(view, color, false);
}

/// A 1 pt (or `width`) border in `color`, kept right across appearance
/// changes like the background.
pub(crate) fn set_layer_border(view: &NSView, color: &NSColor, width: f64) {
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setBorderWidth(width);
    }
    remember_paint(view, color, true);
}

fn remember_paint(view: &NSView, color: &NSColor, border: bool) {
    paint_layer(view, color, border);
    LAYER_COLORS.with_borrow_mut(|m| {
        m.insert((view as *const NSView as usize, border), (objc2::rc::Weak::from(view), objc2::Message::retain(color), border));
        // Chips come and go: drop dead views once the map has doubled.
        if m.len() > LAYER_SWEPT.get() * 2 {
            m.retain(|_, (w, _, _)| w.load().is_some());
            LAYER_SWEPT.set(m.len().max(64));
        }
    });
}

/// Resolve `color` the way it looks in the view's own appearance (light,
/// dark, high contrast, current accent) and put it on the layer.
fn paint_layer(view: &NSView, color: &NSColor, border: bool) {
    let Some(layer) = view.layer() else { return };
    let set = block2::RcBlock::new(|| {
        let cg = color.CGColor();
        if border { layer.setBorderColor(Some(&cg)) } else { layer.setBackgroundColor(Some(&cg)) }
    });
    objc2_app_kit::NSAppearanceCustomization::effectiveAppearance(view).performAsCurrentDrawingAppearance(&set);
}

/// The Mac switched light/dark (or contrast), or the accent colour changed:
/// every layer colour is resolved again, and the chips that bake colours
/// into their views are rebuilt.
pub fn reapply_layer_colors() {
    let live: Vec<(Retained<NSView>, Retained<NSColor>, bool)> = LAYER_COLORS.with_borrow_mut(|m| {
        m.retain(|_, (w, _, _)| w.load().is_some());
        m.values().filter_map(|(w, c, b)| Some((w.load()?, c.clone(), *b))).collect()
    });
    for (view, color, border) in live {
        paint_layer(&view, &color, border);
    }
    with_ui(|u| {
        u.chip_key.replace(String::new());
        for c in u.chips.borrow().iter() {
            c.ivars().rendered.replace(String::new());
            c.ivars().styled.set(0xff);
        }
    });
    refresh(app::ALL);
}

/// Watch for accent colour and appearance changes.
fn watch_appearance() {
    let center = objc2_foundation::NSNotificationCenter::defaultCenter();
    let block = block2::RcBlock::new(|_n: std::ptr::NonNull<NSNotification>| util::later(reapply_layer_colors));
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(objc2_app_kit::NSSystemColorsDidChangeNotification), None, None, &block) };
    std::mem::forget(token);
}

/// The coloured tile: an industry symbol when the profile has one, otherwise
/// its initials. The glyph is white or near-black, whichever reads at 4.5:1
/// on the tile's colour (white failed on amber, teal, lime, green...).
pub(crate) fn badge(hex: &str, symbol_name: Option<&str>, initials: &str, size: f64) -> Retained<NSView> {
    let v = NSView::initWithFrame(NSView::alloc(mtm()), rect(0.0, 0.0, size, size));
    set_layer_color(&v, &util::color(hex), size * 0.27);
    let glyph = util::color(theme::glyph_on(hex));
    if let Some(img) = symbol_name.and_then(symbol) {
        let iv = NSImageView::imageViewWithImage(&img, mtm());
        iv.setContentTintColor(Some(&glyph));
        let inset = size * 0.22;
        iv.setFrame(rect(inset, inset, size - inset * 2.0, size - inset * 2.0));
        v.addSubview(&iv);
    } else {
        // Two initials at a small size came out around 7.5 pt: a small
        // tile shows one, at 9 pt or more.
        let small = size < 24.0;
        let text: String = if small { initials.chars().take(1).collect() } else { initials.to_string() };
        let l = label(&text, if small { (size * 0.5).max(9.0) } else { size * 0.42 }, true);
        l.setTextColor(Some(&glyph));
        l.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        let h = size * 0.6;
        l.setFrame(rect(0.0, (size - h) / 2.0 - 1.0, size, h));
        v.addSubview(&l);
    }
    v
}

/// One sidebar (and switcher) row: badge, name, tab count. The selected
/// client also gets a 3 pt bar in its own colour on the leading edge, so
/// which client is in front never depends on the system highlight alone.
pub(crate) fn client_row(c: &model::Client, width: f64, selected: bool) -> Retained<NSView> {
    use theme::{BADGE_TEXT_GAP as GAP, SIDEBAR_BADGE as BADGE, SIDEBAR_ROW_H as H, SIDEBAR_ROW_PAD as PAD};
    let row = flex(K_PLAIN);
    row.setFrame(rect(0.0, 0.0, width, H));
    if selected {
        let bar = NSView::initWithFrame(NSView::alloc(mtm()), rect(0.0, theme::GRID * 2.0, theme::SELECTION_BAR_W, H - theme::GRID * 4.0));
        let hex = if c.is_search() { theme::SEARCH_BLUE } else { c.color_hex.as_str() };
        set_layer_color(&bar, &util::color(hex), theme::SELECTION_BAR_W / 2.0);
        bar.setAccessibilityElement(false);
        row.addSubview(&bar);
    }
    let sym = if c.is_search() { Some("magnifyingglass") } else { c.symbol_name.as_deref() };
    let b = badge(&c.color_hex, sym, &c.initials(), BADGE);
    b.setFrame(rect(PAD, (H - BADGE) / 2.0, BADGE, BADGE));
    row.addSubview(&b);
    let x = PAD + BADGE + GAP;
    let name = text(&c.name, theme::Text::Headline, theme::Weight::Semibold);
    name.setFrame(rect(x, theme::GRID + 1.0, width - x - theme::GRID * 2.0, 18.0));
    name.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    row.addSubview(&name);
    let n = c.tabs.len();
    let sub_text = if c.is_search() { "Independent search".to_string() } else { format!("{n} tab{}", if n == 1 { "" } else { "s" }) };
    let sub = text(&sub_text, theme::Text::Footnote, theme::Weight::Regular);
    sub.setTextColor(Some(&theme::secondary_text()));
    sub.setFrame(rect(x, theme::GRID * 6.0 - 1.0, width - x - theme::GRID * 2.0, 16.0));
    sub.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    row.addSubview(&sub);
    // One element for VoiceOver: "Acme, 3 tabs" rather than three pieces.
    for v in [&*b, &*name as &NSView, &*sub] {
        v.setAccessibilityElement(false);
    }
    row.setAccessibilityElement(true);
    row.setAccessibilityRole(Some(&util::ns("AXCell")));
    row.setAccessibilityLabel(Some(&util::ns(&format!("{}, {sub_text}", c.name))));
    Retained::into_super(Retained::into_super(row)).downcast::<NSView>().ok().unwrap_or_else(|| NSView::new(mtm()))
}

pub fn build() {
    let mtm = mtm();
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 1320.0, 860.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&util::ns("ZClients"));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setMinSize(NSSize::new(980.0, 620.0));
    window.center();
    window.setFrameAutosaveName(&util::ns("ZClientsMainWindow"));
    window.setDelegate(Some(ProtocolObject::from_ref(&*actions())));

    let root = flex(K_ROOT);
    window.setContentView(Some(&root));

    // ---- sidebar ----
    let sidebar = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect(0.0, 0.0, SIDEBAR_W, 800.0));
    sidebar.setMaterial(NSVisualEffectMaterial::Sidebar);
    sidebar.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    sidebar.setState(NSVisualEffectState::FollowsWindowActiveState);
    let side = flex(K_SIDEBAR);
    side.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    sidebar.addSubview(&side);

    let side_title = section_label("CLIENTS");
    side_title.setTextColor(Some(&NSColor::secondaryLabelColor()));
    side.addSubview(&side_title);
    let sort_button = sym_button("arrow.up.arrow.down", "Sort clients", sel!(sortMenu:));
    side.addSubview(&sort_button);

    let filter_field = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 200.0, 24.0));
    filter_field.setPlaceholderString(Some(&util::ns("Search clients")));
    filter_field.setAccessibilityLabel(Some(&util::ns("Search clients")));
    if let Some(cell) = filter_field.cell() {
        cell.setAccessibilityLabel(Some(&util::ns("Search clients")));
    }
    unsafe {
        filter_field.setTarget(Some(&target()));
        filter_field.setAction(Some(sel!(filterChanged:)));
    }
    filter_field.setSendsSearchStringImmediately(true);
    side.addSubview(&filter_field);

    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, SIDEBAR_W, 400.0));
    let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &util::ns("client"));
    column.setWidth(SIDEBAR_W - 20.0);
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setRowHeight(44.0);
    table.setStyle(NSTableViewStyle::SourceList);
    table.setBackgroundColor(&NSColor::clearColor());
    table.setColumnAutoresizingStyle(objc2_app_kit::NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle);
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*actions())));
        table.setDelegate(Some(ProtocolObject::from_ref(&*actions())));
        table.registerForDraggedTypes(&NSArray::from_retained_slice(&[util::ns(CLIENT_DRAG_TYPE)]));
        table.setDraggingSourceOperationMask_forLocal(NSDragOperation::Move, true);
    }
    let client_menu = NSMenu::new(mtm);
    client_menu.setDelegate(Some(ProtocolObject::from_ref(&*actions())));
    unsafe { table.setMenu(Some(&client_menu)) };
    let table_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, SIDEBAR_W, 400.0));
    table_scroll.setDocumentView(Some(&table));
    table_scroll.setHasVerticalScroller(true);
    table_scroll.setAutohidesScrollers(true);
    table_scroll.setDrawsBackground(false);
    side.addSubview(&table_scroll);

    let count_label = text("", theme::Text::Footnote, theme::Weight::Regular);
    count_label.setFont(Some(&theme::digits(theme::Text::Footnote)));
    count_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    side.addSubview(&count_label);
    let add_button = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Add Client"), Some(&target()), Some(sel!(addClient:)), mtm) };
    add_button.setBordered(false);
    if let Some(img) = symbol("plus.circle.fill") {
        add_button.setImage(Some(&img));
        add_button.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
        set_ax_label(&add_button, "Add Client");
    }
    add_button.setContentTintColor(Some(&NSColor::controlAccentColor()));
    side.addSubview(&add_button);
    root.addSubview(&sidebar);

    // ---- toolbar ----
    let toolbar = flex(K_TOOLBAR);
    let sidebar_button = sym_button("sidebar.left", "Hide Clients (⌃S)", sel!(zcToggleSidebar:));
    toolbar.addSubview(&sidebar_button);
    let back = sym_button("chevron.left", "Back (⌘[)", sel!(back:));
    let forward = sym_button("chevron.right", "Forward (⌘])", sel!(forward:));
    let reload = sym_button("arrow.clockwise", "Reload (⌘R)", sel!(reloadOrStop:));
    let home = sym_button("square.grid.2x2", "App launcher (⌘⇧H)", sel!(home:));
    let address: Retained<NSTextField> = {
        let this = AddressField::alloc(mtm).set_ivars(());
        let field: Retained<AddressField> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 400.0, 24.0)] };
        let cell: Retained<AddressCell> = {
            let this = AddressCell::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), initTextCell: &*util::ns("")] }
        };
        field.setCell(Some(&cell));
        Retained::into_super(field)
    };
    address.setStringValue(&util::ns(""));
    address.setEditable(true);
    address.setSelectable(true);
    address.setBezeled(true);
    address.setDrawsBackground(true);
    if let Some(cell) = address.cell() {
        cell.setScrollable(true);
        cell.setWraps(false);
        cell.setUsesSingleLineMode(true);
    }
    address.setPlaceholderString(Some(&util::ns("Search Zoho or enter address")));
    address.setAccessibilityLabel(Some(&util::ns("Address and search")));
    if let Some(cell) = address.cell() {
        cell.setAccessibilityLabel(Some(&util::ns("Address and search")));
    }
    address.setFont(Some(&theme::font(theme::Text::Body)));
    address.setBezelStyle(objc2_app_kit::NSTextFieldBezelStyle::RoundedBezel);
    // A 28 pt field with its text centred.
    address.setControlSize(objc2_app_kit::NSControlSize::Large);
    let address_icon = NSImageView::imageViewWithImage(&theme::symbol("globe", theme::Icon::Tab).unwrap_or_default(), mtm);
    address_icon.setContentTintColor(Some(&theme::secondary_text()));
    unsafe { address.setDelegate(Some(ProtocolObject::from_ref(&*actions()))) };
    let search = sym_button("magnifyingglass", "Google Search workspace", sel!(search:));
    let meet = sym_button("video.fill", "Start a Google Meet meeting for this client", sel!(meet:));
    let downloads_button = sym_button("arrow.down.circle", "Downloads (⌘⇧J)", sel!(downloads:));
    let plus = sym_button("plus", "New tab (⌘T)", sel!(newTab:));
    // Shown while any tab uses the camera or microphone; one click lists
    // them with Stop.
    let media_button = sym_button("video.fill", "Camera or microphone in use", sel!(mediaMenu:));
    media_button.setContentTintColor(Some(&NSColor::systemRedColor()));
    media_button.setHidden(true);
    for b in [&back, &forward, &reload, &home] {
        toolbar.addSubview(b);
    }
    toolbar.addSubview(&address);
    toolbar.addSubview(&address_icon);
    for b in [&media_button, &search, &meet, &downloads_button, &plus] {
        toolbar.addSubview(b);
    }
    root.addSubview(&toolbar);

    // ---- tab strip ----
    let tabs = flex(K_TABS);
    let tab_plus = sym_button("plus", "New tab (⌘T)", sel!(newTab:));
    tabs.addSubview(&tab_plus);
    let tab_overflow = sym_button("chevron.down", "All tabs (⇧⌘A)", sel!(tabOverflow:));
    tab_overflow.setHidden(true);
    tabs.addSubview(&tab_overflow);
    let drop_marker = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 2.0, 20.0));
    set_layer_color(&drop_marker, &NSColor::controlAccentColor(), 1.0);
    drop_marker.setHidden(true);
    drop_marker.setAccessibilityElement(false);
    tabs.addSubview(&drop_marker);
    tabs.setAccessibilityElement(true);
    tabs.setAccessibilityRole(Some(&util::ns("AXTabGroup")));
    tabs.setAccessibilityLabel(Some(&util::ns("Tabs")));
    root.addSubview(&tabs);

    // ---- find bar ----
    let find = flex(K_FIND);
    let find_field = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(0.0, 0.0, 240.0, 24.0));
    find_field.setPlaceholderString(Some(&util::ns("Find in page")));
    find_field.setAccessibilityLabel(Some(&util::ns("Find in page")));
    if let Some(cell) = find_field.cell() {
        cell.setAccessibilityLabel(Some(&util::ns("Find in page")));
    }
    unsafe {
        find_field.setTarget(Some(&target()));
        find_field.setAction(Some(sel!(findEntered:)));
        find_field.setDelegate(Some(ProtocolObject::from_ref(&*actions())));
    }
    let find_status = text("", theme::Text::Footnote, theme::Weight::Regular);
    find_status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let find_done = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Done"), Some(&target()), Some(sel!(findDone:)), mtm) };
    find.addSubview(&find_field);
    find.addSubview(&find_status);
    find.addSubview(&find_done);
    find.setHidden(true);
    root.addSubview(&find);

    let separator = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 1.0));
    set_layer_color(&separator, &NSColor::separatorColor(), 0.0);
    root.addSubview(&separator);

    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 800.0, 600.0));
    root.addSubview(&content);
    let progress = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 0.0, theme::PROGRESS_H));
    set_layer_color(&progress, &NSColor::controlAccentColor(), 1.0);
    progress.setAlphaValue(0.0);
    progress.setAccessibilityElement(false);
    root.addSubview(&progress);

    let ui = Ui {
        window: window.clone(),
        root,
        sidebar,
        side,
        side_title,
        sort_button,
        filter_field,
        table,
        table_scroll,
        count_label,
        add_button,
        toolbar,
        chip: RefCell::new(None),
        chip_key: RefCell::new(String::new()),
        sidebar_button,
        back,
        forward,
        reload,
        home,
        address,
        address_icon,
        right_buttons: vec![media_button.clone(), search, meet.clone(), downloads_button.clone(), plus],
        meet_button: meet,
        downloads_button,
        tabs,
        drop_marker,
        tab_tint: RefCell::new(None),
        chips: RefCell::new(vec![]),
        tab_plus,
        tab_overflow,
        media_button,
        find,
        find_field,
        find_status,
        find_done,
        separator,
        progress,
        progress_value: Cell::new(0.0),
        progress_fading: Cell::new(false),
        content,
        launcher: RefCell::new(None),
        sidebar_visible: Cell::new(true),
        find_visible: Cell::new(false),
        address_tab: Cell::new(None),
        rows: RefCell::new(vec![]),
        filter: RefCell::new(String::new()),
    };
    UI.with_borrow_mut(|u| *u = Some(ui));
    build_menu();
    watch_appearance();
    // Window first, page second: creating the first web view (and starting
    // its web content process) used to happen before anything was on screen.
    refresh(app::SIDEBAR | app::TABS | app::TOOLBAR);
    window.makeKeyAndOrderFront(None);
    util::later(|| {
        web::prewarm_active_profile();
        refresh(app::CONTENT | app::TABS);
    });
    if std::env::var_os("ZCLIENTS_DEBUG").is_some() {
        let f = window.frame();
        let close = window.standardWindowButton(objc2_app_kit::NSWindowButton::CloseButton);
        util::log(format!(
            "window frame {:?} style {:?} close button: {:?} hidden={:?} frame={:?}",
            f,
            window.styleMask(),
            close.is_some(),
            close.as_ref().map(|c| c.isHidden()),
            close.as_ref().map(|c| c.frame())
        ));
        if let Some(screen) = window.screen() {
            util::log(format!("screen {:?}", screen.frame()));
        }
    }
}

// ---------------------------------------------------------------------------
// Layout.
// ---------------------------------------------------------------------------

fn layout(kind: u8, view: &FlexView) {
    let b = view.bounds().size;
    with_ui(|u| match kind {
        K_ROOT => {
            let side_w = if u.sidebar_visible.get() { SIDEBAR_W } else { 0.0 };
            u.sidebar.setHidden(side_w == 0.0);
            u.sidebar.setFrame(rect(0.0, 0.0, SIDEBAR_W, b.height));
            let mw = b.width - side_w;
            u.toolbar.setFrame(rect(side_w, 0.0, mw, TOP));
            u.tabs.setFrame(rect(side_w, TOP, mw, TAB_H));
            let mut y = TOP + TAB_H;
            u.find.setHidden(!u.find_visible.get());
            if u.find_visible.get() {
                u.find.setFrame(rect(side_w, y, mw, FIND_H));
                y += FIND_H;
            }
            u.separator.setFrame(rect(side_w, y, mw, 1.0));
            u.progress.setFrame(rect(side_w, y - theme::PROGRESS_H + 1.0, mw * u.progress_value.get(), theme::PROGRESS_H));
            u.content.setFrame(rect(side_w, y + 1.0, mw, b.height - y - 1.0));
        }
        K_SIDEBAR => {
            use theme::GRID as G;
            u.side.setFrame(u.sidebar.bounds());
            // Below the traffic lights; everything on the 4-pt grid.
            u.side_title.setFrame(rect(G * 5.0, G * 11.0, b.width - G * 16.0, 16.0));
            u.sort_button.setFrame(rect(b.width - G * 3.0 - theme::ICON_BUTTON, G * 10.0, theme::ICON_BUTTON, G * 6.0));
            u.filter_field.setFrame(rect(G * 3.0, G * 17.0, b.width - G * 6.0, theme::FIELD_H));
            let list_top = G * 25.0;
            u.table_scroll.setFrame(rect(0.0, list_top, b.width, b.height - list_top - G * 10.0));
            u.count_label.setFrame(rect(G * 4.0, b.height - G * 7.0, 100.0, 16.0));
            u.add_button.setFrame(rect(b.width - 116.0, b.height - G * 8.0 - 1.0, 104.0, G * 6.0));
        }
        K_TOOLBAR => {
            use theme::{GRID as G, ICON_BUTTON as BTN, TOOLBAR_GAP as GAP};
            let y = (TOP - BTN) / 2.0;
            // Leave room for the traffic lights when the sidebar is hidden.
            let mut x = if u.sidebar_visible.get() { G * 3.0 } else { G * 21.0 };
            u.sidebar_button.setFrame(rect(x, y, BTN, BTN));
            x += BTN + G;
            if let Some(chip) = u.chip.borrow().as_ref() {
                let w = chip.frame().size.width;
                chip.setFrame(rect(x, y, w, BTN));
                x += w + GAP;
            }
            for btn in [&u.back, &u.forward, &u.reload, &u.home] {
                btn.setFrame(rect(x, y, BTN, BTN));
                x += BTN + G;
            }
            x += GAP - G;
            let shown: Vec<&Retained<NSButton>> = u.right_buttons.iter().filter(|b| !b.isHidden()).collect();
            let right_w = shown.len() as f64 * (BTN + G) - G + GAP * 2.0;
            let addr_w = (b.width - x - right_w).max(120.0);
            let ay = (TOP - theme::ADDRESS_H) / 2.0;
            // The lock (https), globe or magnifying glass sits inside the
            // field, in the room its cell leaves (theme::ADDRESS_ICON_INSET).
            let fx = x;
            u.address.setFrame(rect(fx, ay, addr_w, theme::ADDRESS_H));
            u.address_icon.setFrame(rect(fx + 8.0, ay + (theme::ADDRESS_H - 16.0) / 2.0, 16.0, 16.0));
            let mut rx = fx + addr_w + GAP;
            for btn in shown {
                btn.setFrame(rect(rx, y, BTN, BTN));
                rx += BTN + G;
            }
        }
        K_TABS => {
            // However many tabs there are, the active one, the + button and
            // (when some do not fit) the All Tabs menu stay on screen.
            let chips = u.chips.borrow();
            let active = chips.iter().position(|c| c.ivars().ax_selected.get()).unwrap_or(0);
            let (start, count, w) = tab_window(chips.len(), active, b.width);
            let overflow = count < chips.len();
            let mut x = 8.0;
            for (i, c) in chips.iter().enumerate() {
                let visible = i >= start && i < start + count;
                c.setHidden(!visible);
                if visible {
                    c.setFrame(rect(x, theme::GRID - 1.0, w - theme::GRID, TAB_H - theme::GRID * 2.0 + 2.0));
                    layout_chip(c);
                    x += w;
                }
            }
            u.tab_plus.setFrame(rect(x + 2.0, 4.0, 26.0, 26.0));
            u.tab_overflow.setHidden(!overflow);
            u.tab_overflow.setFrame(rect(x + 30.0, 4.0, 26.0, 26.0));
        }
        K_FIND => {
            u.find_field.setFrame(rect(12.0, 6.0, 260.0, 24.0));
            u.find_status.setFrame(rect(282.0, 10.0, 160.0, 16.0));
            u.find_done.setFrame(rect(b.width - 80.0, 5.0, 68.0, 26.0));
        }
        _ => {}
    });
}

/// Which tabs fit in a strip `width` wide: (first shown, how many, width
/// each). Tabs never get narrower than a readable minimum; when they would,
/// a window of tabs around the active one is shown instead.
pub fn tab_window(n: usize, active: usize, width: f64) -> (usize, usize, f64) {
    const MIN_W: f64 = 96.0;
    const MAX_W: f64 = 210.0;
    let n = n.max(1);
    let avail = (width - 16.0 - 34.0).max(MIN_W);
    if avail / n as f64 >= MIN_W {
        return (0, n, (avail / n as f64).min(MAX_W));
    }
    let avail = (avail - 30.0).max(MIN_W);
    let count = ((avail / MIN_W).floor() as usize).clamp(1, n);
    let start = active.saturating_sub(count / 2).min(n - count);
    (start, count, avail / count as f64)
}

/// A chip's parts are found by identifier: "lead" (colour dot or call
/// icon), "moon" (asleep), "title", "close", "divider".
fn chip_part(chip: &TabChip, id: &str) -> Option<Retained<NSView>> {
    chip.subviews().iter().find(|v| v.identifier().is_some_and(|i| i.to_string() == id))
}

fn layout_chip(chip: &TabChip) {
    let b = chip.bounds().size;
    let mut x = 10.0;
    if let Some(lead) = chip_part(chip, "lead") {
        if lead.isKindOfClass(<NSImageView as objc2::ClassType>::class()) {
            lead.setFrame(rect(7.0, (b.height - 14.0) / 2.0, 14.0, 14.0));
        } else {
            lead.setFrame(rect(10.0, (b.height - 8.0) / 2.0, 8.0, 8.0));
        }
        x = 24.0;
    }
    if let Some(moon) = chip_part(chip, "moon") {
        moon.setFrame(rect(x, (b.height - 14.0) / 2.0, 14.0, 14.0));
        x += 16.0;
    }
    let close_shown = chip_part(chip, "close").is_some_and(|c| !c.isHidden());
    let right = if close_shown { 24.0 } else { 8.0 };
    if let Some(title) = chip_part(chip, "title") {
        let h = title.fittingSize().height.ceil().max(14.0);
        title.setFrame(rect(x, (b.height - h) / 2.0, (b.width - x - right).max(10.0), h));
    }
    if let Some(close) = chip_part(chip, "close") {
        close.setFrame(rect(b.width - 22.0, (b.height - 18.0) / 2.0, 18.0, 18.0));
    }
    if let Some(d) = chip_part(chip, "divider") {
        d.setFrame(rect(b.width + 1.5, 7.0, 1.0, b.height - 14.0));
    }
}

/// Fill, close button and divider for the chip's current state (selected,
/// under the pointer, or neither); no rebuild.
fn style_chip(chip: &TabChip) {
    let iv = chip.ivars();
    let (active, hover) = (iv.ax_selected.get(), iv.hover.get());
    // Restyling every chip on every page-title change was most of the tab
    // strip's cost: only a change of state redraws.
    let state = u8::from(active) | u8::from(hover) << 1 | u8::from(iv.closable.get()) << 2 | u8::from(iv.divider.get()) << 3;
    if iv.styled.replace(state) == state {
        return;
    }
    let fill = if active {
        theme::selected_tab_fill()
    } else if hover {
        theme::hover_fill()
    } else {
        NSColor::clearColor()
    };
    set_layer_color(chip, &fill, theme::CHIP_RADIUS);
    if let Some(close) = chip_part(chip, "close") {
        // Safari: × only on the selected tab and under the pointer.
        // Keyboard Delete and the tab menu (VoiceOver too) still close.
        close.setHidden(!(iv.closable.get() && (active || hover)));
    }
    if let Some(d) = chip_part(chip, "divider") {
        d.setHidden(!iv.divider.get() || hover);
    }
    layout_chip(chip);
}

/// The tab a dragged tab would land on if released at window point `p`,
/// and the chip's frame in the strip; None unless `p` is over the strip.
fn drop_target(p: NSPoint) -> Option<(Uuid, NSRect)> {
    with_ui(|u| {
        let in_strip = u.tabs.convertPoint_fromView(p, None);
        let b = u.tabs.bounds();
        if in_strip.y < 0.0 || in_strip.y > b.size.height || in_strip.x < 0.0 || in_strip.x > b.size.width {
            return None;
        }
        u.chips.borrow().iter().filter(|c| !c.isHidden()).find_map(|c| {
            let f = c.frame();
            (in_strip.x >= f.origin.x && in_strip.x <= f.origin.x + f.size.width).then(|| (c.ivars().tab, f))
        })
    })
    .flatten()
}

/// While a tab is dragged over the strip, a thin accent line shows where it
/// will go: before the tab under the pointer when moving left, after it
/// when moving right.
fn show_drop_marker(dragging: Uuid, p: NSPoint) {
    let target = drop_target(p);
    with_ui(|u| {
        let chips = u.chips.borrow();
        let from = chips.iter().position(|c| c.ivars().tab == dragging);
        match (target, from) {
            (Some((tab, f)), Some(from)) if tab != dragging => {
                let to = chips.iter().position(|c| c.ivars().tab == tab).unwrap_or(from);
                let x = if to < from { f.origin.x - 2.0 } else { f.origin.x + f.size.width };
                u.drop_marker.setFrame(rect(x, 4.0, 2.0, theme::TAB_STRIP_H - 8.0));
                u.drop_marker.setHidden(false);
            }
            _ => u.drop_marker.setHidden(true),
        }
    });
}

fn hide_drop_marker() {
    with_ui(|u| u.drop_marker.setHidden(true));
}

// ---------------------------------------------------------------------------
// Refresh.
// ---------------------------------------------------------------------------

pub fn refresh(what: u8) {
    if what & app::SIDEBAR != 0 {
        refresh_sidebar();
    }
    if what & app::CONTENT != 0 {
        refresh_content();
    }
    if what & app::TABS != 0 {
        refresh_tabs();
    }

    if what & (app::TOOLBAR | app::CONTENT) != 0 {
        refresh_toolbar();
    }
}

fn refresh_sidebar() {
    let filter = with_ui(|u| u.filter.borrow().clone()).unwrap_or_default();
    let (rows, active, count) = app::store(|s| {
        let rows: Vec<Uuid> = s.ordered().iter().filter(|c| filter.is_empty() || c.name.to_lowercase().contains(&filter)).map(|c| c.id).collect();
        let count = s.clients.iter().filter(|c| !c.is_search()).count();
        (rows, s.active_client_id, count)
    });
    let selected = active.and_then(|a| rows.iter().position(|r| *r == a));
    with_ui(|u| {
        u.rows.replace(rows);
        SUPPRESS_SELECTION.set(true);
        u.table.reloadData();
        match selected {
            Some(i) => u.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(i), false),
            None => unsafe { u.table.deselectAll(None) },
        }
        SUPPRESS_SELECTION.set(false);
        u.count_label.setStringValue(&util::ns(&format!("{count} client{}", if count == 1 { "" } else { "s" })));
    });
}

fn refresh_tabs() {
    let data = app::store(|s| {
        s.active_client().map(|c| {
            let tabs: Vec<(model::Tab, bool)> = c.tabs.iter().map(|t| (t.clone(), s.can_close_tab(c.id, t.id))).collect();
            (c.id, c.active_tab_id, tabs)
        })
    });
    // The strip carries a faint wash of the client's colour (the Search
    // workspace stays neutral blue): acting in the wrong client is this
    // app's costliest mistake.
    let tint = app::store(|s| s.active_client().map(|c| if c.is_search() { theme::SEARCH_BLUE.to_string() } else { c.color_hex.clone() }));
    with_ui(|u| {
        if u.tab_tint.replace(tint.clone()) != tint {
            let wash = tint.as_deref().map(theme::client_tint).unwrap_or_else(NSColor::clearColor);
            set_layer_color(&u.tabs, &wash, 0.0);
        }
    });
    let live = web::live_tabs();
    let mtm = mtm();
    let (client, active, tabs) = data.unwrap_or((Uuid::nil(), None, vec![]));

    // Same tabs in the same order: restyle the existing chips in place. A
    // chip the user is pressing or dragging must stay the same view, or the
    // drag is lost the moment the click selects that tab.
    let existing: Vec<(Uuid, Uuid)> = with_ui(|u| u.chips.borrow().iter().map(|c| (c.ivars().client, c.ivars().tab)).collect()).unwrap_or_default();
    let wanted: Vec<(Uuid, Uuid)> = tabs.iter().map(|(t, _)| (client, t.id)).collect();
    let chips: Vec<Retained<TabChip>> = if existing == wanted {
        with_ui(|u| u.chips.borrow().clone()).unwrap_or_default()
    } else {
        tabs.iter()
            .map(|(t, _)| {
                let this = TabChip::alloc(mtm).set_ivars(ChipIvars {
                    client,
                    tab: t.id,
                    down: Cell::new(None),
                    dragged: Cell::new(false),
                    rendered: RefCell::new(String::new()),
                    ax_label: RefCell::new(String::new()),
                    ax_selected: Cell::new(false),
                    hover: Cell::new(false),
                    closable: Cell::new(false),
                    divider: Cell::new(false),
                    styled: Cell::new(0xff),
                });
                let chip: Retained<TabChip> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 150.0, 28.0)] };
                chip
            })
            .collect()
    };

    for (i, ((t, closable), chip)) in tabs.iter().zip(chips.iter()).enumerate() {
        let is_active = Some(t.id) == active;
        // A sleeping tab wakes, signed in and at its place, when selected;
        // until then its title is dimmed and a small moon says why.
        let asleep = t.url.is_some() && !live.contains(&t.id) && !is_active;
        if !chip.ivars().dragged.get() {
            chip.setAlphaValue(1.0);
        }
        // Pages retitle themselves constantly (unread counts, loading);
        // rebuilding every chip each time was the tab strip's main cost.
        let title = t.display_title();
        let color = tab_color(t);
        let call = web::call_state(t.id);
        // State in words, never by colour alone.
        let mut ax = title.clone();
        if let Some(path) = t.pdf_path.as_deref() {
            let kind = crate::preview::kind_by_name(std::path::Path::new(path)).unwrap_or(crate::preview::Kind::Pdf);
            ax.push_str(&format!(", {}", kind.word()));
        }
        if let Some((camera, mic)) = call {
            ax.push_str(match (camera, mic) {
                (true, true) => ", camera and microphone on",
                (true, false) => ", camera on",
                _ => ", microphone on",
            });
        }
        if asleep {
            ax.push_str(", asleep");
        }
        chip.ivars().ax_label.replace(ax);
        chip.ivars().ax_selected.set(is_active);
        chip.ivars().closable.set(*closable);
        // A divider between two unselected tabs only.
        let next_active = tabs.get(i + 1).is_some_and(|(n, _)| Some(n.id) == active);
        chip.ivars().divider.set(!is_active && !next_active && i + 1 < tabs.len());
        let key = format!("{i}|{closable}|{color}|{title}|{call:?}|{asleep}");
        if *chip.ivars().rendered.borrow() == key {
            style_chip(chip);
            continue;
        }
        for v in chip.subviews().iter() {
            v.removeFromSuperview();
        }
        let part = |v: &NSView, id: &str| {
            v.setIdentifier(Some(&util::ns(id)));
            chip.addSubview(v);
        };
        // A tab in a call shows a red camera (or microphone) instead of its
        // colour dot, so the call is easy to find among the tabs.
        if let Some((camera, _)) = call {
            let icon = NSImageView::imageViewWithImage(&theme::symbol(if camera { "video.fill" } else { "mic.fill" }, theme::Icon::Tab).unwrap_or_default(), mtm);
            icon.setContentTintColor(Some(&NSColor::systemRedColor()));
            part(&icon, "lead");
        } else {
            let dot = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 8.0, 8.0));
            set_layer_color(&dot, &util::color(&color), 4.0);
            part(&dot, "lead");
        }
        if asleep {
            let moon = NSImageView::imageViewWithImage(&theme::symbol("moon.zzz", theme::Icon::Tab).unwrap_or_default(), mtm);
            moon.setContentTintColor(Some(&theme::secondary_text()));
            part(&moon, "moon");
        }
        // Every tab title has the same weight: bold on the selected one
        // changed its width and where it was cut off. Selection shows in
        // the background only.
        let label = text(&title, theme::Text::Callout, theme::Weight::Medium);
        if asleep {
            label.setTextColor(Some(&theme::secondary_text()));
        }
        part(&label, "title");
        let host = t.parsed_url().filter(|u| u.scheme() != "file").and_then(|u| u.host_str().map(str::to_string));
        let mut tip = title.clone();
        if let Some(h) = host {
            tip.push('\n');
            tip.push_str(&h);
        }
        if call.is_some() {
            tip.push_str("\nIn a call");
        }
        if asleep {
            tip.push_str("\nAsleep to save memory — click to wake");
        }
        chip.setToolTip(Some(&util::ns(&tip)));
        if *closable {
            let close = sym_button_sized("xmark", "Close tab (⌘W)", sel!(tabClose:), theme::Icon::Tab);
            close.setTag(i as isize);
            set_ax_label(&close, &format!("Close {title}"));
            part(&close, "close");
        }
        let divider = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 1.0, 10.0));
        set_layer_color(&divider, &theme::separator(), 0.0);
        part(&divider, "divider");
        chip.ivars().rendered.replace(key);
        chip.ivars().styled.set(0xff);
        style_chip(chip);
    }

    if existing != wanted {
        with_ui(|u| {
            for c in u.chips.replace(vec![]) {
                c.removeFromSuperview();
            }
            for c in &chips {
                u.tabs.addSubview(c);
            }
            // Keep the drop marker above the chips.
            u.tabs.addSubview(&u.drop_marker);
            u.chips.replace(chips);
        });
    }
    let calls = web::tabs_in_calls();
    refresh_meet_button();
    with_ui(|u| {
        u.tabs.setNeedsLayout(true);
        let hidden = calls.is_empty();
        if u.media_button.isHidden() != hidden {
            u.media_button.setHidden(hidden);
            u.toolbar.setNeedsLayout(true);
        }
        if !hidden {
            let (cam, mic) = calls.iter().fold((false, false), |(a, b), c| (a || c.2, b || c.3));
            let what = match (cam, mic) {
                (true, true) => "Camera and microphone in use",
                (true, false) => "Camera in use",
                _ => "Microphone in use",
            };
            u.media_button.setImage(symbol(if cam { "video.fill" } else { "mic.fill" }).as_deref());
            u.media_button.setToolTip(Some(&util::ns(&format!("{what} — click to see where or stop"))));
            set_ax_label(&u.media_button, what);
        }
    });
}

/// One meeting at a time: the Meet button is greyed out while a meeting is
/// open in any client or a Meet page is in front. Runs with the tab strip
/// (call state, tabs opened/closed) and the toolbar (tab switch, page change).
fn refresh_meet_button() {
    let locked = app::meet_locked();
    with_ui(|u| {
        if u.meet_button.isEnabled() == locked {
            u.meet_button.setEnabled(!locked);
            let tip = if locked { "A Google Meet meeting is already open — end it before starting another" } else { "Start a Google Meet meeting for this client" };
            u.meet_button.setToolTip(Some(&util::ns(tip)));
        }
    });
}

fn tab_color(t: &model::Tab) -> String {
    if t.pdf_path.is_some() {
        return "#ef4444".into();
    }
    if t.effective_workspace() == model::Workspace::GoogleSearch {
        return "#4285f4".into();
    }
    if let Some(u) = t.parsed_url() {
        if crate::policy::is_google_meet(&u) {
            return "#0f9d58".into();
        }
        if crate::policy::is_zoho_widget(&u) {
            return "#f2a516".into();
        }
    }
    model::matched_zoho_app(t.url.as_deref()).map(|a| a.color.to_string()).unwrap_or_else(|| "#7a7f8c".into())
}

thread_local! {
    /// The user chose to use Google Search before adding a client.
    static EMPTY_DISMISSED: Cell<bool> = const { Cell::new(false) };
}

/// No clients yet: the content area explains what a client is and offers
/// to add one, instead of opening on Google Search with no explanation.
fn show_empty_state() -> bool {
    let none = app::store(|s| s.client_ids_in_order().is_empty());
    if !none || EMPTY_DISMISSED.get() {
        return false;
    }
    web::set_active(None);
    with_ui(|u| {
        if u.content.subviews().iter().any(|v| v.identifier().is_some_and(|i| i.to_string() == "zcEmpty")) {
            return;
        }
        u.launcher.replace(None);
        for v in u.content.subviews().iter() {
            v.removeFromSuperview();
        }
        let v = message_screen(
            u.content.bounds(),
            "person.crop.rectangle.stack",
            "Add your first client",
            "A client is one Zoho account, signed in on its own with its own tabs. Add one for each company or account you work with; switching between them is one click.",
            &[("Add Client", sel!(addClient:)), ("Use Google Search", sel!(dismissEmpty:))],
        );
        v.setIdentifier(Some(&util::ns("zcEmpty")));
        u.content.addSubview(&v);
    });
    true
}

fn refresh_content() {
    if show_empty_state() {
        return;
    }
    let active = app::store(|s| s.active_tab().map(|(c, t)| (c.id, c.name.clone(), t.clone())));
    let Some((client, name, tab)) = active else {
        web::set_active(None);
        with_ui(|u| {
            for v in u.content.subviews().iter() {
                v.removeFromSuperview();
            }
        });
        return;
    };
    if tab.url.is_none() {
        web::set_active(None);
        show_launcher(client, &name);
        return;
    }
    web::set_active(Some(tab.id));
    let view = web::view_for(client, &tab);
    with_ui(|u| {
        u.launcher.replace(None);
        let already = unsafe { view.superview() }.is_some_and(|s| Retained::as_ptr(&s) == Retained::as_ptr(&u.content));
        if !already {
            for v in u.content.subviews().iter() {
                v.removeFromSuperview();
            }
            view.setFrame(u.content.bounds());
            view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
            u.content.addSubview(&view);
        }
        for v in u.content.subviews().iter() {
            if v.identifier().is_some_and(|i| i.to_string() == "zcLoadError") {
                v.removeFromSuperview();
            }
        }
        if let Some((kind, url)) = LOAD_ERRORS.with_borrow(|m| m.get(&tab.id).cloned()) {
            u.content.addSubview(&error_overlay(kind, url.as_deref(), u.content.bounds()));
        }
        // Keyboard focus stays where the user put it: in a text field, or on
        // the tab strip (keyboard tab switching); otherwise the page gets it.
        let keep_focus = u
            .window
            .firstResponder()
            .is_some_and(|r| r.isKindOfClass(<objc2_app_kit::NSText as objc2::ClassType>::class()) || r.isKindOfClass(<TabChip as objc2::ClassType>::class()));
        if u.address.currentEditor().is_none() && !keep_focus {
            u.window.makeFirstResponder(Some(&view));
        }
    });
}

fn show_launcher(client: Uuid, name: &str) {
    let current = with_ui(|u| u.launcher.borrow().as_ref().map(|(c, v)| (*c, v.clone()))).flatten();
    if let Some((c, v)) = &current {
        let shown = with_ui(|u| unsafe { v.superview() }.is_some_and(|s| Retained::as_ptr(&s) == Retained::as_ptr(&u.content))).unwrap_or(false);
        if *c == client && shown {
            return;
        }
    }
    let view = crate::picker::build(name);
    with_ui(|u| {
        for v in u.content.subviews().iter() {
            v.removeFromSuperview();
        }
        view.setFrame(u.content.bounds());
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        u.content.addSubview(&view);
        u.launcher.replace(Some((client, view.clone())));
    });
    // A new tab is for choosing an app: put the cursor straight in the picker.
    util::later(crate::picker::focus);
}

fn refresh_toolbar() {
    refresh_meet_button();
    let info = app::store(|s| s.active_tab().map(|(c, t)| (c.color_hex.clone(), c.symbol_name.clone(), c.initials(), c.name.clone(), c.is_search(), t.id, t.url.clone())));
    let page = info.as_ref().and_then(|i| web::page_state(i.5));
    let active_downloads = downloads::active_count();
    with_ui(|u| {
        // Profile chip, rebuilt only when the profile it shows changes.
        let key = info.as_ref().map(|i| format!("{}|{:?}|{}|{}", i.0, i.1, i.3, i.4)).unwrap_or_default();
        let rebuild = *u.chip_key.borrow() != key;
        if rebuild {
            if let Some(old) = u.chip.replace(None) {
                old.removeFromSuperview();
            }
            u.chip_key.replace(key);
        }
        if let (true, Some((hex, sym, initials, name, search, _, _))) = (rebuild, &info) {
            let l = text(name, theme::Text::Callout, theme::Weight::Semibold);
            l.sizeToFit();
            let text_w = l.frame().size.width.ceil().clamp(20.0, 150.0);
            let chip = tile(T_CHIP, 0, rect(0.0, 9.0, text_w + 38.0, 28.0));
            set_layer_color(&chip, &theme::chip_fill(), 14.0);
            // Outlined in the client's colour, next to its badge.
            set_layer_border(&chip, &util::color(if *search { theme::SEARCH_BLUE } else { hex.as_str() }), 1.0);
            let size = theme::TOOLBAR_CHIP_BADGE;
            let b = badge(hex, if *search { Some("magnifyingglass") } else { sym.as_deref() }, initials, size);
            b.setFrame(rect(6.0, (theme::ICON_BUTTON - size) / 2.0, size, size));
            chip.addSubview(&b);
            l.setFrame(rect(30.0, 5.0, text_w, 18.0));
            chip.addSubview(&l);
            chip.setToolTip(Some(&util::ns(if *search { "Independent Google Search workspace" } else { "Customise this client" })));
            u.toolbar.addSubview(&chip);
            u.chip.replace(Some(chip));
        }
        let (can_back, can_fwd, loading, url) = match (&page, &info) {
            (Some(p), _) => (p.can_back, p.can_forward, p.loading, p.url.clone()),
            (None, Some(i)) => (false, false, false, i.6.clone().unwrap_or_default()),
            _ => (false, false, false, String::new()),
        };
        show_progress(u, loading, page.as_ref().map_or(0.0, |p| p.progress));
        u.back.setEnabled(can_back);
        u.forward.setEnabled(can_fwd);
        if let Some(img) = symbol(if loading { "xmark" } else { "arrow.clockwise" }) {
            u.reload.setImage(Some(&img));
        }
        // Another tab came to the front while the address bar was being
        // edited: drop the edit, like a browser, so one tab's half-typed
        // address can never be applied to another tab.
        let tab = info.as_ref().map(|i| i.5);
        if u.address_tab.replace(tab) != tab && u.address.currentEditor().is_some() {
            u.address.abortEditing();
            if let Some(v) = web::active_view() {
                u.window.makeFirstResponder(Some(&v));
            }
        }
        if u.address.currentEditor().is_none() {
            let shown = if url.starts_with("file:") { String::new() } else { url };
            let (name, tip) = address_glyph(&shown).unwrap_or(TYPING_GLYPH);
            u.address_icon.setImage(theme::symbol(name, theme::Icon::Tab).as_deref());
            u.address_icon.setToolTip(Some(&util::ns(tip)));
            u.address.setStringValue(&util::ns(&shown));
        } else {
            // While typing, the icon says what Return will do.
            u.address_icon.setImage(theme::symbol(TYPING_GLYPH.0, theme::Icon::Tab).as_deref());
            u.address_icon.setToolTip(Some(&util::ns(TYPING_GLYPH.1)));
        }
        // The Dock icon shows how many files are downloading, like Safari.
        let dock = NSApplication::sharedApplication(mtm()).dockTile();
        dock.setBadgeLabel((active_downloads > 0).then(|| util::ns(&active_downloads.to_string())).as_deref());
        if active_downloads > 0 {
            u.downloads_button.setFont(Some(&theme::digits(theme::Text::Footnote)));
            u.downloads_button.setTitle(&util::ns(&active_downloads.to_string()));
            u.downloads_button.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
        } else {
            u.downloads_button.setTitle(&util::ns(""));
            u.downloads_button.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
        }
        u.toolbar.setNeedsLayout(true);
    });
}

/// The glyph inside the address field: a lock for an https page, a globe
/// for anything else, none for an empty field.
/// The address field's icon while typing or with no address.
const TYPING_GLYPH: (&str, &str) = ("magnifyingglass", "Search Zoho or enter address");

pub fn address_glyph(url: &str) -> Option<(&'static str, &'static str)> {
    if url.is_empty() {
        None
    } else if url.starts_with("https:") {
        Some(("lock.fill", "Secure connection (https)"))
    } else {
        Some(("globe", "Not a secure (https) address"))
    }
}

/// Grow the progress line while the page in front loads (never less than a
/// sliver, so a slow first byte still shows something), then fill it and
/// fade it out.
fn show_progress(u: &Ui, loading: bool, fraction: f64) {
    let showing = u.progress.alphaValue() > 0.0;
    if loading {
        u.progress_fading.set(false);
        u.progress_value.set(fraction.clamp(0.08, 1.0));
        u.progress.setAlphaValue(1.0);
    } else if showing && !u.progress_fading.replace(true) {
        u.progress_value.set(1.0);
        let line = u.progress.clone();
        // Seen full for a moment, then gone; reset once invisible.
        util::after(0.25, move || {
            let animator: Retained<NSView> = unsafe { msg_send![&*line, animator] };
            animator.setAlphaValue(0.0);
        });
        util::after(0.6, || {
            with_ui(|u| {
                if u.progress_fading.get() {
                    u.progress.setAlphaValue(0.0);
                    u.progress_value.set(0.0);
                    u.progress_fading.set(false);
                    u.root.setNeedsLayout(true);
                }
            });
        });
    }
    u.root.setNeedsLayout(true);
}

// ---------------------------------------------------------------------------
// Small commands.
// ---------------------------------------------------------------------------

/// Drop the downloads list open under its toolbar button.
pub fn show_downloads() {
    // A menu tracks modally; an unattended self-test run would stall on it.
    if util::is_test_build() && std::env::var_os("ZCLIENTS_SELFTEST").is_some() {
        return;
    }
    let a = actions();
    unsafe {
        let _: () = msg_send![&*a, showDownloads: None::<&AnyObject>];
    }
}

pub fn find_visible() -> bool {
    with_ui(|u| u.find_visible.get()).unwrap_or(false)
}

pub fn sidebar_visible() -> bool {
    with_ui(|u| u.sidebar_visible.get()).unwrap_or(false)
}

/// End editing the address bar. With `revert` (Esc) the tab's own address
/// comes back. The cursor goes back to the page.
fn leave_address(revert: bool) {
    with_ui(|u| {
        if revert {
            u.address.abortEditing();
        }
        match web::active_view() {
            Some(v) => u.window.makeFirstResponder(Some(&v)),
            None => u.window.makeFirstResponder(None),
        };
    });
    if revert {
        refresh(app::TOOLBAR);
    }
}

pub fn address_text() -> String {
    with_ui(|u| u.address.stringValue().to_string()).unwrap_or_default()
}

pub fn set_address(text: &str) {
    with_ui(|u| u.address.setStringValue(&util::ns(text)));
}

/// Self-tests: a real mouse click (down + up through `NSApp.sendEvent`) in
/// the middle of the address bar, or of tab chip `index`.
pub fn click_address() {
    if let Some(v) = with_ui(|u| u.address.clone()) {
        click_center(&v);
    }
}

pub fn click_tab_chip(index: usize) {
    if let Some(c) = with_ui(|u| u.chips.borrow().get(index).cloned()).flatten() {
        click_center(&c);
    }
}

/// Self-tests: the address bar's text, whether it is being edited, and the
/// selected range in it.
pub fn address_state() -> String {
    with_ui(|u| {
        let editing = u.address.currentEditor();
        let sel = editing.as_ref().map(|e| e.selectedRange());
        format!("text={:?} editing={} selection={:?}", u.address.stringValue().to_string(), editing.is_some(), sel.map(|r| (r.location, r.length)))
    })
    .unwrap_or_default()
}

/// Self-tests: replace the address bar's text as if typed (it must already
/// be being edited).
pub fn type_in_address(text: &str) {
    with_ui(|u| {
        if let Some(e) = u.address.currentEditor() {
            unsafe {
                e.selectAll(None);
                // insertText: is the path real typing takes, so the field
                // posts controlTextDidChange: as it would for keystrokes.
                let _: () = msg_send![&*e, insertText: &*util::ns(text)];
            }
        }
    });
}

/// Self-tests: select characters `from..to` of the address bar's text.
pub fn select_in_address(from: usize, to: usize) {
    with_ui(|u| {
        if let Some(e) = u.address.currentEditor() {
            e.setSelectedRange(objc2_foundation::NSRange::new(from, to - from));
        }
    });
}

fn click_center(view: &NSView) {
    let Some(window) = view.window() else { return };
    let b = view.bounds();
    let at = view.convertPoint_toView(NSPoint::new(b.size.width / 2.0, b.size.height / 2.0), None);
    let app = NSApplication::sharedApplication(mtm());
    let event = |kind| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            at,
            NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
    };
    // A text field tracks the mouse inside mouseDown until the button comes
    // up, so the mouse-up must already be waiting in the queue.
    if let (Some(down), Some(up)) = (event(objc2_app_kit::NSEventType::LeftMouseDown), event(objc2_app_kit::NSEventType::LeftMouseUp)) {
        app.postEvent_atStart(&up, false);
        app.sendEvent(&down);
    }
}

/// Self-tests and the Focus Tab Strip command: keyboard focus to the
/// active tab's chip.
pub fn focus_tab_strip() {
    with_ui(|u| {
        if let Some(c) = u.chips.borrow().iter().find(|c| c.ivars().ax_selected.get()) {
            u.window.makeFirstResponder(Some(c));
        }
    });
}

/// The red close button: hide the window (leaving full screen first, or
/// its empty Space would stay behind) and keep everything running.
fn hide_window() {
    let full = with_ui(|u| u.window.styleMask().contains(NSWindowStyleMask::FullScreen)).unwrap_or(false);
    if full {
        with_ui(|u| u.window.toggleFullScreen(None));
        util::after(0.9, hide_window);
        return;
    }
    with_ui(|u| u.window.orderOut(None));
    // The same snapshot as when the app goes to the background.
    util::later(app::app_resigned_active);
}

/// Bring the main window back (Dock icon, Window › ZClients, or a page
/// that needs the user: a dialog or the print panel).
pub fn show_window() {
    with_ui(|u| {
        if u.window.isMiniaturized() {
            u.window.deminiaturize(None);
        }
        u.window.makeKeyAndOrderFront(None);
    });
}

pub fn window_visible() -> bool {
    with_ui(|u| u.window.isVisible()).unwrap_or(false)
}

pub fn focus_view(view: &NSView) {
    with_ui(|u| u.window.makeFirstResponder(Some(view)));
}

pub fn focus_address() {
    with_ui(|u| {
        u.window.makeFirstResponder(Some(&u.address));
        if let Some(editor) = u.address.currentEditor() {
            unsafe { editor.selectAll(None) };
        }
    });
}

/// After ⌘T: the app picker when the new tab shows it, else the address bar.
pub fn focus_new_tab_soon() {
    util::after(0.08, || if crate::picker::is_showing() { crate::picker::focus() } else { focus_address() });
}

/// What the sidebar button says it will do, for its tooltip and VoiceOver.
pub fn sidebar_button_label(visible: bool) -> &'static str {
    if visible { "Hide Clients (⌃S)" } else { "Show Clients (⌃S)" }
}

fn toggle_sidebar() {
    with_ui(|u| {
        u.sidebar_visible.set(!u.sidebar_visible.get());
        let tip = sidebar_button_label(u.sidebar_visible.get());
        u.sidebar_button.setToolTip(Some(&util::ns(tip)));
        set_ax_label(&u.sidebar_button, tip.split(" (").next().unwrap_or(tip));
        u.root.setNeedsLayout(true);
        u.toolbar.setNeedsLayout(true);
    });
}

/// A code editor got ⌘F / ⌘G / ⇧⌘G and left it unhandled: the app's find.
pub fn editor_left_find_key(key: &str) {
    match key {
        "f" => show_find(true),
        "g" => run_find(false),
        "shift+g" => run_find(true),
        _ => show_find(false),
    }
}

fn show_find(visible: bool) {
    with_ui(|u| {
        u.find_visible.set(visible);
        u.root.setNeedsLayout(true);
        if visible {
            u.window.makeFirstResponder(Some(&u.find_field));
        } else {
            u.find_status.setStringValue(&util::ns(""));
            if let Some(v) = web::active_view() {
                u.window.makeFirstResponder(Some(&v));
            }
        }
    });
}

fn run_find(backwards: bool) {
    let text = with_ui(|u| u.find_field.stringValue().to_string()).unwrap_or_default();
    if text.is_empty() {
        return show_find(true);
    }
    web::find(&text, backwards, |found| {
        with_ui(|u| u.find_status.setStringValue(&util::ns(if found { "" } else { "Not found" })));
    });
}

fn customise_active() {
    if let Some(id) = app::store(|s| s.active_client().filter(|c| !c.is_search()).map(|c| c.id)) {
        show_client_sheet(Some(id));
    }
}

// ---------------------------------------------------------------------------
// Add / customise sheet.
// ---------------------------------------------------------------------------

const SYMBOLS: &[(&str, &[&str])] = &[
    ("Cleaning & Facilities", &["sparkles", "bubbles.and.sparkles.fill", "spray.and.wipe.fill", "washer.fill", "trash.fill", "drop.fill"]),
    ("Real Estate & Property", &["building.2.fill", "house.fill", "key.fill", "signpost.right.fill", "map.fill", "bed.double.fill"]),
    ("Construction & Trades", &["hammer.fill", "wrench.and.screwdriver.fill", "paintbrush.fill", "paintroller.fill", "gearshape.2.fill", "cone.fill"]),
    ("Legal & Professional", &["building.columns.fill", "scroll.fill", "signature", "checkmark.seal.fill", "doc.text.fill", "briefcase.fill"]),
    ("Finance & Accounting", &["banknote.fill", "creditcard.fill", "chart.line.uptrend.xyaxis", "chart.pie.fill", "dollarsign.circle.fill", "percent"]),
    ("Healthcare & Wellness", &["cross.case.fill", "stethoscope", "heart.fill", "pills.fill", "waveform.path.ecg", "syringe.fill"]),
    ("Food & Hospitality", &["fork.knife", "cup.and.saucer.fill", "birthday.cake.fill", "wineglass.fill", "carrot.fill", "bell.fill"]),
    ("Retail & E-commerce", &["cart.fill", "bag.fill", "tag.fill", "shippingbox.fill", "storefront.fill", "gift.fill"]),
    ("Logistics & Transport", &["truck.box.fill", "airplane", "ferry.fill", "car.fill", "location.fill", "globe.europe.africa.fill"]),
    ("Technology & Software", &["laptopcomputer", "desktopcomputer", "server.rack", "cpu.fill", "network", "cloud.fill"]),
    ("Education & Training", &["graduationcap.fill", "book.fill", "books.vertical.fill", "pencil.and.ruler.fill", "backpack.fill", "lightbulb.fill"]),
    ("Marketing & Media", &["megaphone.fill", "camera.fill", "video.fill", "mic.fill", "paintpalette.fill", "newspaper.fill"]),
    ("Manufacturing & Energy", &["gearshape.fill", "cube.fill", "bolt.fill", "flame.fill", "powerplug.fill", "sun.max.fill"]),
    ("Agriculture & Outdoors", &["leaf.fill", "tree.fill", "pawprint.fill", "mountain.2.fill", "hare.fill", "drop.fill"]),
    ("Beauty, Fitness & Events", &["scissors", "figure.run", "dumbbell.fill", "suitcase.fill", "ticket.fill", "party.popper.fill"]),
    ("Security & Community", &["shield.lefthalf.filled", "lock.fill", "person.3.fill", "handshake.fill", "hands.clap.fill", "heart.text.square.fill"]),
    ("General", &["star.fill", "circle.hexagongrid.fill", "flag.fill", "bookmark.fill", "target", "diamond.fill"]),
];

struct Sheet {
    window: Retained<NSWindow>,
    client: Option<Uuid>,
    name: Retained<NSTextField>,
    well: Retained<NSColorWell>,
    symbol: Retained<NSPopUpButton>,
    symbol_names: Vec<Option<String>>,
    email: Retained<NSTextField>,
    password: Retained<NSSecureTextField>,
    login_status: Retained<NSTextField>,
    change_button: Retained<NSButton>,
    remove_button: Retained<NSButton>,
    keep_signed_in: Retained<NSButton>,
    retention: Retained<NSPopUpButton>,
    /// What Save does with the saved login.
    login_mode: Cell<LoginMode>,
    /// Inline, red explanations of why Save did not close the sheet.
    name_error: Retained<NSTextField>,
    login_error: Retained<NSTextField>,
}

/// Why the client sheet cannot be saved as it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetProblem {
    NoName,
    NoPassword,
    NoEmail,
    BadEmail,
}

impl SheetProblem {
    fn message(self) -> &'static str {
        match self {
            SheetProblem::NoName => "Enter a client name.",
            SheetProblem::NoPassword => "Enter the password too, or clear the email to save no login.",
            SheetProblem::NoEmail => "Enter the email too, or clear the password to save no login.",
            SheetProblem::BadEmail => "This doesn't look like an email address.",
        }
    }
}

/// A plain shape check: something@domain.tld, no spaces.
pub fn looks_like_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    !local.is_empty() && !domain.contains('@') && !e.chars().any(char::is_whitespace) && domain.split('.').count() >= 2 && domain.split('.').all(|part| !part.is_empty())
}

/// What stops Save: an empty name, or a login with only one of its two
/// halves (which used to be dropped silently while the sheet closed as if
/// saved), or an email that cannot be one. `entering_login` is whether the
/// email/password fields are in use.
pub fn sheet_problem(name: &str, entering_login: bool, email: &str, password: &str) -> Option<SheetProblem> {
    if name.trim().is_empty() {
        return Some(SheetProblem::NoName);
    }
    if !entering_login {
        return None;
    }
    let email = email.trim();
    match (email.is_empty(), password.is_empty()) {
        (true, true) => None,
        (false, true) => Some(SheetProblem::NoPassword),
        (true, false) => Some(SheetProblem::NoEmail),
        (false, false) if !looks_like_email(email) => Some(SheetProblem::BadEmail),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoginMode {
    /// Leave it as it is (the default for an existing profile).
    Keep,
    /// The fields are showing: save what is typed there (if anything).
    Replace,
    /// Remove the saved login on Save.
    Remove,
}

fn pretty_symbol(name: &str) -> String {
    let base = name.split('.').next().unwrap_or(name);
    let mut c = base.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub fn show_client_sheet(client: Option<Uuid>) {
    if SHEET.with_borrow(|s| s.is_some()) {
        return;
    }
    let mtm = mtm();
    let existing = client.and_then(|id| app::store(|s| s.client(id).filter(|c| !c.is_search()).cloned()));
    if client.is_some() && existing.is_none() {
        return;
    }
    // Only whether a login is saved, and for which address. The password
    // itself is never put into a field.
    let saved_email = client.and_then(keychain::saved_login_email);
    let (w, h) = (480.0, 500.0);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect(0.0, 0.0, w, h), NSWindowStyleMask::Titled, NSBackingStoreType::Buffered, false)
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let body = flex(K_PLAIN);
    body.setFrame(rect(0.0, 0.0, w, h));
    window.setContentView(Some(&body));

    let heading = text(if existing.is_some() { "Customise Client" } else { "Add Client" }, theme::Text::Title3, theme::Weight::Bold);
    heading.setFrame(rect(20.0, 18.0, w - 40.0, 22.0));
    body.addSubview(&heading);

    let fx = theme::SHEET_MARGIN + theme::LABEL_COL_W + theme::GRID * 2.0;
    let caption = |text: &str, y: f64| {
        let l = self::text(text, theme::Text::Body, theme::Weight::Regular);
        l.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        // Labels right-aligned in one column; every field starts on one edge.
        l.setFrame(rect(theme::SHEET_MARGIN, y + 3.0, theme::LABEL_COL_W, 18.0));
        body.addSubview(&l);
    };

    caption("Name", 56.0);
    let name = NSTextField::textFieldWithString(&util::ns(existing.as_ref().map(|c| c.name.as_str()).unwrap_or("")), mtm);
    name.setPlaceholderString(Some(&util::ns("Client name (e.g. Acme Corp)")));
    name.setFrame(rect(fx, 56.0, w - fx - theme::SHEET_MARGIN, 24.0));
    body.addSubview(&name);

    let name_error = error_label(rect(fx, 81.0, w - fx - theme::SHEET_MARGIN, 14.0));
    body.addSubview(&name_error);

    caption("Colour", 98.0);
    let well = NSColorWell::initWithFrame(NSColorWell::alloc(mtm), rect(fx, 98.0, 44.0, 44.0));
    let start_hex = existing.as_ref().map(|c| c.color_hex.clone()).unwrap_or_else(|| model::PROFILE_COLORS[0].1.to_string());
    well.setColor(&util::color(&start_hex));
    body.addSubview(&well);
    for (i, (cname, hex)) in model::PROFILE_COLORS.iter().enumerate() {
        let (r, c) = (i / 8, i % 8);
        let t = tile(T_SWATCH, i, rect(fx + 56.0 + c as f64 * 26.0, 98.0 + r as f64 * 24.0, 20.0, 20.0));
        set_layer_color(&t, &util::color(hex), 10.0);
        t.setToolTip(Some(&util::ns(cname)));
        body.addSubview(&t);
    }

    caption("Symbol", 150.0);
    let symbol_popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(fx, 148.0, w - fx - theme::SHEET_MARGIN, 26.0), false);
    symbol_popup.setAutoenablesItems(false);
    let mut names: Vec<Option<String>> = vec![None];
    symbol_popup.addItemWithTitle(&util::ns("Initials (no symbol)"));
    let current_symbol = existing.as_ref().and_then(|c| c.symbol_name.clone());
    let mut selected = 0;
    if let Some(menu) = symbol_popup.menu() {
        for (group, syms) in SYMBOLS {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            names.push(None);
            let header = unsafe { menu.addItemWithTitle_action_keyEquivalent(&util::ns(group), None, &util::ns("")) };
            header.setEnabled(false);
            names.push(None);
            for s in *syms {
                let Some(img) = symbol(s) else { continue };
                let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&util::ns(&format!("   {}", pretty_symbol(s))), None, &util::ns("")) };
                item.setImage(Some(&img));
                if current_symbol.as_deref() == Some(*s) && selected == 0 {
                    selected = names.len();
                }
                names.push(Some(s.to_string()));
            }
        }
    }
    symbol_popup.selectItemAtIndex(selected as isize);
    body.addSubview(&symbol_popup);

    let section = text("Sign-in", theme::Text::Headline, theme::Weight::Semibold);
    section.setFrame(rect(20.0, 190.0, w - 40.0, 18.0));
    body.addSubview(&section);

    let retention = existing.as_ref().map(|c| c.retention()).unwrap_or_default();
    let keep = unsafe { NSButton::checkboxWithTitle_target_action(&util::ns("Keep me signed in"), Some(&target()), Some(sel!(sheetKeepToggled:)), mtm) };
    keep.setState(if retention.keep { NSControlStateValueOn } else { NSControlStateValueOff });
    keep.setFrame(rect(fx, 214.0, 160.0, 22.0));
    keep.setToolTip(Some(&util::ns("Stay signed in to Zoho across quits. Signing out, or Zoho ending the session, always signs you out here too.")));
    body.addSubview(&keep);
    let retention_popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(fx + 164.0, 212.0, w - fx - 164.0 - theme::SHEET_MARGIN, 26.0), false);
    for days in crate::session::RETENTION_CHOICES {
        let title = match days {
            7 => "for up to 1 week unused".to_string(),
            365 => "for up to 1 year unused".to_string(),
            d => format!("for up to {d} days unused"),
        };
        retention_popup.addItemWithTitle(&util::ns(&title));
    }
    let at = crate::session::RETENTION_CHOICES.iter().position(|d| *d == retention.days).unwrap_or(2);
    retention_popup.selectItemAtIndex(at as isize);
    retention_popup.setEnabled(retention.keep);
    retention_popup.setAccessibilityLabel(Some(&util::ns("How long to stay signed in without using this client")));
    body.addSubview(&retention_popup);

    caption("Saved login", 248.0);
    let status_text = match (&existing, &saved_email) {
        (None, _) => "Optional — fill in below, or sign in once and ZClients offers to save it.".to_string(),
        (Some(_), Some(e)) => format!("Saved for {e}"),
        (Some(_), None) => "Not saved".to_string(),
    };
    let login_status = text(&status_text, theme::Text::Callout, theme::Weight::Regular);
    login_status.setFrame(rect(fx, 251.0, w - fx - 186.0, 18.0));
    body.addSubview(&login_status);
    let change_button = unsafe {
        NSButton::buttonWithTitle_target_action(&util::ns(if saved_email.is_some() { "Change…" } else { "Add…" }), Some(&target()), Some(sel!(sheetChangeLogin:)), mtm)
    };
    change_button.setFrame(rect(w - 180.0, 245.0, 84.0, 28.0));
    change_button.setHidden(existing.is_none());
    body.addSubview(&change_button);
    let remove_button = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Remove"), Some(&target()), Some(sel!(sheetRemoveLogin:)), mtm) };
    remove_button.setFrame(rect(w - 94.0, 245.0, 78.0, 28.0));
    remove_button.setHasDestructiveAction(true);
    remove_button.setContentTintColor(Some(&theme::destructive()));
    remove_button.setHidden(existing.is_none());
    remove_button.setEnabled(saved_email.is_some());
    remove_button.setAccessibilityLabel(Some(&util::ns("Remove saved login")));
    body.addSubview(&remove_button);

    caption("Email", 284.0);
    let email = NSTextField::textFieldWithString(&util::ns(""), mtm);
    email.setPlaceholderString(Some(&util::ns("you@company.com")));
    email.setFrame(rect(fx, 284.0, w - fx - theme::SHEET_MARGIN, 24.0));
    body.addSubview(&email);

    caption("Password", 318.0);
    let password = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(fx, 318.0, w - fx - theme::SHEET_MARGIN, 24.0));
    password.setPlaceholderString(Some(&util::ns(if saved_email.is_some() { "New password" } else { "Password" })));
    body.addSubview(&password);

    let login_error = error_label(rect(fx, 344.0, w - fx - theme::SHEET_MARGIN, 14.0));
    body.addSubview(&login_error);

    let note = NSTextField::wrappingLabelWithString(
        &util::ns(
            "A saved login is stored encrypted in ZClients's vault, for this client only; the vault's key is protected by your Mac's Keychain. If Zoho asks this client to sign in again, ZClients fills it in on Zoho's own sign-in page and you press Sign In.",
        ),
        mtm,
    );
    note.setFont(Some(&theme::font(theme::Text::Footnote)));
    note.setTextColor(Some(&NSColor::secondaryLabelColor()));
    note.setFrame(rect(fx, 364.0, w - fx - theme::SHEET_MARGIN, 72.0));
    body.addSubview(&note);

    if existing.is_some() {
        // Deleting is reachable where the client is edited, not only from
        // the sidebar's context menu. Red, on the far left, away from Save.
        let delete = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Delete Client…"), Some(&target()), Some(sel!(sheetDelete:)), mtm) };
        delete.setHasDestructiveAction(true);
        delete.setContentTintColor(Some(&theme::destructive()));
        delete.sizeToFit();
        let dw = delete.frame().size.width.max(theme::BUTTON_W + 22.0);
        delete.setFrame(rect(16.0, h - 46.0, dw, 28.0));
        body.addSubview(&delete);
    }
    let cancel = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Cancel"), Some(&target()), Some(sel!(sheetCancel:)), mtm) };
    cancel.setKeyEquivalent(&util::ns("\u{1b}"));
    cancel.setFrame(rect(w - 196.0, h - 46.0, 88.0, 28.0));
    body.addSubview(&cancel);
    let save = unsafe { NSButton::buttonWithTitle_target_action(&util::ns("Save"), Some(&target()), Some(sel!(sheetSave:)), mtm) };
    save.setKeyEquivalent(&util::ns("\r"));
    save.setFrame(rect(w - 104.0, h - 46.0, 88.0, 28.0));
    body.addSubview(&save);

    let mode = if existing.is_some() { LoginMode::Keep } else { LoginMode::Replace };
    window.setInitialFirstResponder(Some(&name));
    let sheet = Sheet {
        window: window.clone(),
        client,
        name,
        well,
        symbol: symbol_popup,
        symbol_names: names,
        email,
        password,
        login_status,
        change_button,
        remove_button,
        keep_signed_in: keep,
        retention: retention_popup,
        login_mode: Cell::new(mode),
        name_error,
        login_error,
    };
    SHEET.with_borrow_mut(|s| *s = Some(sheet));
    sheet_set_login_mode(mode);
    with_ui(|u| unsafe { u.window.beginSheet_completionHandler(&window, None) });
}

/// Show the email/password fields only while the user is entering a new
/// login; say plainly what Save will do.
fn sheet_set_login_mode(mode: LoginMode) {
    SHEET.with_borrow(|s| {
        let Some(s) = s else { return };
        s.login_mode.set(mode);
        let fields = mode == LoginMode::Replace;
        for f in [&*s.email as &NSTextField, &*s.password] {
            f.setEnabled(fields);
            f.setStringValue(&util::ns(""));
        }
        if s.client.is_some() {
            let saved = s.client.and_then(keychain::saved_login_email);
            let text = match (mode, &saved) {
                (LoginMode::Keep, Some(e)) => format!("Saved for {e}"),
                (LoginMode::Keep, None) => "Not saved".into(),
                (LoginMode::Replace, Some(_)) => "Enter the new login; Save replaces the saved one".into(),
                (LoginMode::Replace, None) => "Enter a login to save".into(),
                (LoginMode::Remove, _) => "Will be removed when you click Save".into(),
            };
            s.login_status.setStringValue(&util::ns(&text));
            s.remove_button.setEnabled(saved.is_some() && mode != LoginMode::Remove);
            s.change_button.setEnabled(mode != LoginMode::Replace);
        }
        if fields {
            s.window.makeFirstResponder(Some(&s.email));
        }
    });
}

fn sheet_pick_swatch(index: usize) {
    if let Some((_, hex)) = model::PROFILE_COLORS.get(index) {
        SHEET.with_borrow(|s| {
            if let Some(s) = s {
                s.well.setColor(&util::color(hex));
            }
        });
    }
}

/// Self-tests: press Save (true) or Cancel on the open client sheet.
pub fn finish_sheet(save: bool) {
    sheet_finish(save);
}

/// Self-tests: the client sheet's name field and its error line.
pub fn sheet_state() -> Option<(String, String)> {
    SHEET.with_borrow(|s| s.as_ref().map(|s| (s.name.stringValue().to_string(), s.name_error.stringValue().to_string())))
}

/// Have VoiceOver say `text` now, at high priority (it interrupts what it
/// was reading).
pub fn announce(text: &str) {
    use objc2_app_kit::{
        NSAccessibilityAnnouncementKey, NSAccessibilityAnnouncementRequestedNotification, NSAccessibilityPostNotificationWithUserInfo, NSAccessibilityPriorityKey,
        NSAccessibilityPriorityLevel,
    };
    let message = util::ns(text);
    let priority = objc2_foundation::NSNumber::new_isize(NSAccessibilityPriorityLevel::High.0);
    let keys = unsafe { [NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey] };
    let values: [&AnyObject; 2] = [&message, &priority];
    let info = objc2_foundation::NSDictionary::from_slices(&keys, &values);
    let app = NSApplication::sharedApplication(mtm());
    unsafe { NSAccessibilityPostNotificationWithUserInfo(&app, NSAccessibilityAnnouncementRequestedNotification, Some(&info)) };
}

/// A small red line under a field, empty until there is something to say.
fn error_label(frame: NSRect) -> Retained<NSTextField> {
    let l = text("", theme::Text::Footnote, theme::Weight::Regular);
    l.setTextColor(Some(&theme::destructive()));
    l.setFrame(frame);
    l
}

fn sheet_finish(save: bool) {
    let Some(sheet) = SHEET.with_borrow_mut(|s| s.take()) else { return };
    if save {
        let name = sheet.name.stringValue().to_string();
        let entering = sheet.login_mode.get() == LoginMode::Replace;
        let (email, password) = (sheet.email.stringValue().to_string(), sheet.password.stringValue().to_string());
        sheet.name_error.setStringValue(&util::ns(""));
        sheet.login_error.setStringValue(&util::ns(""));
        if let Some(problem) = sheet_problem(&name, entering, &email, &password) {
            // Stay open, say what is wrong right under it, and put the
            // cursor where the fix goes.
            let (line, field): (&NSTextField, &NSTextField) = match problem {
                SheetProblem::NoName => (&sheet.name_error, &sheet.name),
                SheetProblem::NoPassword => (&sheet.login_error, &sheet.password),
                SheetProblem::NoEmail | SheetProblem::BadEmail => (&sheet.login_error, &sheet.email),
            };
            line.setStringValue(&util::ns(problem.message()));
            line.setAccessibilityLabel(Some(&util::ns(problem.message())));
            sheet.window.makeFirstResponder(Some(field));
            // VoiceOver reads the field that got focus, not the red line.
            announce(problem.message());
            SHEET.with_borrow_mut(|s| *s = Some(sheet));
            return;
        }
        let color = util::color_hex(&sheet.well.color()).unwrap_or_else(|| "#e42527".into());
        let symbol = sheet.symbol_names.get(sheet.symbol.indexOfSelectedItem().max(0) as usize).cloned().flatten();
        let login = match sheet.login_mode.get() {
            LoginMode::Keep => app::LoginChange::Keep,
            LoginMode::Remove => app::LoginChange::Remove,
            LoginMode::Replace => {
                let l = keychain::Login { email: sheet.email.stringValue().to_string().trim().to_string(), password: sheet.password.stringValue().to_string() };
                if l.is_empty() { app::LoginChange::Keep } else { app::LoginChange::Replace(l) }
            }
        };
        sheet.password.setStringValue(&util::ns(""));
        let days = crate::session::RETENTION_CHOICES.get(sheet.retention.indexOfSelectedItem().max(0) as usize).copied().unwrap_or(crate::session::DEFAULT_RETENTION_DAYS);
        let retention = crate::session::Retention { keep: sheet.keep_signed_in.state() == NSControlStateValueOn, days };
        match sheet.client {
            Some(id) => app::update_client(id, &name, &color, symbol, login, retention),
            None => app::add_client(&name, &color, symbol, login, retention),
        }
    } else {
        sheet.password.setStringValue(&util::ns(""));
    }
    with_ui(|u| u.window.endSheet(&sheet.window));
    sheet.window.orderOut(None);
    // The shared colour panel the colour well opened would otherwise float
    // on, editing nothing.
    if objc2_app_kit::NSColorPanel::sharedColorPanelExists(mtm()) {
        objc2_app_kit::NSColorPanel::sharedColorPanel(mtm()).close();
    }
    sheet.well.deactivate();
}

// ---------------------------------------------------------------------------
// Main menu.
// ---------------------------------------------------------------------------

fn submenu(bar: &NSMenu, title: &str) -> Retained<NSMenu> {
    let mtm = mtm();
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &util::ns(title));
    let item = NSMenuItem::new(mtm);
    item.setTitle(&util::ns(title));
    item.setSubmenu(Some(&menu));
    bar.addItem(&item);
    menu
}

fn key(menu: &NSMenu, title: &str, action: Sel, k: &str, mods: NSEventModifierFlags) -> Retained<NSMenuItem> {
    let item = add_item(menu, title, action, k, 0);
    item.setKeyEquivalentModifierMask(mods);
    item
}

/// A second shortcut for an action already in the menu: hidden, but its
/// key equivalent stays live.
fn alt_key(menu: &NSMenu, title: &str, action: Sel, k: &str, mods: NSEventModifierFlags) {
    let item = key(menu, title, action, k, mods);
    item.setHidden(true);
    item.setAllowsKeyEquivalentWhenHidden(true);
}

/// Standard responder-chain item (copy/paste etc.), no explicit target.
fn std_item(menu: &NSMenu, title: &str, action: Sel, k: &str, mods: NSEventModifierFlags) {
    let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&util::ns(title), Some(action), &util::ns(k)) };
    item.setKeyEquivalentModifierMask(mods);
}

fn build_menu() {
    let mtm = mtm();
    let app = NSApplication::sharedApplication(mtm);
    let bar = NSMenu::new(mtm);
    let cmd = NSEventModifierFlags::Command;
    let cmd_shift = NSEventModifierFlags::Command | NSEventModifierFlags::Shift;
    let ctrl = NSEventModifierFlags::Control;

    let app_menu = submenu(&bar, "ZClients");
    add_item(&app_menu, "About ZClients", sel!(about:), "", 0);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    key(&app_menu, "Settings…", sel!(settings:), ",", cmd);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    std_item(&app_menu, "Hide ZClients", sel!(hide:), "h", cmd);
    std_item(&app_menu, "Hide Others", sel!(hideOtherApplications:), "h", cmd | NSEventModifierFlags::Option);
    std_item(&app_menu, "Show All", sel!(unhideAllApplications:), "", cmd);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    std_item(&app_menu, "Quit ZClients", sel!(terminate:), "q", cmd);

    let file = submenu(&bar, "File");
    key(&file, "New Tab", sel!(newTab:), "t", cmd);
    key(&file, "New Client…", sel!(addClient:), "n", cmd_shift);
    key(&file, "Duplicate Tab", sel!(duplicateTab:), "d", cmd);
    key(&file, "Reopen Closed Tab", sel!(reopenTab:), "t", cmd_shift);
    file.addItem(&NSMenuItem::separatorItem(mtm));
    key(&file, "Customise Client…", sel!(customise:), ",", cmd_shift);
    key(&file, "Download Current Page", sel!(downloadPage:), "s", cmd_shift);
    // Always available, even while a page owns ⇧⌘S.
    alt_key(&file, "Download Current Page ", sel!(downloadPage:), "s", cmd | NSEventModifierFlags::Option);
    key(&file, "Print…", sel!(printPage:), "p", cmd);
    file.addItem(&NSMenuItem::separatorItem(mtm));
    key(&file, "Close Tab", sel!(closeTab:), "w", cmd);

    let edit = submenu(&bar, "Edit");
    std_item(&edit, "Undo", sel!(undo:), "z", cmd);
    std_item(&edit, "Redo", sel!(redo:), "z", cmd_shift);
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    std_item(&edit, "Cut", sel!(cut:), "x", cmd);
    std_item(&edit, "Copy", sel!(copy:), "c", cmd);
    std_item(&edit, "Paste", sel!(paste:), "v", cmd);
    std_item(&edit, "Paste and Match Style", sel!(pasteAsPlainText:), "v", cmd_shift);
    let alt = unsafe { edit.addItemWithTitle_action_keyEquivalent(&util::ns("Paste and Match Style "), Some(sel!(pasteAsPlainText:)), &util::ns("v")) };
    alt.setKeyEquivalentModifierMask(cmd_shift | NSEventModifierFlags::Option);
    alt.setHidden(true);
    alt.setAllowsKeyEquivalentWhenHidden(true);
    std_item(&edit, "Delete", sel!(delete:), "", NSEventModifierFlags::empty());
    std_item(&edit, "Select All", sel!(selectAll:), "a", cmd);
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    key(&edit, "Find…", sel!(find:), "f", cmd);
    key(&edit, "Find Next", sel!(findNext:), "g", cmd);
    key(&edit, "Find Previous", sel!(findPrev:), "g", cmd_shift);

    let view = submenu(&bar, "View");
    // Not `toggleSidebar:` — AppKit reserves that name and re-routes it.
    // Control-S, as the user asked; it always works, even with a page
    // focused (Control shortcuts reach the menus first). ⌃⌘S (the standard
    // Mac one) and ⌘S (outside pages) keep working too.
    key(&view, "Toggle Sidebar", sel!(zcToggleSidebar:), "s", ctrl);
    alt_key(&view, "Toggle Sidebar ", sel!(zcToggleSidebar:), "s", cmd | ctrl);
    alt_key(&view, "Toggle Sidebar  ", sel!(zcToggleSidebar:), "s", cmd);
    view.addItem(&NSMenuItem::separatorItem(mtm));
    key(&view, "Zoom In", sel!(zoomIn:), "=", cmd);
    alt_key(&view, "Zoom In ", sel!(zoomIn:), "+", cmd);
    key(&view, "Zoom Out", sel!(zoomOut:), "-", cmd);
    key(&view, "Actual Size", sel!(zoomReset:), "0", cmd);
    view.addItem(&NSMenuItem::separatorItem(mtm));
    key(&view, "Downloads", sel!(showDownloads:), "j", cmd_shift);
    std_item(&view, "Enter Full Screen", sel!(toggleFullScreen:), "f", cmd | ctrl);

    let go = submenu(&bar, "Go");
    key(&go, "Back", sel!(back:), "[", cmd);
    key(&go, "Forward", sel!(forward:), "]", cmd);
    key(&go, "Reload", sel!(reload:), "r", cmd);
    key(&go, "Force Reload", sel!(forceReload:), "r", cmd_shift);
    go.addItem(&NSMenuItem::separatorItem(mtm));
    key(&go, "App Launcher (Home)", sel!(home:), "h", cmd_shift);
    key(&go, "Open Location", sel!(openLocation:), "l", cmd);
    key(&go, "Google Search", sel!(search:), "k", cmd);
    alt_key(&go, "Google Search ", sel!(search:), "k", cmd | NSEventModifierFlags::Option);
    key(&go, "New Google Meet Meeting", sel!(meet:), "m", cmd_shift);
    // Switching clients is the app's main job: all of it from the keyboard.
    go.addItem(&NSMenuItem::separatorItem(mtm));
    key(&go, "Switch to Client…", sel!(switchClient:), "o", cmd_shift);
    key(&go, "Next Client", sel!(nextClient:), "\u{F701}", cmd | ctrl);
    key(&go, "Previous Client", sel!(prevClient:), "\u{F700}", cmd | ctrl);
    let clients = NSMenuItem::new(mtm);
    clients.setTitle(&util::ns("Client"));
    let numbered = NSMenu::initWithTitle(NSMenu::alloc(mtm), &util::ns("Client"));
    for n in 1..=9 {
        let item = key(&numbered, &format!("Client {n}"), sel!(selectClientN:), &n.to_string(), cmd | NSEventModifierFlags::Option);
        item.setTag((n - 1) as isize);
    }
    clients.setSubmenu(Some(&numbered));
    go.addItem(&clients);

    let tabs = submenu(&bar, "Tabs");
    key(&tabs, "Next Tab", sel!(nextTab:), "\t", ctrl);
    key(&tabs, "Previous Tab", sel!(prevTab:), "\t", ctrl | NSEventModifierFlags::Shift);
    alt_key(&tabs, "Next Tab ", sel!(nextTab:), "]", cmd_shift);
    alt_key(&tabs, "Previous Tab ", sel!(prevTab:), "[", cmd_shift);
    // ⌘⌥→ / ⌘⌥← as in Chrome.
    alt_key(&tabs, "Next Tab  ", sel!(nextTab:), "\u{F703}", cmd | NSEventModifierFlags::Option);
    alt_key(&tabs, "Previous Tab  ", sel!(prevTab:), "\u{F702}", cmd | NSEventModifierFlags::Option);
    tabs.addItem(&NSMenuItem::separatorItem(mtm));
    for n in 1..=8 {
        let item = key(&tabs, &format!("Tab {n}"), sel!(selectTabN:), &n.to_string(), cmd);
        item.setTag((n - 1) as isize);
    }
    key(&tabs, "Last Tab", sel!(lastTab:), "9", cmd);
    tabs.addItem(&NSMenuItem::separatorItem(mtm));
    key(&tabs, "Search Tabs…", sel!(tabOverflow:), "a", cmd_shift);
    key(&tabs, "Move Tab Left", sel!(moveTabLeft:), "\u{F702}", cmd | ctrl | NSEventModifierFlags::Option);
    key(&tabs, "Move Tab Right", sel!(moveTabRight:), "\u{F703}", cmd | ctrl | NSEventModifierFlags::Option);
    key(&tabs, "Focus Tab Strip", sel!(focusTabs:), "t", cmd | ctrl);

    let window = submenu(&bar, "Window");
    std_item(&window, "Minimize", sel!(performMiniaturize:), "m", cmd);
    std_item(&window, "Zoom", sel!(performZoom:), "", cmd);
    window.addItem(&NSMenuItem::separatorItem(mtm));
    // A hidden window is not listed by AppKit; this always brings it back.
    add_item(&window, "ZClients", sel!(showMainWindow:), "", 0);
    app.setWindowsMenu(Some(&window));

    let help = submenu(&bar, "Help");
    add_item(&help, "Export Diagnostics…", sel!(exportDiagnostics:), "", 0);
    app.setHelpMenu(Some(&help));

    app.setMainMenu(Some(&bar));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::LoadError as E;

    #[test]
    fn the_sidebar_button_says_what_it_will_do() {
        assert_eq!(sidebar_button_label(true), "Hide Clients (⌃S)");
        assert_eq!(sidebar_button_label(false), "Show Clients (⌃S)");
    }

    #[test]
    fn the_address_bar_says_whether_the_page_is_https() {
        assert_eq!(address_glyph("https://crm.zoho.com/").map(|g| g.0), Some("lock.fill"));
        assert_eq!(address_glyph("http://example.com/").map(|g| g.0), Some("globe"));
        assert_eq!(address_glyph(""), None);
    }

    #[test]
    fn menu_commands_are_enabled_only_when_they_can_act() {
        let launcher = MenuContext { page: false, real_client: true, tabs: 1, clients: 2, real_clients: 1, ..Default::default() };
        for a in ["back:", "forward:", "reload:", "printPage:", "find:", "zoomIn:", "downloadPage:", "moveTabLeft:", "nextClient:"] {
            assert!(!menu_enabled(a, 0, &MenuContext::default()), "{a} with nothing open");
        }
        assert!(!menu_enabled("printPage:", 0, &launcher), "the launcher has nothing to print");
        assert!(menu_enabled("customise:", 0, &launcher));
        assert!(menu_enabled("nextClient:", 0, &launcher));
        assert!(menu_enabled("selectClientN:", 0, &launcher));
        assert!(!menu_enabled("selectClientN:", 1, &launcher), "only one real client");
        let search = MenuContext { page: true, can_back: true, real_client: false, tabs: 3, clients: 2, real_clients: 1, ..Default::default() };
        assert!(!menu_enabled("customise:", 0, &search), "Search has nothing to customise");
        assert!(menu_enabled("back:", 0, &search) && !menu_enabled("forward:", 0, &search));
        assert!(menu_enabled("selectTabN:", 2, &search) && !menu_enabled("selectTabN:", 3, &search));
        assert!(menu_enabled("newTab:", 0, &MenuContext::default()), "unlisted commands stay on");
        assert!(menu_enabled("meet:", 0, &MenuContext::default()));
        assert!(!menu_enabled("meet:", 0, &MenuContext { meet_locked: true, ..Default::default() }), "one meeting at a time");
    }

    #[test]
    fn half_a_login_is_never_silently_dropped() {
        assert_eq!(sheet_problem("  ", false, "", ""), Some(SheetProblem::NoName));
        assert_eq!(sheet_problem("Acme", true, "jo@acme.com", ""), Some(SheetProblem::NoPassword));
        assert_eq!(sheet_problem("Acme", true, "", "secret"), Some(SheetProblem::NoEmail));
        assert_eq!(sheet_problem("Acme", true, "jo at acme", "secret"), Some(SheetProblem::BadEmail));
        assert_eq!(sheet_problem("Acme", true, "", ""), None, "no login at all is fine");
        assert_eq!(sheet_problem("Acme", true, " jo@acme.co.uk ", "secret"), None);
        assert_eq!(sheet_problem("Acme", false, "jo@", ""), None, "fields not in use");
        for bad in ["jo@acme", "@acme.com", "jo@@acme.com", "jo@acme..com", "j o@acme.com", "jo@.com"] {
            assert!(!looks_like_email(bad), "{bad}");
        }
        assert!(looks_like_email("first.last+zoho@sub.example.com"));
    }

    #[test]
    fn every_error_screen_offers_a_way_forward() {
        let pdf = Some("https://crm.zoho.com/crm/org1/attachment/1.pdf");
        assert_eq!(error_actions(E::PdfMissing, pdf), (ErrorAction::Redownload, Some(ErrorAction::CloseTab)));
        assert_eq!(error_actions(E::PdfMissing, None), (ErrorAction::CloseTab, None), "nothing to fetch again");
        assert_eq!(error_actions(E::NotAllowed, Some("https://example.com/")), (ErrorAction::OpenInBrowser, Some(ErrorAction::CloseTab)));
        assert_eq!(error_actions(E::NotAllowed, Some("file:///etc/passwd")), (ErrorAction::CloseTab, None), "never hands a file: address to the browser");
        assert_eq!(error_actions(E::Crashed, Some("https://crm.zoho.com/")), (ErrorAction::TryAgain, Some(ErrorAction::OpenInBrowser)));
        assert_eq!(error_actions(E::Offline, Some("https://crm.zoho.com/")), (ErrorAction::TryAgain, None));
        assert_eq!(error_actions(E::BlockedInSearch, Some("https://example.com/")), (ErrorAction::Dismiss, None), "never offers the browser");
        for kind in [E::Crashed, E::PdfMissing, E::NotAllowed, E::BlockedInSearch, E::Offline, E::Certificate, E::Authentication, E::Unsupported, E::Other] {
            let (title, body) = load_error_text(kind, Some("https://crm.zoho.com/x"));
            assert!(!title.is_empty() && !body.is_empty(), "{kind:?}");
        }
    }
}
