//! Each tab's exact place: WebKit's own snapshot of the tab (back/forward
//! history, the current page, scroll position), written whenever a tab goes
//! to sleep, when the app goes to the background, on a timer, and on quit.
//!
//! The file records the URL it was taken on. It is only replayed while that
//! still matches the tab's remembered URL, so a stale snapshot can never send a
//! tab somewhere other than where the user last was.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use crate::{policy, util};

thread_local! {
    /// The latest snapshot of every tab saved this run, exactly as written.
    /// Reads come from here first, so a tab woken while its file is still
    /// being written gets the new snapshot, and an unchanged snapshot is not
    /// written again.
    static LATEST: RefCell<HashMap<Uuid, Vec<u8>>> = RefCell::new(HashMap::new());
}

fn dir() -> PathBuf {
    util::private_dir("TabState")
}

fn file(tab: Uuid) -> PathBuf {
    dir().join(format!("{}.state", tab.hyphenated().to_string().to_uppercase()))
}

pub fn save(tab: Uuid, url: &str, data: &[u8]) {
    // Never record a sign-in form as "where the user was".
    if policy::parse(url).is_some_and(|u| policy::is_auth_page(&u)) || data.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(4 + url.len() + data.len());
    out.extend_from_slice(&(url.len() as u32).to_le_bytes());
    out.extend_from_slice(url.as_bytes());
    out.extend_from_slice(data);
    if LATEST.with_borrow(|m| m.get(&tab) == Some(&out)) {
        return;
    }
    util::write_soon(file(tab), out.clone(), 0o600);
    LATEST.with_borrow_mut(|m| m.insert(tab, out));
}

/// The snapshot for `tab`, if it was taken on `expected_url`.
pub fn load(tab: Uuid, expected_url: &str) -> Option<Vec<u8>> {
    let raw = LATEST.with_borrow(|m| m.get(&tab).cloned()).or_else(|| std::fs::read(file(tab)).ok())?;
    let len = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let url = std::str::from_utf8(raw.get(4..4 + len)?).ok()?;
    let same = match (policy::parse(url), policy::parse(expected_url)) {
        (Some(a), Some(b)) => policy::page_key(&a) == policy::page_key(&b),
        _ => false,
    };
    same.then(|| raw[4 + len..].to_vec())
}

pub fn delete(tab: Uuid) {
    LATEST.with_borrow_mut(|m| m.remove(&tab));
    util::remove_soon(file(tab));
}

/// Remove snapshots for tabs that no longer exist.
pub fn prune(keep: &[Uuid]) {
    let keep: std::collections::HashSet<String> =
        keep.iter().map(|id| format!("{}.state", id.hyphenated().to_string().to_uppercase())).collect();
    if let Ok(entries) = std::fs::read_dir(dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}
