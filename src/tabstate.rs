//! Each tab's exact place: WebKit's own snapshot of the tab (back/forward
//! history, the current page, scroll position), written whenever a tab goes
//! to sleep, when the app goes to the background, on a timer, and on quit.
//!
//! The file records the URL it was taken on. It is only replayed while that
//! still matches the tab's remembered URL, so a stale snapshot can never send a
//! tab somewhere other than where the user last was.

use std::path::PathBuf;

use uuid::Uuid;

use crate::{policy, util};

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
    if let Err(e) = util::write_atomic(&file(tab), &out, 0o600) {
        util::log(format!("could not save tab state: {e}"));
    }
}

/// The snapshot for `tab`, if it was taken on `expected_url`.
pub fn load(tab: Uuid, expected_url: &str) -> Option<Vec<u8>> {
    let raw = std::fs::read(file(tab)).ok()?;
    let len = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let url = std::str::from_utf8(raw.get(4..4 + len)?).ok()?;
    let same = match (policy::parse(url), policy::parse(expected_url)) {
        (Some(a), Some(b)) => policy::page_key(&a) == policy::page_key(&b),
        _ => false,
    };
    same.then(|| raw[4 + len..].to_vec())
}

pub fn delete(tab: Uuid) {
    let _ = std::fs::remove_file(file(tab));
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
