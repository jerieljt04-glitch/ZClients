//! The hub: owns the profile store, persists it, and turns user commands into
//! model changes, web view work and UI refreshes.
//!
//! Re-entrancy rule: the store is only borrowed for pure data work. UI
//! refreshes and saves are always deferred to the next run-loop turn, so no
//! AppKit or WebKit callback can ever run while the store is borrowed.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::Mutex;

use url::Url;
use uuid::Uuid;

use crate::model::{Effect, SortMode, Store, Workspace, GOOGLE_SEARCH_ID, ZOHO_APPS};
use crate::{keychain, policy, suggest, tabstate, ui, util, web};

pub const SIDEBAR: u8 = 1;
pub const TABS: u8 = 2;
pub const CONTENT: u8 = 4;
pub const TOOLBAR: u8 = 8;
pub const ALL: u8 = SIDEBAR | TABS | CONTENT | TOOLBAR;

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
    static PENDING: Cell<u8> = const { Cell::new(0) };
    static SAVE_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static META_PENDING: RefCell<HashSet<Uuid>> = RefCell::new(HashSet::new());
    static META_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static DECLINED_LOGIN: RefCell<HashSet<Uuid>> = RefCell::new(HashSet::new());
}

/// Highest save generation written so far. Saves run on a background thread;
/// the generation check stops an older snapshot from landing after a newer
/// one.
static WRITTEN: Mutex<u64> = Mutex::new(0);
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn save_path() -> std::path::PathBuf {
    util::support_dir().join("clients.json")
}

pub fn load() {
    let path = save_path();
    let store = match std::fs::read(&path) {
        Ok(data) => match Store::from_json(&data) {
            Ok(s) => s,
            Err(e) => {
                // Never overwrite a file we could not read: keep it aside.
                let aside = path.with_extension(format!("unreadable-{}.json", std::process::id()));
                let _ = std::fs::copy(&path, &aside);
                util::log(format!("could not read {}: {e}; kept a copy at {}", path.display(), aside.display()));
                let mut s = Store::default();
                s.repair();
                s
            }
        },
        Err(_) => {
            let mut s = Store::default();
            s.repair();
            s
        }
    };
    let keep: Vec<Uuid> = store.clients.iter().flat_map(|c| c.tabs.iter().map(|t| t.id)).collect();
    tabstate::prune(&keep);
    STORE.with_borrow_mut(|s| *s = store);
}

pub fn store<R>(f: impl FnOnce(&Store) -> R) -> R {
    STORE.with_borrow(f)
}

/// Change the store, then refresh `what` and save.
pub fn mutate<R>(what: u8, f: impl FnOnce(&mut Store) -> R) -> R {
    let r = STORE.with_borrow_mut(f);
    changed(what);
    schedule_save();
    r
}

pub fn changed(what: u8) {
    let before = PENDING.get();
    PENDING.set(before | what);
    if before == 0 && what != 0 {
        util::later(|| {
            let what = PENDING.replace(0);
            ui::refresh(what);
        });
    }
}

fn schedule_save() {
    if SAVE_SCHEDULED.replace(true) {
        return;
    }
    util::after(0.6, || {
        SAVE_SCHEDULED.set(false);
        save_now(false);
    });
}

pub fn save_now(sync: bool) {
    let data = store(|s| s.to_json());
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let write = move || {
        let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        if generation <= *written {
            return;
        }
        match util::write_atomic(&save_path(), &data, 0o600) {
            Ok(()) => *written = generation,
            Err(e) => util::log(format!("could not save profiles: {e}")),
        }
    };
    if sync {
        write();
    } else {
        std::thread::spawn(write);
    }
}

pub fn apply(effects: Vec<Effect>) {
    for e in effects {
        match e {
            Effect::DropTab(id) => {
                web::release(id);
                tabstate::delete(id);
            }
            Effect::ResetTab(id) => web::sleep_tab(id),
            Effect::DeleteClient { client_id, tab_ids } => {
                web::delete_client(client_id, &tab_ids);
                keychain::delete_login(client_id);
            }
            Effect::RemoveFile(path) => {
                let dir = util::pdf_previews_dir();
                let p = std::path::PathBuf::from(&path);
                if p.starts_with(&dir) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
}

fn mutate_apply(what: u8, f: impl FnOnce(&mut Store) -> Vec<Effect>) {
    let effects = mutate(what, f);
    apply(effects);
}

// ---- live page updates ----------------------------------------------------

/// A web view's title/URL/loading state changed. Coalesced: a page that
/// updates its title every second (unread badges) costs one refresh per
/// quarter second at most, not one per change.
pub fn web_state_changed(tab: Uuid) {
    META_PENDING.with_borrow_mut(|s| s.insert(tab));
    if META_SCHEDULED.replace(true) {
        return;
    }
    util::after(0.25, || {
        META_SCHEDULED.set(false);
        let tabs: Vec<Uuid> = META_PENDING.with_borrow_mut(|s| s.drain().collect());
        let active = store(|s| s.active_tab().map(|(_, t)| t.id));
        let mut any = false;
        for tab in tabs {
            let Some(ps) = web::page_state(tab) else { continue };
            let changed_meta = STORE.with_borrow_mut(|s| s.update_tab_meta(tab, Some(&ps.url), Some(&ps.title)));
            any |= changed_meta;
            if Some(tab) == active {
                changed(TOOLBAR);
            }
        }
        if any {
            changed(TABS);
            schedule_save();
        }
    });
}

// ---- clients ----------------------------------------------------------------

pub fn select_client(id: Uuid) {
    if store(|s| s.active_client_id == Some(id)) {
        return;
    }
    // Leaving a client is the moment its login is most worth backing up.
    web::capture_sessions(false);
    mutate(ALL, |s| s.select_client(id));
}

pub fn add_client(name: &str, color: &str, symbol: Option<String>, login: Option<keychain::Login>) {
    let id = mutate(ALL, |s| s.add_client(name, color, symbol));
    if let Some(l) = login.filter(|l| !l.is_empty()) {
        keychain::write_login(id, &l);
    }
}

pub fn update_client(id: Uuid, name: &str, color: &str, symbol: Option<String>, login: Option<keychain::Login>, forget_login: bool) {
    mutate(ALL, |s| s.update_client(id, name, color, symbol));
    if forget_login {
        keychain::delete_login(id);
    } else if let Some(l) = login.filter(|l| !l.is_empty()) {
        if keychain::read_login(id).as_ref() != Some(&l) {
            keychain::write_login(id, &l);
        }
    }
}

pub fn confirm_delete_client(id: Uuid) {
    let Some(name) = store(|s| s.client(id).filter(|c| !c.is_search()).map(|c| c.name.clone())) else { return };
    let (button, _) = web::run_alert(
        &format!("Delete “{name}”?"),
        "This signs the profile out and permanently removes its tabs, saved session and saved login from this Mac.",
        &["Delete", "Cancel"],
        None,
    );
    if button == 0 {
        mutate_apply(ALL, |s| s.delete_client(id));
    }
}

pub fn move_client(id: Uuid, offset: isize) {
    mutate(SIDEBAR, |s| s.move_client_by(id, offset));
}

pub fn move_client_over(id: Uuid, target: Uuid) {
    mutate(SIDEBAR, |s| s.move_client_over(id, target));
}

pub fn set_sort(mode: SortMode) {
    mutate(SIDEBAR, |s| s.set_sort_mode(mode));
}

/// Zoho's sign-in page reported credentials the user just submitted.
pub fn offer_to_save_login(client: Uuid, email: String, password: String) {
    let Some(name) = store(|s| s.client(client).filter(|c| !c.is_search()).map(|c| c.name.clone())) else { return };
    let new = keychain::Login { email, password };
    let existing = keychain::read_login(client);
    if existing.as_ref() == Some(&new) || DECLINED_LOGIN.with_borrow(|d| d.contains(&client)) {
        return;
    }
    let (title, button) = match &existing {
        Some(e) if e.email.eq_ignore_ascii_case(&new.email) => (format!("Update the saved password for “{name}”?"), "Update"),
        _ => (format!("Save this Zoho sign-in to “{name}”?"), "Save"),
    };
    let (choice, _) = web::run_alert(
        &title,
        &format!(
            "{}\n\nIt is stored encrypted in your Mac's Keychain, only for this profile, and used to sign it back in automatically if Zoho ever asks again. Deleting the profile removes it.",
            new.email
        ),
        &[button, "Not Now"],
        None,
    );
    if choice == 0 {
        keychain::write_login(client, &new);
    } else {
        DECLINED_LOGIN.with_borrow_mut(|d| d.insert(client));
    }
}

// ---- tabs -----------------------------------------------------------------

pub fn select_tab(client: Uuid, tab: Uuid) {
    mutate(TABS | CONTENT | TOOLBAR, |s| s.select_tab(client, tab));
}

pub fn new_tab() {
    if let Some(id) = store(|s| s.active_client_id) {
        mutate(ALL, |s| s.add_tab(id, None, None, None, true));
        ui::focus_new_tab_soon();
    }
}

pub fn close_tab(client: Uuid, tab: Uuid) {
    mutate_apply(ALL, |s| s.close_tab(client, tab));
}

pub fn close_active_tab() {
    if let Some((c, t)) = store(|s| s.active_tab().map(|(c, t)| (c.id, t.id))) {
        close_tab(c, t);
    }
}

pub fn reopen_closed_tab() {
    mutate(ALL, |s| s.reopen_closed_tab());
}

pub fn duplicate_tab() {
    let live = web::active_view().and_then(|v| unsafe { v.URL() }).and_then(|u| util::url_string(&u));
    mutate(ALL, |s| s.duplicate_active_tab(live));
}

pub fn cycle_tab(dir: isize) {
    mutate(TABS | CONTENT | TOOLBAR, |s| s.cycle_tab(dir));
}

pub fn select_tab_index(i: usize) {
    mutate(TABS | CONTENT | TOOLBAR, |s| s.select_tab_index(i));
}

pub fn select_last_tab() {
    mutate(TABS | CONTENT | TOOLBAR, |s| s.select_last_tab());
}

pub fn move_tab(client: Uuid, tab: Uuid, offset: isize) {
    mutate(TABS, |s| s.move_tab_by(client, tab, offset));
}

pub fn move_tab_over(client: Uuid, tab: Uuid, target: Uuid) {
    mutate(TABS, |s| s.move_tab_over(client, tab, target));
}

pub fn go_home() {
    mutate_apply(ALL, |s| s.go_home());
}

pub fn open_app(index: usize) {
    let Some(app) = ZOHO_APPS.get(index) else { return };
    let Some(tab) = store(|s| s.active_tab().map(|(_, t)| t.id)) else { return };
    mutate_apply(ALL, |s| s.open_app(tab, app));
}

pub fn open_meet() {
    mutate(ALL, |s| s.open_google_meet());
}

pub fn open_search(query: &str) {
    if let Some(u) = suggest::google_search_url(query) {
        open_search_url(&u);
    }
}

pub fn open_search_url(url: &Url) {
    mutate_apply(ALL, |s| s.open_google_search(url));
}

pub fn open_in_client_workspace(client: Uuid, url: Url) {
    mutate(ALL, |s| s.open_in_client_workspace(client, &url, None));
}

thread_local! {
    /// The last background tab a page opened: (opener tab, new tab).
    static LAST_BACKGROUND: Cell<Option<(Uuid, Uuid)>> = const { Cell::new(None) };
}

/// A page opened a new window for a URL this app shows: a new tab after
/// the current one. In the background (⌘-click) the user stays on the page
/// and the tab starts loading straight away, as in any browser; several
/// background tabs from one page line up in the order they were clicked.
pub fn open_new_tab_from_page(client: Uuid, ws: Workspace, url: Url, opener: Option<Uuid>, background: bool) {
    let ws = if policy::is_google_search(&url) { Workspace::GoogleSearch } else if ws == Workspace::GoogleSearch && policy::should_open_in_client_workspace(&url) { Workspace::Zoho } else { ws };
    if ws == Workspace::Zoho && client == GOOGLE_SEARCH_ID {
        return open_in_client_workspace(client, url);
    }
    let after = LAST_BACKGROUND
        .get()
        .filter(|(o, t)| background && Some(*o) == opener && store(|s| s.client(client).is_some_and(|c| c.tabs.iter().any(|x| x.id == *t))))
        .map(|(_, t)| t);
    let tab = mutate(ALL, |s| match after {
        Some(a) => s.add_tab_after(client, a, url.to_string(), "Loading…".into(), Some(ws)),
        None => s.add_tab(client, Some(url.to_string()), Some("Loading…".into()), Some(ws), !background),
    });
    let Some(tab) = tab else { return };
    web::mark_opened_by_page(tab);
    if background {
        LAST_BACKGROUND.set(opener.map(|o| (o, tab)));
        if let Some(t) = store(|s| s.find_tab(tab).cloned()) {
            web::view_for(client, &t);
        }
    }
}

/// A tab a page opened turned out to be only a download: close it, the way
/// a browser does.
pub fn discard_tab(client: Uuid, tab: Uuid) {
    mutate_apply(ALL, |s| s.discard_tab(client, tab));
}

/// A PDF finished saving to the previews folder: show it as a tab.
pub fn open_pdf_tab(client: Uuid, path: std::path::PathBuf, title: String, source_url: Option<String>) -> Option<Uuid> {
    let p = path.to_string_lossy().into_owned();
    let tab = mutate(ALL, |s| s.add_pdf_tab(client, &p, &title, source_url));
    if tab.is_none() {
        // The profile was deleted while the PDF downloaded.
        apply(vec![Effect::RemoveFile(p)]);
    }
    tab
}

/// Show a PDF from ~/Downloads as a tab of `client`. The tab gets its own
/// copy in the previews folder, so closing the tab never touches the user's
/// file and moving or renaming that file never breaks the tab.
pub fn open_downloaded_pdf(client: Uuid, path: &std::path::Path, name: &str, source_url: Option<String>) -> Option<Uuid> {
    store(|s| s.client(client).map(|_| ()))?;
    let copy = util::pdf_previews_dir().join(format!("{}.pdf", Uuid::new_v4()));
    if let Err(e) = std::fs::copy(path, &copy) {
        util::log(format!("could not open {} as a tab: {e}", path.display()));
        return None;
    }
    open_pdf_tab(client, copy, name.to_string(), source_url)
}

/// Bring a tab (and its profile) to the front. False if it no longer exists.
pub fn show_tab(client: Uuid, tab: Uuid) -> bool {
    let exists = store(|s| s.client(client).is_some_and(|c| c.tabs.iter().any(|t| t.id == tab)));
    if exists {
        mutate(ALL, |s| {
            s.select_client(client);
            s.select_tab(client, tab);
        });
    }
    exists
}

/// Self-test helper: open `url` in a new tab in front.
pub fn address_entered_in_new_tab(url: &str) {
    if let (Some(client), Some(u)) = (store(|s| s.active_client_id), policy::parse(url)) {
        mutate(ALL, |s| s.add_tab(client, Some(u.to_string()), None, None, true));
    }
}

/// What Enter in the address bar does.
pub fn address_entered(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let ws = store(|s| s.active_tab().map(|(_, t)| t.effective_workspace())).unwrap_or(Workspace::Zoho);
    if let Some(u) = policy::normalized_web_url(text, ws) {
        return load_destination(u);
    }
    if let Some(u) = policy::normalized_http_url(text) {
        if policy::is_google_search(&u) {
            return open_search_url(&u);
        }
        if policy::is_allowed_web(&u, Workspace::GoogleSearch) || policy::is_zoho_family(&u) {
            return load_destination(u);
        }
        // Any other website: the Mac's default browser, rather than nowhere.
        if let Some(ns) = util::ns_url(u.as_str()) {
            objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&ns);
        }
        return;
    }
    if let Some(u) = suggest::best_zoho_url(text) {
        return load_destination(u);
    }
    open_search(text);
}

fn load_destination(url: Url) {
    let Some((client, tab, ws, launcher)) =
        store(|s| s.active_tab().map(|(c, t)| (c.id, t.id, t.effective_workspace(), t.url.is_none() || t.pdf_path.is_some())))
    else {
        return;
    };
    if ws == Workspace::GoogleSearch && policy::should_open_in_client_workspace(&url) {
        return open_in_client_workspace(client, url);
    }
    if launcher || web::live_view(tab).is_none() {
        mutate_apply(ALL, |s| s.navigate_tab(tab, Some(url.to_string()), Some(ws).filter(|w| *w != Workspace::Zoho)));
    } else {
        web::load_url(&url);
    }
}

// ---- lifecycle ------------------------------------------------------------

pub fn start_background_work() {
    // Sessions are backed up every 45 s (only rewritten when they change) and
    // every awake tab's exact place every minute. Both timers carry slack so
    // macOS can batch the wake-ups.
    std::mem::forget(util::every(45.0, || web::capture_sessions(false)));
    std::mem::forget(util::every(60.0, web::save_all_states));
    web::watch_memory_pressure();
    std::mem::forget(util::every(300.0, web::sleep_idle_tabs));
    util::after(3.0, crate::session::migrate_legacy);
}

pub fn app_resigned_active() {
    web::save_all_states();
    web::capture_sessions(false);
    save_now(false);
}

pub fn app_will_quit() {
    web::save_all_states();
    web::capture_sessions(true);
    save_now(true);
    util::flush_writes();
}
