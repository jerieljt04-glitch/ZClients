//! "Keep me signed in": keeps each profile's Zoho/Google sign-in across
//! quits, for as long as that profile's retention allows, and never longer
//! than the user or the server wants.
//!
//! WebKit's own cookie store is the source of truth. The keeper only:
//!
//!   * **Promotes** a sign-in *session* cookie (no expiry, which WebKit drops
//!     on quit) to a persistent one that lives for the profile's retention
//!     period, and renews cookies it promoted while the profile is in use
//!     (rolling). A cookie the server re-issues with its own expiry is left
//!     exactly as the server set it — a shortened expiry stays short.
//!   * **Backs up** the sign-in cookies of a store into the encrypted vault,
//!     rewriting only when something (including an exact expiry) changed.
//!   * **Restores** backed-up cookies a store is missing before its first
//!     page load (tracking prevention can purge storage), never beyond the
//!     retention period and never a cookie whose own expiry has passed.
//!
//! Logout is respected: once a store is known to be loaded (its first
//! restore ran and either sign-in cookies were seen or a page finished
//! loading), a jar with no sign-in cookies left means the user signed out
//! or the server ended the session, and the backup is deleted rather than
//! kept. A store that simply has not loaded yet never erases a backup.
//! A cookie-store observer captures within seconds of any change, so a quit
//! or crash right after signing out cannot bring the old session back.
//!
//! Only Zoho data-centre and Google account cookie domains are ever kept
//! (`policy::is_persistable_cookie_domain`).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSDate, NSHTTPCookie, NSHTTPCookieDomain, NSHTTPCookieExpires, NSHTTPCookieName, NSHTTPCookiePath, NSHTTPCookieSameSitePolicy, NSHTTPCookieSecure, NSHTTPCookieValue,
    NSHTTPCookieVersion, NSMutableDictionary, NSObject, NSObjectProtocol, NSRunLoop, NSString,
};
use objc2_web_kit::{WKHTTPCookieStore, WKHTTPCookieStoreObserver, WKWebsiteDataStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{keychain, policy, util};

const DAY: f64 = 24.0 * 3600.0;

/// The retention choices offered per profile, in days.
pub const RETENTION_CHOICES: &[u32] = &[7, 30, 90, 365];
pub const DEFAULT_RETENTION_DAYS: u32 = 90;

/// A profile's choice. `keep == false` turns the keeper off entirely for it:
/// nothing is promoted, backed up or restored (WebKit still keeps whatever
/// persistent cookies the site itself set).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Retention {
    pub keep: bool,
    pub days: u32,
}

impl Retention {
    pub fn seconds(&self) -> f64 {
        self.days.clamp(1, 400) as f64 * DAY
    }
}

impl Default for Retention {
    fn default() -> Self {
        Retention { keep: true, days: DEFAULT_RETENTION_DAYS }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub is_secure: bool,
    #[serde(rename = "isHTTPOnly")]
    pub is_http_only: bool,
    /// Seconds since 1970; None is a session cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(default)]
    pub version: i64,
}

impl StoredCookie {
    pub fn key(&self) -> String {
        format!("{}\u{1}{}\u{1}{}", self.name, self.domain.to_ascii_lowercase(), self.path)
    }
}

/// The sealed backup. Version 3 adds `savedAt` and `promoted`; versions 1
/// (Swift) and 2 (Rust ≤ 1.4.0) still decode.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Jar {
    #[serde(default)]
    pub version: u32,
    /// When this backup was taken (seconds since 1970).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<f64>,
    #[serde(default)]
    pub cookies: Vec<StoredCookie>,
    /// Cookies the keeper promoted from session cookies: key → the expiry it
    /// gave them. A live expiry that differs means the server re-set it.
    #[serde(default)]
    pub promoted: BTreeMap<String, f64>,
}

pub const JAR_VERSION: u32 = 3;

fn same_time(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0
}

/// Which backed-up cookies to put back into a store, and with what expiry.
///
/// * Cookies the store already has always win.
/// * Nothing when the profile does not keep sessions, or the backup itself is
///   older than the retention period.
/// * A cookie whose expiry has passed stays dead; no cookie gets more than
///   the retention period from now (a clock that jumped, or a 1.4.0 backup
///   carrying 400-day expiries, cannot stretch it).
pub fn restore_plan(jar: &Jar, present: &HashSet<String>, now: f64, retention: Retention) -> Vec<StoredCookie> {
    if !retention.keep {
        return vec![];
    }
    let limit = now + retention.seconds();
    // A saved_at in the future (clock moved back) counts as now.
    if jar.saved_at.is_some_and(|t| t.min(now) + retention.seconds() < now) {
        return vec![];
    }
    jar.cookies
        .iter()
        .filter(|c| !present.contains(&c.key()) && policy::is_persistable_cookie_domain(&c.domain))
        .filter_map(|c| {
            let expires = match c.expires {
                Some(e) if e <= now => return None,
                Some(e) => e.min(limit),
                None => limit,
            };
            Some(StoredCookie { expires: Some(expires), ..c.clone() })
        })
        .collect()
}

/// What to re-save into a live store: session cookies made persistent, and
/// cookies the keeper promoted earlier renewed once half their period has
/// passed. Returns those cookies and the updated `promoted` ledger.
pub fn promotion_plan(live: &[StoredCookie], promoted: &BTreeMap<String, f64>, now: f64, retention: Retention) -> (Vec<StoredCookie>, BTreeMap<String, f64>) {
    let mut ledger = BTreeMap::new();
    let mut out = vec![];
    if !retention.keep {
        return (out, ledger);
    }
    let period = retention.seconds();
    for c in live.iter().filter(|c| policy::is_persistable_cookie_domain(&c.domain)) {
        let key = c.key();
        match (c.expires, promoted.get(&key)) {
            (None, _) => {
                let e = now + period;
                ledger.insert(key, e);
                out.push(StoredCookie { expires: Some(e), ..c.clone() });
            }
            (Some(live_e), Some(&ours)) if same_time(live_e, ours) => {
                if live_e - now < period / 2.0 {
                    let e = now + period;
                    ledger.insert(key, e);
                    out.push(StoredCookie { expires: Some(e), ..c.clone() });
                } else {
                    ledger.insert(key, ours);
                }
            }
            // The server set this expiry itself (or it never was ours):
            // leave it exactly as it is.
            _ => {}
        }
    }
    (out, ledger)
}

/// The store's state as far as the keeper knows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StoreState {
    /// The first restore has not completed: an empty jar means nothing.
    Loading,
    /// Restored, but no sign-in cookie or finished page seen yet.
    Restored,
    /// Known loaded: sign-in cookies were seen, or a page finished loading.
    Loaded,
}

#[derive(Debug, PartialEq)]
pub enum CaptureAction {
    Nothing,
    Write(Jar),
    /// The store is loaded and holds no sign-in cookies: signed out.
    Delete,
}

/// What a capture should do with the backup, given the live sign-in cookies
/// (after promotion) and the current ledger.
pub fn capture_action(state: StoreState, live: Vec<StoredCookie>, promoted: BTreeMap<String, f64>, retention: Retention, now: f64) -> CaptureAction {
    if !retention.keep {
        return if state == StoreState::Loading { CaptureAction::Nothing } else { CaptureAction::Delete };
    }
    match (state, live.is_empty()) {
        (StoreState::Loading, _) | (StoreState::Restored, true) => CaptureAction::Nothing,
        (StoreState::Loaded, true) => CaptureAction::Delete,
        (_, false) => {
            let promoted = promoted.into_iter().filter(|(k, _)| live.iter().any(|c| c.key() == *k)).collect();
            CaptureAction::Write(Jar { version: JAR_VERSION, saved_at: Some(now), cookies: live, promoted })
        }
    }
}

/// Identity of a jar's contents, including exact expiries (to the second),
/// so a server shortening a cookie's life is written to the backup.
pub fn jar_hash(cookies: &[StoredCookie]) -> u64 {
    let mut sorted: Vec<&StoredCookie> = cookies.iter().collect();
    sorted.sort_by_key(|c| c.key());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in sorted {
        (&c.name, &c.domain, &c.path, &c.value, c.is_secure, c.is_http_only, c.expires.map(|e| e.round() as i64), &c.same_site).hash(&mut h);
    }
    h.finish()
}

fn now_unix() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ---- NSHTTPCookie <-> StoredCookie ------------------------------------------

fn from_ns(c: &NSHTTPCookie) -> Option<StoredCookie> {
    let name = c.name().to_string();
    let domain = c.domain().to_string();
    if name.is_empty() || domain.is_empty() {
        return None;
    }
    let path = c.path().to_string();
    Some(StoredCookie {
        name,
        value: c.value().to_string(),
        domain,
        path: if path.is_empty() { "/".into() } else { path },
        is_secure: c.isSecure(),
        is_http_only: c.isHTTPOnly(),
        expires: if c.isSessionOnly() { None } else { c.expiresDate().map(|d| d.timeIntervalSince1970()) },
        same_site: c.sameSitePolicy().map(|s| s.to_string()),
        version: c.version() as i64,
    })
}

fn to_ns(c: &StoredCookie) -> Option<Retained<NSHTTPCookie>> {
    let props: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
    let put = |k: &NSString, v: &AnyObject| props.insert(k, v);
    unsafe {
        put(NSHTTPCookieName, &util::ns(&c.name));
        put(NSHTTPCookieValue, &util::ns(&c.value));
        put(NSHTTPCookieDomain, &util::ns(&c.domain));
        put(NSHTTPCookiePath, &util::ns(&c.path));
        put(NSHTTPCookieVersion, &util::ns(&c.version.to_string()));
        if let Some(e) = c.expires {
            put(NSHTTPCookieExpires, &NSDate::dateWithTimeIntervalSince1970(e));
        }
        if c.is_secure {
            put(NSHTTPCookieSecure, &util::ns("TRUE"));
        }
        if c.is_http_only {
            put(&util::ns("HttpOnly"), &util::ns("TRUE"));
        }
        if let Some(s) = &c.same_site {
            put(NSHTTPCookieSameSitePolicy, &util::ns(s));
        }
        NSHTTPCookie::cookieWithProperties(&Retained::cast_unchecked::<objc2_foundation::NSDictionary<NSString, AnyObject>>(props))
    }
}

// ---- keeper state -------------------------------------------------------------

struct StoreEntry {
    store: Retained<WKWebsiteDataStore>,
    retention: Retention,
    observer: Option<Retained<CookieObserver>>,
}

#[derive(Default)]
struct Keeper {
    hydrated: HashSet<Uuid>,
    /// Stores known loaded this run (see `StoreState::Loaded`).
    loaded: HashSet<Uuid>,
    waiters: HashMap<Uuid, Vec<Box<dyn FnOnce()>>>,
    last_written: HashMap<Uuid, u64>,
    promoted: HashMap<Uuid, BTreeMap<String, f64>>,
    stores: HashMap<Uuid, StoreEntry>,
    capture_scheduled: HashSet<Uuid>,
    flushing: bool,
}

thread_local! {
    static KEEPER: RefCell<Keeper> = RefCell::new(Keeper::default());
}

define_class!(
    /// Hears every cookie change in one store and captures it shortly after,
    /// so a sign-out reaches the backup within seconds.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Uuid]
    struct CookieObserver;

    unsafe impl NSObjectProtocol for CookieObserver {}

    unsafe impl WKHTTPCookieStoreObserver for CookieObserver {
        #[unsafe(method(cookiesDidChangeInCookieStore:))]
        fn cookies_changed(&self, _store: &WKHTTPCookieStore) {
            schedule_capture(*self.ivars());
        }
    }
);

fn schedule_capture(id: Uuid) {
    if !KEEPER.with_borrow_mut(|k| k.hydrated.contains(&id) && k.capture_scheduled.insert(id)) {
        return;
    }
    // Coalesced: a busy page changes cookies constantly; one capture per
    // 8 s is enough to notice a sign-out long before anyone quits.
    util::after(8.0, move || {
        let entry = KEEPER.with_borrow_mut(|k| {
            k.capture_scheduled.remove(&id);
            k.stores.get(&id).map(|e| (e.store.clone(), e.retention))
        });
        if let Some((store, retention)) = entry {
            capture(&store, id, retention, None);
        }
    });
}

/// A profile's store is in use: watch its cookies.
pub fn register(id: Uuid, store: &WKWebsiteDataStore, retention: Retention) {
    if KEEPER.with_borrow(|k| k.stores.contains_key(&id)) {
        return set_retention(id, retention);
    }
    let observer = MainThreadMarker::new().map(|mtm| {
        let this = CookieObserver::alloc(mtm).set_ivars(id);
        let o: Retained<CookieObserver> = unsafe { msg_send![super(this), init] };
        unsafe { store.httpCookieStore().addObserver(ProtocolObject::from_ref(&*o)) };
        o
    });
    let entry = StoreEntry { store: store.retain_store(), retention, observer };
    KEEPER.with_borrow_mut(|k| k.stores.insert(id, entry));
}

trait RetainStore {
    fn retain_store(&self) -> Retained<WKWebsiteDataStore>;
}

impl RetainStore for WKWebsiteDataStore {
    fn retain_store(&self) -> Retained<WKWebsiteDataStore> {
        objc2::Message::retain(self)
    }
}

/// The user changed a profile's "Keep me signed in". Turning it off deletes
/// the backup now; turning it on starts backing up from the next capture.
pub fn set_retention(id: Uuid, retention: Retention) {
    let store = KEEPER.with_borrow_mut(|k| {
        let e = k.stores.get_mut(&id)?;
        e.retention = retention;
        k.last_written.remove(&id);
        Some(e.store.clone())
    });
    if !retention.keep {
        keychain::delete_session(id);
    }
    if let Some(store) = store {
        capture(&store, id, retention, None);
    }
}

/// A page in this store finished loading: its cookies are certainly loaded.
pub fn note_page_loaded(id: Uuid) {
    let first = KEEPER.with_borrow_mut(|k| k.hydrated.contains(&id) && k.loaded.insert(id));
    if first {
        schedule_capture(id);
    }
}

fn state_of(k: &Keeper, id: Uuid) -> StoreState {
    if !k.hydrated.contains(&id) {
        StoreState::Loading
    } else if k.loaded.contains(&id) {
        StoreState::Loaded
    } else {
        StoreState::Restored
    }
}

fn legacy_file(id: Uuid) -> std::path::PathBuf {
    util::support_dir().join("Sessions").join(format!("{}.json", id.hyphenated().to_string().to_uppercase()))
}

pub fn decode_jar(data: &[u8]) -> Option<Jar> {
    serde_json::from_slice::<Jar>(data).ok()
}

/// The backed-up jar: the vault copy, or a plaintext file left by the Swift
/// build, which is sealed into the vault and then deleted.
fn load_backup(id: Uuid) -> Jar {
    if let Some(mut data) = keychain::read_session(id) {
        let jar = decode_jar(&data);
        keychain::wipe(&mut data);
        if let Some(jar) = jar {
            return jar;
        }
    }
    let path = legacy_file(id);
    let Ok(data) = std::fs::read(&path) else { return Jar::default() };
    let cookies: Vec<StoredCookie> = decode_jar(&data).map(|j| j.cookies).unwrap_or_default().into_iter().filter(|c| policy::is_persistable_cookie_domain(&c.domain)).collect();
    let jar = Jar { version: JAR_VERSION, saved_at: Some(now_unix()), cookies, promoted: BTreeMap::new() };
    if !jar.cookies.is_empty() && write_backup(id, &jar) {
        let _ = std::fs::remove_file(&path);
    }
    jar
}

fn write_backup(id: Uuid, jar: &Jar) -> bool {
    let mut data = serde_json::to_vec(jar).unwrap_or_default();
    let ok = keychain::write_session(id, &data);
    keychain::wipe(&mut data);
    ok
}

fn write_backup_soon(id: Uuid, jar: &Jar) -> bool {
    let mut data = serde_json::to_vec(jar).unwrap_or_default();
    let ok = keychain::write_session_soon(id, &data);
    keychain::wipe(&mut data);
    ok
}

/// Put back any backed-up cookie `store` is missing, then call `done` on the
/// main thread. Repeat calls never restore twice; callers that arrive while a
/// restore is running are queued behind it, which is what lets a tab hold its
/// first page load until its session is back.
pub fn hydrate(store: &WKWebsiteDataStore, id: Uuid, retention: Retention, done: impl FnOnce() + 'static) {
    if KEEPER.with_borrow(|k| k.hydrated.contains(&id)) {
        return done();
    }
    let first = KEEPER.with_borrow_mut(|k| {
        let first = !k.waiters.contains_key(&id);
        k.waiters.entry(id).or_default().push(Box::new(done));
        first
    });
    if !first {
        return;
    }
    let started = std::time::Instant::now();

    let backup = if retention.keep { load_backup(id) } else { Jar::default() };
    KEEPER.with_borrow_mut(|k| k.promoted.insert(id, backup.promoted.clone()));
    // Nothing to put back: the first page need not wait for the cookie
    // store at all (the store counts as loaded once a page finishes).
    if backup.cookies.is_empty() {
        return finish(id);
    }
    // A wedged network process never calls back, and the tab would stay
    // blank for good. After a few seconds the page loads anyway (signed
    // out at worst); a late callback still restores the cookies.
    util::after(HYDRATE_TIMEOUT_SECS, move || {
        if KEEPER.with_borrow(|k| k.waiters.contains_key(&id)) {
            util::log(format!("session restore did not answer in {HYDRATE_TIMEOUT_SECS} s; loading without waiting"));
            finish(id);
        }
    });
    let cookie_store = unsafe { store.httpCookieStore() };
    let cs = cookie_store.clone();
    let block = RcBlock::new(move |existing: NonNull<NSArray<NSHTTPCookie>>| {
        let existing = unsafe { existing.as_ref() };
        let live: Vec<StoredCookie> = existing.iter().filter_map(|c| from_ns(&c)).collect();
        if live.iter().any(|c| policy::is_persistable_cookie_domain(&c.domain)) {
            KEEPER.with_borrow_mut(|k| k.loaded.insert(id));
        }
        let present: HashSet<String> = live.iter().map(|c| c.key()).collect();
        let plan = restore_plan(&backup, &present, now_unix(), retention);
        let cookies: Vec<Retained<NSHTTPCookie>> = plan.iter().filter_map(to_ns).collect();
        if util::is_test_build() {
            util::log(format!("hydrate: backup {} cookies, store had {}, restored {}", backup.cookies.len(), present.len(), cookies.len()));
            util::log(format!("PERF first load held {:.1} ms for the session restore", started.elapsed().as_secs_f64() * 1000.0));
        }
        if cookies.is_empty() {
            return finish(id);
        }
        let array = NSArray::from_retained_slice(&cookies);
        let done = RcBlock::new(move || finish(id));
        unsafe { cs.setCookies_completionHandler(&array, Some(&done)) };
    });
    unsafe { cookie_store.getAllCookies(&block) };
}

/// How long a tab's first load waits for its session to come back.
const HYDRATE_TIMEOUT_SECS: f64 = 4.0;

/// The store is ready: release every load waiting on it. Safe to call more
/// than once (the watchdog and a late WebKit callback can both get here):
/// each waiter runs exactly once.
fn finish(id: Uuid) {
    let waiters = KEEPER.with_borrow_mut(|k| {
        k.hydrated.insert(id);
        k.waiters.remove(&id).unwrap_or_default()
    });
    for w in waiters {
        w();
    }
}

/// Promote/renew session cookies and bring the backup in line with the live
/// store. `done` runs when finished.
pub fn capture(store: &WKWebsiteDataStore, id: Uuid, retention: Retention, done: Option<Box<dyn FnOnce()>>) {
    let cookie_store = unsafe { store.httpCookieStore() };
    let cs = cookie_store.clone();
    let done = RefCell::new(done);
    let block = RcBlock::new(move |all: NonNull<NSArray<NSHTTPCookie>>| {
        let all = unsafe { all.as_ref() };
        // Check the domain before converting: most cookies in a store are
        // analytics and widget cookies the keeper never keeps.
        let mut live: Vec<StoredCookie> = all.iter().filter(|c| policy::is_persistable_cookie_domain(&c.domain().to_string())).filter_map(|c| from_ns(&c)).collect();
        let now = now_unix();
        let (state, old_ledger) = KEEPER.with_borrow_mut(|k| {
            if !live.is_empty() && k.hydrated.contains(&id) {
                k.loaded.insert(id);
            }
            (state_of(k, id), k.promoted.get(&id).cloned().unwrap_or_default())
        });
        let (promote, ledger) = if state == StoreState::Loading { (vec![], old_ledger) } else { promotion_plan(&live, &old_ledger, now, retention) };
        if !promote.is_empty() {
            let ns: Vec<Retained<NSHTTPCookie>> = promote.iter().filter_map(to_ns).collect();
            // WebKit invokes this handler without a nil check, so it must
            // always be a real block.
            let noop = RcBlock::new(|| {});
            unsafe { cs.setCookies_completionHandler(&NSArray::from_retained_slice(&ns), Some(&noop)) };
            for p in promote {
                if let Some(slot) = live.iter_mut().find(|c| c.key() == p.key()) {
                    *slot = p;
                }
            }
        }
        KEEPER.with_borrow_mut(|k| k.promoted.insert(id, ledger.clone()));
        match capture_action(state, live, ledger, retention, now) {
            CaptureAction::Nothing => {}
            CaptureAction::Delete => {
                let had = KEEPER.with_borrow_mut(|k| k.last_written.insert(id, 0) != Some(0));
                if had || keychain::session_exists(id) {
                    if util::is_test_build() {
                        util::log("capture: signed out, session backup removed");
                    }
                    keychain::delete_session(id);
                }
            }
            CaptureAction::Write(jar) => {
                let hash = jar_hash(&jar.cookies) ^ jar.promoted.len() as u64;
                let changed = KEEPER.with_borrow(|k| k.last_written.get(&id) != Some(&hash));
                if changed && write_backup_soon(id, &jar) {
                    KEEPER.with_borrow_mut(|k| k.last_written.insert(id, hash));
                }
            }
        }
        if let Some(done) = done.borrow_mut().take() {
            done();
        }
    });
    unsafe { cookie_store.getAllCookies(&block) };
}

/// Capture every live store. With `wait` (used on quit) the main run loop is
/// pumped until the writes finish, bounded so a wedged store cannot hang quit.
pub fn capture_all(wait: bool) {
    let stores: Vec<(Uuid, Retained<WKWebsiteDataStore>, Retention)> = KEEPER.with_borrow(|k| k.stores.iter().map(|(id, e)| (*id, e.store.clone(), e.retention)).collect());
    if stores.is_empty() {
        return;
    }
    if !wait {
        for (id, s, r) in &stores {
            capture(s, *id, *r, None);
        }
        return;
    }
    if KEEPER.with_borrow(|k| k.flushing) {
        return;
    }
    KEEPER.with_borrow_mut(|k| k.flushing = true);
    let remaining = std::rc::Rc::new(std::cell::Cell::new(stores.len()));
    for (id, s, r) in &stores {
        let rem = remaining.clone();
        capture(s, *id, *r, Some(Box::new(move || rem.set(rem.get() - 1))));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let run_loop = NSRunLoop::currentRunLoop();
    while remaining.get() > 0 && std::time::Instant::now() < deadline {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
        unsafe { run_loop.runMode_beforeDate(objc2_foundation::NSDefaultRunLoopMode, &until) };
    }
    KEEPER.with_borrow_mut(|k| k.flushing = false);
}

/// Move every plaintext backup the Swift build left in `Sessions/` into the
/// encrypted vault, then remove the plaintext. Files are only deleted once
/// their contents are safely sealed.
pub fn migrate_legacy() {
    let dir = util::support_dir().join("Sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(".json").and_then(|s| Uuid::parse_str(s).ok()) {
            if keychain::read_session(id).is_none() {
                let _ = load_backup(id);
            } else {
                // Already sealed by an earlier run; the plaintext is stale.
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(&dir); // only succeeds once it is empty
}

/// Forget a deleted profile's backup and stop watching its store.
pub fn forget(id: Uuid) {
    let entry = KEEPER.with_borrow_mut(|k| {
        k.hydrated.remove(&id);
        k.loaded.remove(&id);
        k.waiters.remove(&id);
        k.last_written.remove(&id);
        k.promoted.remove(&id);
        k.stores.remove(&id)
    });
    if let Some(StoreEntry { store, observer: Some(o), .. }) = entry {
        unsafe { store.httpCookieStore().removeObserver(ProtocolObject::from_ref(&*o)) };
    }
    keychain::delete_session(id);
    let _ = std::fs::remove_file(legacy_file(id));
}

/// The user cleared this store's cookies: drop its backup and ledger so
/// nothing is put back.
pub fn clear_backup(id: Uuid) {
    KEEPER.with_borrow_mut(|k| {
        k.last_written.remove(&id);
        k.promoted.remove(&id);
        k.loaded.remove(&id);
    });
    keychain::delete_session(id);
    let _ = std::fs::remove_file(legacy_file(id));
}

/// Has every trace of this store's backup gone (for verified deletion)?
pub fn backup_gone(id: Uuid) -> bool {
    !keychain::session_exists(id) && !legacy_file(id).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishing_twice_runs_each_waiting_load_once() {
        let id = Uuid::new_v4();
        let runs = std::rc::Rc::new(std::cell::Cell::new(0));
        let r = runs.clone();
        KEEPER.with_borrow_mut(|k| k.waiters.entry(id).or_default().push(Box::new(move || r.set(r.get() + 1))));
        finish(id);
        finish(id);
        assert_eq!(runs.get(), 1);
        assert!(KEEPER.with_borrow(|k| k.hydrated.contains(&id) && !k.waiters.contains_key(&id)));
    }

    const R: Retention = Retention { keep: true, days: 30 };

    fn cookie(name: &str, domain: &str, expires: Option<f64>) -> StoredCookie {
        StoredCookie { name: name.into(), value: "v".into(), domain: domain.into(), path: "/".into(), is_secure: true, is_http_only: true, expires, same_site: None, version: 0 }
    }

    fn jar(cookies: Vec<StoredCookie>, saved_at: f64) -> Jar {
        Jar { version: JAR_VERSION, saved_at: Some(saved_at), cookies, promoted: BTreeMap::new() }
    }

    #[test]
    fn present_cookies_win_and_expired_ones_stay_dead() {
        let now = 1_000_000.0;
        let backup = jar(
            vec![
                cookie("_iamadt", ".zoho.com", None),
                cookie("fresh", ".zoho.com", None),
                cookie("old", ".zoho.com", Some(now - 1.0)),
                cookie("ad", ".doubleclick.net", None),
                cookie("widget", ".zappsusercontent.com", None),
            ],
            now,
        );
        let present: HashSet<String> = [cookie("fresh", ".zoho.com", None).key()].into();
        let plan = restore_plan(&backup, &present, now, R);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].name, "_iamadt");
        assert_eq!(plan[0].expires, Some(now + R.seconds()), "bounded by the profile's retention, not 400 days");
    }

    #[test]
    fn a_shortened_server_expiry_is_kept_through_backup_and_restore() {
        let now = 1_000_000.0;
        // The keeper promoted the cookie earlier...
        let mut ledger = BTreeMap::new();
        ledger.insert(cookie("s", ".zoho.com", None).key(), now + R.seconds());
        // ...then the server re-set it to expire in an hour.
        let live = vec![cookie("s", ".zoho.com", Some(now + 3600.0))];
        let (promote, ledger) = promotion_plan(&live, &ledger, now, R);
        assert!(promote.is_empty(), "never stretched back out");
        assert!(ledger.is_empty(), "no longer ours to renew");
        let CaptureAction::Write(j) = capture_action(StoreState::Loaded, live.clone(), ledger, R, now) else { panic!() };
        assert_eq!(j.cookies[0].expires, Some(now + 3600.0));
        let plan = restore_plan(&j, &HashSet::new(), now + 60.0, R);
        assert_eq!(plan[0].expires, Some(now + 3600.0));
        assert!(restore_plan(&j, &HashSet::new(), now + 3601.0, R).is_empty(), "dead after its own expiry");
        assert_ne!(jar_hash(&live), jar_hash(&[cookie("s", ".zoho.com", Some(now + R.seconds()))]), "an expiry change is a change");
    }

    #[test]
    fn session_cookies_are_promoted_for_the_retention_period_and_renewed_while_used() {
        let now = 5_000_000.0;
        let live = vec![cookie("s", ".zoho.eu", None), cookie("p", ".zoho.eu", Some(now + 99.0)), cookie("t", ".tracker.io", None)];
        let (plan, ledger) = promotion_plan(&live, &BTreeMap::new(), now, R);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].expires, Some(now + R.seconds()));
        // Two weeks later nothing changes; past half the period it renews.
        let later = vec![StoredCookie { expires: Some(now + R.seconds()), ..live[0].clone() }];
        assert!(promotion_plan(&later, &ledger, now + 14.0 * DAY, R).0.is_empty());
        let (renew, _) = promotion_plan(&later, &ledger, now + 16.0 * DAY, R);
        assert_eq!(renew[0].expires, Some(now + 16.0 * DAY + R.seconds()));
    }

    #[test]
    fn explicit_logout_deletes_the_backup_and_nothing_is_restored() {
        let now = 1_000_000.0;
        assert_eq!(capture_action(StoreState::Loaded, vec![], BTreeMap::new(), R, now), CaptureAction::Delete);
        // Zoho leaves a load-balancer cookie behind: the backup now mirrors
        // that, so the sign-in cookie is never put back.
        let after = vec![cookie("zalb", ".zoho.com", None)];
        let CaptureAction::Write(j) = capture_action(StoreState::Loaded, after, BTreeMap::new(), R, now) else { panic!() };
        assert!(restore_plan(&j, &HashSet::new(), now, R).iter().all(|c| c.name != "_iamadt"));
    }

    #[test]
    fn a_store_that_has_not_loaded_never_erases_a_backup() {
        assert_eq!(capture_action(StoreState::Loading, vec![], BTreeMap::new(), R, 0.0), CaptureAction::Nothing);
        assert_eq!(capture_action(StoreState::Restored, vec![], BTreeMap::new(), R, 0.0), CaptureAction::Nothing);
        assert_eq!(capture_action(StoreState::Loading, vec![cookie("a", ".zoho.com", None)], BTreeMap::new(), R, 0.0), CaptureAction::Nothing, "partial load");
    }

    #[test]
    fn server_invalidation_replaces_the_backup() {
        let now = 1_000_000.0;
        let before = vec![cookie("_iamadt", ".zoho.com", Some(now + 1000.0))];
        let after = vec![StoredCookie { value: "new".into(), ..before[0].clone() }];
        assert_ne!(jar_hash(&before), jar_hash(&after));
        let CaptureAction::Write(j) = capture_action(StoreState::Loaded, after, BTreeMap::new(), R, now) else { panic!() };
        assert_eq!(j.cookies[0].value, "new");
    }

    #[test]
    fn clock_skew_cannot_stretch_a_session() {
        let now = 1_000_000.0;
        // Backup taken "in the future" (the clock moved back) with an
        // expiry far beyond retention.
        let skewed = jar(vec![cookie("s", ".zoho.com", Some(now + 400.0 * DAY))], now + 100.0 * DAY);
        let plan = restore_plan(&skewed, &HashSet::new(), now, R);
        assert_eq!(plan[0].expires, Some(now + R.seconds()));
        // A backup older than the retention period is not used at all.
        let stale = jar(vec![cookie("s", ".zoho.com", None)], now - 31.0 * DAY);
        assert!(restore_plan(&stale, &HashSet::new(), now, R).is_empty());
    }

    #[test]
    fn keep_signed_in_off_means_no_promotion_backup_or_restore() {
        let off = Retention { keep: false, days: 30 };
        let live = vec![cookie("s", ".zoho.com", None)];
        assert!(promotion_plan(&live, &BTreeMap::new(), 0.0, off).0.is_empty());
        assert_eq!(capture_action(StoreState::Loaded, live.clone(), BTreeMap::new(), off, 0.0), CaptureAction::Delete);
        assert!(restore_plan(&jar(live, 0.0), &HashSet::new(), 0.0, off).is_empty());
    }

    #[test]
    fn legacy_backups_decode_and_are_bounded() {
        let swift = br#"{"version":1,"savedAt":1.0,"cookies":[{"name":"_z_identity","value":"x","domain":".zoho.com","path":"/","isSecure":true,"isHTTPOnly":true,"version":0}]}"#;
        let j = decode_jar(swift).unwrap();
        assert_eq!(j.cookies[0].name, "_z_identity");
        assert!(j.cookies[0].expires.is_none());
        // 1.4.0 wrote version 2 jars with 400-day promotions and no savedAt.
        let now = 1_000_000.0;
        let v2 = format!(
            r#"{{"version":2,"cookies":[{{"name":"s","value":"x","domain":".zoho.com","path":"/","isSecure":true,"isHTTPOnly":true,"expires":{},"version":0}}]}}"#,
            now + 399.0 * DAY
        );
        let j = decode_jar(v2.as_bytes()).unwrap();
        assert_eq!(j.saved_at, None);
        assert_eq!(restore_plan(&j, &HashSet::new(), now, R)[0].expires, Some(now + R.seconds()));
        assert!(decode_jar(b"{not json").is_none(), "a damaged backup is ignored, not trusted");
    }

    #[test]
    fn the_ledger_forgets_cookies_that_are_gone() {
        let mut ledger = BTreeMap::new();
        ledger.insert("gone".to_string(), 5.0);
        let live = vec![cookie("a", ".zoho.com", Some(100.0 * DAY))];
        let CaptureAction::Write(j) = capture_action(StoreState::Loaded, live, ledger, R, 0.0) else { panic!() };
        assert!(j.promoted.is_empty());
    }
}
