//! Keeps every profile signed in until the profile is deleted.
//!
//! Each profile's `WKWebsiteDataStore` already writes cookies to disk, but
//! WebKit still signs people out in two ways:
//!
//!   1. Cookies with no expiry ("session cookies") are thrown away on quit.
//!      Zoho issues its sign-in cookies that way unless the browser is marked
//!      trusted, so a normal quit used to drop every login.
//!   2. Tracking prevention can purge or shorten a site's storage.
//!
//! So the keeper does three things for the Zoho/Google sign-in domains only:
//!
//!   * **Promote**: a session cookie found in a live store is re-saved into
//!     that same store as a persistent cookie, so WebKit itself keeps it
//!     across quits and crashes.
//!   * **Back up**: the jar is mirrored into the macOS Keychain (encrypted),
//!     but only when it actually changed.
//!   * **Restore**: before a store's first page load, any backed-up cookie the
//!     store is missing is put back, so the first request is already signed in.
//!
//! Lifetimes roll forward: every promotion or restore grants
//! [`PERMANENT_LIFETIME`] from *now*, so a profile that is used at least once
//! a year never times out on our side. Zoho can still end a session on its
//! own servers (password change, admin policy); the saved login in
//! `keychain.rs` covers that case.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSArray, NSDate, NSHTTPCookie, NSHTTPCookieDomain, NSHTTPCookieExpires, NSHTTPCookieName, NSHTTPCookiePath,
    NSHTTPCookieSameSitePolicy, NSHTTPCookieSecure, NSHTTPCookieValue, NSHTTPCookieVersion, NSMutableDictionary,
    NSRunLoop, NSString,
};
use objc2_web_kit::WKWebsiteDataStore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{keychain, policy, util};

/// How long a promoted or restored cookie lives, counted from now. 400 days
/// is the longest lifetime current cookie rules allow.
pub const PERMANENT_LIFETIME: f64 = 400.0 * 24.0 * 3600.0;

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

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Jar {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    cookies: Vec<StoredCookie>,
}

/// Which backed-up cookies to put back into a store, and with what expiry.
/// Cookies the store already has always win (a fresh login is never replaced
/// by an older backup); cookies whose own expiry has passed stay dead.
pub fn restore_plan(backup: &[StoredCookie], present: &HashSet<String>, now: f64) -> Vec<StoredCookie> {
    backup
        .iter()
        .filter(|c| !present.contains(&c.key()) && policy::is_persistable_cookie_domain(&c.domain))
        .filter_map(|c| {
            let expires = match c.expires {
                Some(e) if e <= now => return None,
                Some(e) => e,
                None => now + PERMANENT_LIFETIME,
            };
            Some(StoredCookie { expires: Some(expires), ..c.clone() })
        })
        .collect()
}

/// Session cookies in a live store that should be made persistent.
pub fn promotion_plan(live: &[StoredCookie], now: f64) -> Vec<StoredCookie> {
    live.iter()
        .filter(|c| c.expires.is_none() && policy::is_persistable_cookie_domain(&c.domain))
        .map(|c| StoredCookie { expires: Some(now + PERMANENT_LIFETIME), ..c.clone() })
        .collect()
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
        NSHTTPCookie::cookieWithProperties(&*Retained::cast_unchecked::<objc2_foundation::NSDictionary<NSString, AnyObject>>(props))
    }
}

// ---- keeper state -------------------------------------------------------------

#[derive(Default)]
struct Keeper {
    hydrated: HashSet<Uuid>,
    waiters: HashMap<Uuid, Vec<Box<dyn FnOnce()>>>,
    last_written: HashMap<Uuid, u64>,
    flushing: bool,
}

thread_local! {
    static KEEPER: RefCell<Keeper> = RefCell::new(Keeper::default());
}

fn legacy_file(id: Uuid) -> std::path::PathBuf {
    util::support_dir().join("Sessions").join(format!("{}.json", id.hyphenated().to_string().to_uppercase()))
}

/// The backed-up jar: the Keychain copy, or a plaintext file left by the Swift
/// build, which is moved into the Keychain and then deleted.
fn load_backup(id: Uuid) -> Vec<StoredCookie> {
    if let Some(data) = keychain::read_session(id) {
        if let Ok(jar) = serde_json::from_slice::<Jar>(&data) {
            return jar.cookies;
        }
    }
    let path = legacy_file(id);
    let Ok(data) = std::fs::read(&path) else { return vec![] };
    let cookies: Vec<StoredCookie> = serde_json::from_slice::<Jar>(&data)
        .map(|j| j.cookies)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| policy::is_persistable_cookie_domain(&c.domain))
        .collect();
    if !cookies.is_empty() && write_backup(id, &cookies) {
        let _ = std::fs::remove_file(&path);
    }
    cookies
}

fn jar_hash(cookies: &[StoredCookie]) -> u64 {
    let mut sorted: Vec<&StoredCookie> = cookies.iter().collect();
    sorted.sort_by_key(|c| c.key());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in sorted {
        // Expiry is left out: promotions roll it forward on every pass, and
        // rewriting the Keychain for that alone would be wasted work.
        (&c.name, &c.domain, &c.path, &c.value, c.is_secure, c.is_http_only, c.expires.is_some()).hash(&mut h);
    }
    h.finish()
}

fn write_backup(id: Uuid, cookies: &[StoredCookie]) -> bool {
    let data = serde_json::to_vec(&Jar { version: 2, cookies: cookies.to_vec() }).unwrap_or_default();
    keychain::write_session(id, &data)
}

/// Put back any backed-up cookie `store` is missing, then call `done` on the
/// main thread. Repeat calls never restore twice; callers that arrive while a
/// restore is running are queued behind it, which is what lets a tab hold its
/// first page load until its session is back.
pub fn hydrate(store: &WKWebsiteDataStore, id: Uuid, done: impl FnOnce() + 'static) {
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

    let backup = load_backup(id);
    if backup.is_empty() {
        return finish(id);
    }
    let cookie_store = unsafe { store.httpCookieStore() };
    let cs = cookie_store.clone();
    let block = RcBlock::new(move |existing: NonNull<NSArray<NSHTTPCookie>>| {
        let existing = unsafe { existing.as_ref() };
        let present: HashSet<String> = existing.iter().filter_map(|c| from_ns(&c)).map(|c| c.key()).collect();
        let plan = restore_plan(&backup, &present, now_unix());
        let cookies: Vec<Retained<NSHTTPCookie>> = plan.iter().filter_map(to_ns).collect();
        if util::is_test_build() {
            util::log(format!("hydrate {id}: backup {} cookies, store had {}, restored {}", backup.len(), present.len(), cookies.len()));
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

fn finish(id: Uuid) {
    let waiters = KEEPER.with_borrow_mut(|k| {
        k.hydrated.insert(id);
        k.waiters.remove(&id).unwrap_or_default()
    });
    for w in waiters {
        w();
    }
}

/// Promote session cookies and back the jar up. `done` runs when finished.
pub fn capture(store: &WKWebsiteDataStore, id: Uuid, done: Option<Box<dyn FnOnce()>>) {
    let cookie_store = unsafe { store.httpCookieStore() };
    let cs = cookie_store.clone();
    let done = RefCell::new(done);
    let block = RcBlock::new(move |all: NonNull<NSArray<NSHTTPCookie>>| {
        let all = unsafe { all.as_ref() };
        let live: Vec<StoredCookie> = all
            .iter()
            .filter_map(|c| from_ns(&c))
            .filter(|c| policy::is_persistable_cookie_domain(&c.domain))
            .collect();

        // An empty jar is almost always a store that has not loaded yet;
        // never let it erase a good backup.
        if !live.is_empty() {
            let now = now_unix();
            let promote = promotion_plan(&live, now);
            if util::is_test_build() {
                util::log(format!("capture {id}: {} sign-in cookies, promoted {}", live.len(), promote.len()));
            }
            if !promote.is_empty() {
                let ns: Vec<Retained<NSHTTPCookie>> = promote.iter().filter_map(to_ns).collect();
                // WebKit invokes this handler without a nil check, so it must
                // always be a real block.
                let noop = RcBlock::new(|| {});
                unsafe { cs.setCookies_completionHandler(&NSArray::from_retained_slice(&ns), Some(&noop)) };
            }
            // Record what will be in the store after promotion.
            let mut backup = live;
            for p in promote {
                if let Some(slot) = backup.iter_mut().find(|c| c.key() == p.key()) {
                    *slot = p;
                }
            }
            let hash = jar_hash(&backup);
            let changed = KEEPER.with_borrow(|k| k.last_written.get(&id) != Some(&hash));
            if changed && write_backup(id, &backup) {
                KEEPER.with_borrow_mut(|k| k.last_written.insert(id, hash));
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
pub fn capture_all(stores: Vec<(Uuid, Retained<WKWebsiteDataStore>)>, wait: bool) {
    if stores.is_empty() {
        return;
    }
    if !wait {
        for (id, s) in &stores {
            capture(s, *id, None);
        }
        return;
    }
    if KEEPER.with_borrow(|k| k.flushing) {
        return;
    }
    KEEPER.with_borrow_mut(|k| k.flushing = true);
    let remaining = std::rc::Rc::new(std::cell::Cell::new(stores.len()));
    for (id, s) in &stores {
        let r = remaining.clone();
        capture(s, *id, Some(Box::new(move || r.set(r.get() - 1))));
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

/// Forget a deleted profile's backup.
pub fn forget(id: Uuid) {
    KEEPER.with_borrow_mut(|k| {
        k.hydrated.remove(&id);
        k.waiters.remove(&id);
        k.last_written.remove(&id);
    });
    keychain::delete_session(id);
    let _ = std::fs::remove_file(legacy_file(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, domain: &str, expires: Option<f64>) -> StoredCookie {
        StoredCookie {
            name: name.into(), value: "v".into(), domain: domain.into(), path: "/".into(),
            is_secure: true, is_http_only: true, expires, same_site: None, version: 0,
        }
    }

    #[test]
    fn present_cookies_win_and_expired_ones_stay_dead() {
        let now = 1_000_000.0;
        let backup = vec![
            cookie("_iamadt", ".zoho.com", None),
            cookie("fresh", ".zoho.com", None),
            cookie("old", ".zoho.com", Some(now - 1.0)),
            cookie("ad", ".doubleclick.net", None),
        ];
        let present: HashSet<String> = [cookie("fresh", ".zoho.com", None).key()].into();
        let plan = restore_plan(&backup, &present, now);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].name, "_iamadt");
        assert_eq!(plan[0].expires, Some(now + PERMANENT_LIFETIME), "session cookie restored as long-lived");
    }

    #[test]
    fn dated_cookies_keep_their_own_expiry() {
        let now = 1_000_000.0;
        let plan = restore_plan(&[cookie("a", "crm.zoho.com", Some(now + 3600.0))], &HashSet::new(), now);
        assert_eq!(plan[0].expires, Some(now + 3600.0));
    }

    #[test]
    fn only_sign_in_session_cookies_are_promoted() {
        let now = 5.0;
        let live = vec![cookie("s", ".zoho.eu", None), cookie("p", ".zoho.eu", Some(99.0)), cookie("t", ".tracker.io", None)];
        let plan = promotion_plan(&live, now);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].name, "s");
        assert_eq!(plan[0].expires, Some(now + PERMANENT_LIFETIME));
    }

    #[test]
    fn swift_backups_still_decode() {
        let legacy = br#"{"version":1,"savedAt":1.0,"cookies":[{"name":"_z_identity","value":"x","domain":".zoho.com","path":"/","isSecure":true,"isHTTPOnly":true,"version":0}]}"#;
        let jar: Jar = serde_json::from_slice(legacy).unwrap();
        assert_eq!(jar.cookies[0].name, "_z_identity");
        assert!(jar.cookies[0].expires.is_none());
    }

    #[test]
    fn expiry_rollover_alone_does_not_count_as_a_change() {
        let a = vec![cookie("s", ".zoho.com", Some(10.0))];
        let b = vec![cookie("s", ".zoho.com", Some(20.0))];
        assert_eq!(jar_hash(&a), jar_hash(&b));
        let c = vec![StoredCookie { value: "new".into(), ..a[0].clone() }];
        assert_ne!(jar_hash(&a), jar_hash(&c));
    }
}
