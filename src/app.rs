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

use crate::model::{Effect, GOOGLE_SEARCH_ID, SortMode, Store, Workspace, ZOHO_APPS};
use crate::{cleanup, keychain, policy, preview, suggest, tabstate, ui, util, web};

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
    /// The profile file could not be used and the user has not chosen what
    /// to do: nothing is written over it.
    static READ_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Highest save generation written so far. Saves run on a background thread;
/// the generation check stops an older snapshot from landing after a newer
/// one.
static WRITTEN: Mutex<u64> = Mutex::new(0);
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn save_path() -> std::path::PathBuf {
    util::support_dir().join("clients.json")
}

fn backup_path() -> std::path::PathBuf {
    util::support_dir().join("clients.json.bak")
}

/// Load the profiles. A file that exists but cannot be used is never
/// replaced: the app stays read-only until the user picks Restore, Start
/// Empty (the file is kept aside) or Quit.
pub fn load() {
    let started = std::time::Instant::now();
    load_store();
    if util::is_test_build() {
        util::log(format!("PERF profiles loaded in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0));
    }
}

fn load_store() {
    let path = save_path();
    let store = match crate::model::classify_read(std::fs::read(&path)) {
        Ok(Some(s)) => {
            // The last file that loaded cleanly, for Restore: written in the
            // background, and only when it differs from the current copy.
            if let Ok(data) = std::fs::read(&path)
                && std::fs::read(backup_path()).ok().as_deref() != Some(data.as_slice())
            {
                util::write_soon(backup_path(), data, 0o600);
            }
            s
        }
        Ok(None) => {
            let mut s = Store::default();
            s.repair();
            s
        }
        Err(problem) => recover(&problem),
    };
    let keep: Vec<Uuid> = store.clients.iter().flat_map(|c| c.tabs.iter().map(|t| t.id)).collect();
    if !READ_ONLY.get() {
        tabstate::prune(&keep);
    }
    STORE.with_borrow_mut(|s| *s = store);
}

fn recover(problem: &crate::model::LoadProblem) -> Store {
    READ_ONLY.set(true);
    util::log(format!("profiles file unusable: {problem}"));
    let backup = std::fs::read(backup_path()).ok().and_then(|d| crate::model::decode_store(&d).ok());
    let mut buttons = vec![];
    if backup.is_some() {
        buttons.push("Restore Last Good Copy");
    }
    buttons.extend(["Start with No Clients", "Quit"]);
    let (choice, _) = web::run_alert(
        "ZClients could not open your clients",
        &format!(
            "The clients file {problem}. It has not been changed.\n\nRestore Last Good Copy uses the copy saved the last time it opened correctly. Start with No Clients keeps the file aside (clients.json.damaged-…) so nothing is lost. Your clients' sign-ins (website data) are kept either way."
        ),
        &buttons,
        None,
    );
    let pick = buttons.get(choice).copied().unwrap_or("Quit");
    let aside = |path: &std::path::Path| {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let to = path.with_extension(format!("json.damaged-{stamp}"));
        std::fs::rename(path, &to).or_else(|_| std::fs::copy(path, &to).map(|_| ()))
    };
    match (pick, backup) {
        ("Restore Last Good Copy", Some(store)) => {
            if save_path().exists() && aside(&save_path()).is_err() {
                return store;
            }
            READ_ONLY.set(false);
            store
        }
        ("Start with No Clients", _) => {
            if save_path().exists() && aside(&save_path()).is_err() {
                util::log("could not keep the damaged file aside; staying read-only");
            } else {
                READ_ONLY.set(false);
            }
            let mut s = Store::default();
            s.repair();
            s
        }
        // Quit without writing anything.
        _ => std::process::exit(0),
    }
}

pub fn is_read_only() -> bool {
    READ_ONLY.get()
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
    if READ_ONLY.get() {
        return;
    }
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
                web::forget_tab(id);
                ui::forget_tab(id);
                tabstate::delete(id);
            }
            Effect::ResetTab(id) => {
                web::sleep_tab(id);
                web::forget_tab(id);
                ui::forget_tab(id);
            }
            Effect::DeleteClient { client_id, tab_ids } => {
                for t in &tab_ids {
                    web::forget_tab(*t);
                    ui::forget_tab(*t);
                }
                web::forget_client(client_id);
                let stores = web::release_client(client_id, &tab_ids);
                let name = DELETING_NAME.with_borrow_mut(|n| n.take()).unwrap_or_else(|| "Client".into());
                let previews = PENDING_PREVIEWS.with_borrow_mut(std::mem::take);
                cleanup::start(cleanup::Job::new(client_id, &name, &stores, &tab_ids, &previews));
            }
            Effect::RemoveFile(path) => {
                preview::remove_preview(&util::pdf_previews_dir(), std::path::Path::new(&path));
            }
        }
    }
}

thread_local! {
    static DELETING_NAME: RefCell<Option<String>> = const { RefCell::new(None) };
    static PENDING_PREVIEWS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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
            let changed_meta = STORE.with_borrow_mut(|s| s.update_tab_meta(tab, Some(&ps.url), Some(&ps.title)) | s.learn_data_centre(tab, &ps.url));
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

/// What the profile sheet does with the saved login.
pub enum LoginChange {
    /// Leave whatever is saved as it is.
    Keep,
    /// Save these details instead (both email and password given).
    Replace(keychain::Login),
    /// Remove the saved login.
    Remove,
}

pub fn add_client(name: &str, color: &str, symbol: Option<String>, login: LoginChange, retention: crate::session::Retention) {
    let id = mutate(ALL, |s| {
        let id = s.add_client(name, color, symbol);
        s.set_retention(id, retention);
        id
    });
    apply_login_change(id, login);
}

pub fn update_client(id: Uuid, name: &str, color: &str, symbol: Option<String>, login: LoginChange, retention: crate::session::Retention) {
    let changed = mutate(ALL, |s| {
        s.update_client(id, name, color, symbol);
        s.set_retention(id, retention)
    });
    if changed {
        web::retention_changed(id);
    }
    apply_login_change(id, login);
}

fn apply_login_change(id: Uuid, change: LoginChange) {
    match change {
        LoginChange::Keep => {}
        LoginChange::Replace(l) if !l.is_empty() => {
            if !keychain::write_login(id, &l) {
                web::run_alert("The login could not be saved", "The vault key in your Keychain is not available. Nothing was changed.", &["OK"], None);
            }
        }
        LoginChange::Replace(_) => {}
        LoginChange::Remove => {
            if !keychain::delete_login(id) {
                web::run_alert("The saved login could not be removed", "ZClients will not use it, and will try to remove it again when the client is deleted.", &["OK"], None);
            }
        }
    }
}

pub fn confirm_delete_client(id: Uuid) {
    let Some(name) = store(|s| s.client(id).filter(|c| !c.is_search()).map(|c| c.name.clone())) else { return };
    let (button, _) = web::run_alert(
        &format!("Delete “{name}”?"),
        "This signs the client out and removes its tabs, website data (cookies and storage), saved session, saved login and file previews from this Mac. Files in your Downloads folder are not touched.",
        &["Delete", "Cancel"],
        None,
    );
    if button == 0 {
        let previews: Vec<String> = store(|s| s.client(id).map(|c| c.tabs.iter().filter_map(|t| t.pdf_path.clone()).collect())).unwrap_or_default();
        DELETING_NAME.with_borrow_mut(|n| *n = Some(name));
        PENDING_PREVIEWS.with_borrow_mut(|p| *p = previews);
        mutate_apply(ALL, |s| s.delete_client(id));
    }
}

/// A deletion finished its pass: report what is still left, if anything,
/// and offer to try again.
pub fn cleanup_finished(job: cleanup::Job) {
    changed(TOOLBAR);
    if job.pending.is_empty() {
        return;
    }
    if util::is_test_build() && std::env::var_os("ZCLIENTS_SELFTEST").is_some() {
        return;
    }
    let (choice, _) = web::run_alert(
        &format!("Some of “{}”'s data is not removed yet", job.name),
        &format!("Still on this Mac: {}.\n\nZClients keeps trying each time it opens, and you can retry from Settings › Privacy.", job.remaining().join(", ")),
        &["Retry Now", "Later"],
        None,
    );
    if choice == 0 {
        util::later(move || cleanup::run(job.client));
    }
}

/// ⌃⌘↓ / ⌃⌘↑: the next or previous client in the sidebar.
pub fn cycle_client(dir: isize) {
    if let Some(id) = store(|s| s.client_offset(dir)) {
        select_client(id);
    }
}

/// ⌥⌘1 … ⌥⌘9: the n-th client (from 0) in the sidebar, Search not counted.
pub fn select_client_number(n: usize) {
    if let Some(id) = store(|s| s.client_ids_in_order().get(n).copied()) {
        select_client(id);
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
            "{}\n\nIt is stored encrypted in ZClients's vault, for this client only; the vault's key is kept in your Mac's Keychain. If Zoho asks you to sign in again, ZClients fills it in on Zoho's own sign-in page and you press Sign In. Deleting the client removes it.",
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

/// The user closes a tab (⌘W, the tab's ×, middle-click, its menu). A tab
/// in a call asks first: closing it hangs up.
pub fn request_close_tab(client: Uuid, tab: Uuid) {
    if let Some((camera, _)) = web::call_state(tab) {
        let title = store(|s| s.find_tab(tab).map(|t| t.display_title())).unwrap_or_default();
        let what = if camera { "camera and microphone" } else { "microphone" };
        let (choice, _) =
            web::run_alert(&format!("Close “{title}” and end the call?"), &format!("This tab is using the {what}. Closing it hangs up for you."), &["Close Tab", "Cancel"], None);
        if choice != 0 {
            return;
        }
    }
    close_tab(client, tab);
}

pub fn close_active_tab() {
    if let Some((c, t)) = store(|s| s.active_tab().map(|(c, t)| (c.id, t.id))) {
        request_close_tab(c, t);
    }
}

pub fn reopen_closed_tab() {
    mutate(ALL, |s| s.reopen_closed_tab());
}

pub fn duplicate_tab() {
    // ⌘D on a Meet tab is Meet's mute key, wherever the keyboard is.
    if web::forward_meet_shortcut() {
        return;
    }
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

/// The Meet button (⇧⌘M): an instant Google Meet meeting for the profile in
/// front, or back to the call it is already in. From the Search workspace
/// (no profile of its own) the user picks which profile hosts it.
pub fn open_meet() {
    let calling: Vec<Uuid> = web::tabs_in_calls().into_iter().map(|(_, tab, _, _)| tab).collect();
    if let Some(effects) = mutate(ALL, |s| s.open_google_meet(&calling)) {
        apply(effects);
        return;
    }
    if let Some(client) = choose_profile("Start a Google Meet meeting in which client?") {
        let effects = mutate(ALL, |s| s.open_google_meet_in(client, &calling)).unwrap_or_default();
        apply(effects);
    }
}

/// Ask the user to pick one of the real profiles.
pub fn choose_profile(question: &str) -> Option<Uuid> {
    let profiles: Vec<(Uuid, String)> = store(|s| s.ordered().iter().filter(|c| !c.is_search()).map(|c| (c.id, c.name.clone())).collect());
    match profiles.len() {
        0 => None,
        1 => Some(profiles[0].0),
        _ => ui::pick_profile(question, &profiles),
    }
}

pub fn open_search(query: &str) {
    if let Some(u) = suggest::google_search_url(query) {
        open_search_url(&u);
    }
}

pub fn open_search_url(url: &Url) {
    let origin = store(|s| s.active_client_id);
    mutate_apply(ALL, |s| s.open_google_search(url, origin));
}

/// A Zoho or Meet page reached from Search opens in a real profile: the one
/// the search was started from, or (if unknown or deleted) the one the user
/// picks. Never in the Search workspace's own store.
pub fn open_from_search(search_tab: Option<Uuid>, url: Url) {
    let origin = search_tab.and_then(|t| store(|s| s.search_origin(t)));
    let Some(client) = origin.or_else(|| choose_profile(&format!("Open {} in which client?", url.host_str().unwrap_or("this page")))) else { return };
    open_in_client_workspace(client, url);
}

pub fn open_in_client_workspace(client: Uuid, url: Url) {
    if client == GOOGLE_SEARCH_ID {
        return open_from_search(None, url);
    }
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
    let ws = if policy::is_google_search(&url) {
        Workspace::GoogleSearch
    } else if ws == Workspace::GoogleSearch && policy::search_route(&url) == policy::SearchRoute::Client {
        Workspace::Zoho
    } else {
        ws
    };
    if ws == Workspace::Zoho && client == GOOGLE_SEARCH_ID {
        return open_from_search(opener, url);
    }
    let after =
        LAST_BACKGROUND.get().filter(|(o, t)| background && Some(*o) == opener && store(|s| s.client(client).is_some_and(|c| c.tabs.iter().any(|x| x.id == *t)))).map(|(_, t)| t);
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

/// Show a PDF, picture, video or audio file from ~/Downloads as a tab of
/// `client`. The tab gets its own copy in the previews folder, so closing
/// the tab never touches the user's file and moving or renaming that file
/// never breaks the tab.
pub fn open_downloaded_preview(client: Uuid, path: &std::path::Path, name: &str, source_url: Option<String>) -> Option<Uuid> {
    store(|s| s.client(client).map(|_| ()))?;
    // Copied through the descriptor whose bytes were checked, into its own
    // private folder, named for what those bytes are.
    let copy = match preview::copy_into_preview(path, name) {
        Ok(c) => c,
        Err(e) => {
            util::log(format!("a downloaded file was not opened as a tab: {e}"));
            return None;
        }
    };
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

/// Return in the address bar, typed while `tab` was in front. Dropped if a
/// different tab is in front by the time it runs: one tab's text never
/// navigates another.
pub fn address_entered_for(tab: Option<Uuid>, text: &str) {
    let active = store(|s| s.active_tab().map(|(_, t)| t.id));
    if !address_target_ok(tab, active) {
        util::log("address entry dropped: the tab changed while typing");
        return;
    }
    address_entered(text);
}

pub fn address_target_ok(editing: Option<Uuid>, active: Option<Uuid>) -> bool {
    editing.is_some() && editing == active
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
        // The Search workspace opens nothing else, not even elsewhere.
        if ws == Workspace::GoogleSearch {
            if let Some(tab) = store(|s| s.active_tab().map(|(_, t)| t.id)) {
                ui::show_load_error(tab, web::LoadError::BlockedInSearch, Some(u.to_string()));
            }
            return;
        }
        // Any other website from a client: the Mac's default browser,
        // rather than nowhere.
        if let Some(ns) = util::ns_url(u.as_str()) {
            objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&ns);
        }
        return;
    }
    let dc = store(|s| s.active_data_centre().map(str::to_string));
    if let Some(u) = suggest::best_zoho_url(text, dc.as_deref()) {
        return load_destination(u);
    }
    open_search(text);
}

fn load_destination(url: Url) {
    let url = policy::regional_entry(&url).unwrap_or(url);
    let Some((_client, tab, ws, launcher)) = store(|s| s.active_tab().map(|(c, t)| (c.id, t.id, t.effective_workspace(), t.url.is_none() || t.pdf_path.is_some()))) else {
        return;
    };
    // From Search, Meet goes to a client; Google and Zoho pages load here.
    if ws == Workspace::GoogleSearch && policy::search_route(&url) == policy::SearchRoute::Client {
        return open_from_search(Some(tab), url);
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
    web::watch_network(ui::network_restored);
    std::mem::forget(util::every(300.0, web::sleep_idle_tabs));
    util::after(3.0, crate::session::migrate_legacy);
    util::after(2.0, || {
        let n = keychain::migrate();
        if n > 0 {
            util::log(format!("vault: {n} record(s) re-sealed in format 2"));
        }
    });
    util::after(1.0, cleanup::resume_all);
    if !READ_ONLY.get() {
        util::after(4.0, || {
            migrate_flat_previews();
            prune_previews();
        });
    }
}

/// Every preview file a tab (open, or closed but reopenable this run) still
/// refers to.
fn referenced_previews() -> HashSet<std::path::PathBuf> {
    store(|s| s.preview_paths()).into_iter().map(std::path::PathBuf::from).collect()
}

/// Remove previews no tab refers to. Run at launch and on a clean quit:
/// a preview lives as long as its tab (and, once closed, until quit).
pub fn prune_previews() -> usize {
    if READ_ONLY.get() {
        return 0;
    }
    let n = preview::prune(&util::pdf_previews_dir(), &referenced_previews());
    if n > 0 {
        util::log(format!("removed {n} unused preview(s)"));
    }
    n
}

/// Previews kept by 1.4.0 and earlier sit side by side in one folder, so
/// WebKit could read all of them from any PDF tab. Each moves into its own
/// private folder; an invalid one is dropped from its tab.
fn migrate_flat_previews() {
    let root = util::pdf_previews_dir();
    let tabs: Vec<(Uuid, String, String)> = store(|s| s.clients.iter().flat_map(|c| c.tabs.iter().filter_map(|t| Some((t.id, t.pdf_path.clone()?, t.title.clone())))).collect());
    for (tab, path, title) in tabs {
        let p = std::path::Path::new(&path);
        if preview::preview_dir_of(&root, p).is_some() {
            continue;
        }
        let moved = preview::migrate_flat(&root, p, &title).map(|m| m.to_string_lossy().into_owned());
        mutate(TABS | CONTENT, |s| s.set_pdf_path(tab, moved));
    }
}

/// Settings › Privacy: remove every PDF preview not on screen in a tab and
/// clear the downloads list. Files in ~/Downloads are not touched.
pub fn clear_previews_and_download_history() -> usize {
    mutate(ALL, |s| s.forget_closed_tabs());
    crate::downloads::clear_finished();
    prune_previews()
}

pub fn app_resigned_active() {
    web::save_all_states();
    web::capture_sessions(false);
    save_now(false);
}

thread_local! {
    /// The user already agreed to quit (or a self-test is ending).
    static QUIT_CONFIRMED: Cell<bool> = const { Cell::new(false) };
}

pub enum QuitReply {
    Now,
    Cancel,
    /// Downloads are being cancelled; `replyToApplicationShouldTerminate`
    /// follows once their partial files are gone.
    Later,
}

/// What to ask before quitting with `downloads` files still downloading
/// and `calls` tabs using the camera or microphone. None: nothing to lose.
pub fn quit_question(downloads: usize, calls: usize) -> Option<String> {
    let files = match downloads {
        0 => None,
        1 => Some("cancel 1 download".to_string()),
        n => Some(format!("cancel {n} downloads")),
    };
    let call = match calls {
        0 => None,
        1 => Some("end the call".to_string()),
        n => Some(format!("end {n} calls")),
    };
    match (files, call) {
        (None, None) => None,
        (Some(f), None) => Some(format!("Quit and {f}?")),
        (None, Some(c)) => Some(format!("Quit and {c}?")),
        (Some(f), Some(c)) => Some(format!("Quit, {f} and {c}?")),
    }
}

/// Quitting would cancel downloads (WebKit writes straight to the final
/// name in ~/Downloads, so a half-written file would be left looking
/// complete) or hang up a call: ask. On Quit, live downloads are cancelled
/// and their partial files deleted before the app exits.
pub fn should_quit() -> QuitReply {
    if QUIT_CONFIRMED.get() {
        return QuitReply::Now;
    }
    let downloading = crate::downloads::active_count();
    let Some(question) = quit_question(downloading, web::tabs_in_calls().len()) else { return QuitReply::Now };
    let (choice, _) = web::run_alert(&question, "Partly downloaded files are deleted. Anyone in a call with you sees you leave.", &["Quit", "Cancel"], None);
    if choice != 0 {
        return QuitReply::Cancel;
    }
    QUIT_CONFIRMED.set(true);
    if downloading == 0 {
        return QuitReply::Now;
    }
    let partial = crate::downloads::cancel_all();
    // WebKit may still flush a last chunk after cancelling: remove the
    // files again just before exiting.
    util::after(0.6, move || {
        for p in partial {
            let _ = std::fs::remove_file(p);
        }
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            objc2_app_kit::NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
        }
    });
    QuitReply::Later
}

/// Self-tests end by quitting without being asked.
pub fn quit_without_asking() {
    QUIT_CONFIRMED.set(true);
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        objc2_app_kit::NSApplication::sharedApplication(mtm).terminate(None);
    }
}

pub fn app_will_quit() {
    web::save_all_states();
    web::capture_sessions(true);
    // Closed tabs are not reopenable after quitting: their previews go.
    mutate(0, |s| s.forget_closed_tabs());
    save_now(true);
    prune_previews();
    util::flush_writes();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quitting_asks_only_when_something_would_be_lost() {
        assert_eq!(quit_question(0, 0), None);
        assert_eq!(quit_question(1, 0).as_deref(), Some("Quit and cancel 1 download?"));
        assert_eq!(quit_question(2, 0).as_deref(), Some("Quit and cancel 2 downloads?"));
        assert_eq!(quit_question(0, 1).as_deref(), Some("Quit and end the call?"));
        assert_eq!(quit_question(2, 1).as_deref(), Some("Quit, cancel 2 downloads and end the call?"));
        assert_eq!(quit_question(1, 2).as_deref(), Some("Quit, cancel 1 download and end 2 calls?"));
    }

    #[test]
    fn address_text_only_navigates_the_tab_it_was_typed_for() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(address_target_ok(Some(a), Some(a)));
        assert!(!address_target_ok(Some(a), Some(b)), "tab switched while typing");
        assert!(!address_target_ok(None, Some(a)), "editing began before any tab was known");
        assert!(!address_target_ok(Some(a), None), "the tab was closed");
    }
}
