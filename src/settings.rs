//! App-wide preferences (`settings.json`): developer mode, per-site download
//! permission, per-site zoom, keyboard choices. Per-profile choices (how long
//! to stay signed in) live with the profile in `clients.json`.
//!
//! Nothing secret is stored here, and no page content: origins and numbers
//! only.

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::util;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum SitePermission {
    Ask,
    Allow,
    Block,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub version: u32,
    /// Web Inspector for pages. Only honoured in test builds and debug
    /// builds; a release build of the real app never makes pages inspectable.
    pub developer_mode: bool,
    /// Control-A selects all (the Windows habit the user asked for). Off
    /// gives the standard Mac "start of line".
    pub control_a_selects_all: bool,
    /// While a web page has the keyboard, ⌘D, ⌘S, ⌘K and ⇧⌘S belong to the
    /// page (Zoho Sheet fill-down, Writer save, link insertion...). The
    /// app's own versions stay on ⌃S, ⌥⌘K, ⌥⌘S and the menus.
    pub page_owns_editor_shortcuts: bool,
    /// Origin ("https://crm.zoho.com") → what to do when it starts a
    /// download without a recent click.
    pub downloads: BTreeMap<String, SitePermission>,
    /// "<profile uuid>|<host>" → page zoom.
    pub zoom: BTreeMap<String, f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { version: 1, developer_mode: false, control_a_selects_all: true, page_owns_editor_shortcuts: true, downloads: BTreeMap::new(), zoom: BTreeMap::new() }
    }
}

thread_local! {
    static SETTINGS: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

fn path() -> std::path::PathBuf {
    util::support_dir().join("settings.json")
}

/// Unreadable settings fall back to the defaults but the file is kept
/// aside, never silently overwritten.
fn load() -> Settings {
    match std::fs::read(path()) {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
            let aside = path().with_extension(format!("unreadable-{}.json", std::process::id()));
            let _ = std::fs::rename(path(), &aside);
            util::log(format!("settings unreadable ({e}); kept aside"));
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

pub fn get<R>(f: impl FnOnce(&Settings) -> R) -> R {
    SETTINGS.with_borrow_mut(|s| f(s.get_or_insert_with(load)))
}

pub fn update(f: impl FnOnce(&mut Settings)) {
    let data = SETTINGS.with_borrow_mut(|s| {
        let s = s.get_or_insert_with(load);
        f(s);
        serde_json::to_vec_pretty(s).unwrap_or_default()
    });
    util::write_soon(path(), data, 0o600);
}

/// May pages be inspected with Safari's Web Inspector? Never in a release
/// build of the real app, whatever the settings file or environment says.
pub fn web_inspector_allowed() -> bool {
    inspector_policy(cfg!(debug_assertions), util::is_test_build(), get(|s| s.developer_mode))
}

pub fn inspector_policy(debug_build: bool, test_build: bool, developer_mode: bool) -> bool {
    debug_build || (test_build && developer_mode)
}

pub fn download_permission(origin: &str) -> SitePermission {
    get(|s| s.downloads.get(origin).copied().unwrap_or(SitePermission::Ask))
}

pub fn set_download_permission(origin: &str, p: SitePermission) {
    update(|s| {
        if p == SitePermission::Ask {
            s.downloads.remove(origin);
        } else {
            s.downloads.insert(origin.to_string(), p);
        }
    });
}

pub fn zoom_for(profile: uuid::Uuid, host: &str) -> Option<f64> {
    get(|s| s.zoom.get(&format!("{profile}|{host}")).copied())
}

pub fn set_zoom(profile: uuid::Uuid, host: &str, zoom: f64) {
    update(|s| {
        let key = format!("{profile}|{host}");
        if (zoom - 1.0).abs() < 0.01 {
            s.zoom.remove(&key);
        } else {
            s.zoom.insert(key, zoom);
        }
    });
}

/// Forget everything about a deleted profile.
pub fn forget_profile(profile: uuid::Uuid) {
    let prefix = format!("{profile}|");
    update(|s| s.zoom.retain(|k, _| !k.starts_with(&prefix)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_builds_are_never_inspectable() {
        assert!(!inspector_policy(false, false, true), "real release app ignores developer mode");
        assert!(!inspector_policy(false, false, false));
        assert!(!inspector_policy(false, true, false), "test build: off by default");
        assert!(inspector_policy(false, true, true));
        assert!(inspector_policy(true, false, false), "debug builds");
    }

    #[test]
    fn settings_decode_with_missing_and_unknown_fields() {
        let s: Settings = serde_json::from_str(r#"{"developerMode":true,"futureThing":1}"#).unwrap();
        assert!(s.developer_mode);
        assert!(s.control_a_selects_all, "missing fields take defaults");
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
    }
}
