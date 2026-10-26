//! Profiles ("clients"), their tabs, ordering and the saved-session file.
//!
//! The JSON written here is byte-compatible with the Swift build's
//! `clients.json` (upper-case UUIDs, dates as seconds since 2001-01-01), so the
//! existing profiles, tabs and sign-ins carry over and the Swift app can still
//! read the file if it is ever reinstalled.
//!
//! Methods never touch AppKit or WebKit. Anything that must happen outside the
//! model (dropping a web view, deleting a data store) is returned as an
//! [`Effect`] for the caller to carry out.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::policy;

/// Seconds between 1970-01-01 and 2001-01-01, Foundation's reference date.
const REFERENCE_DATE_OFFSET: f64 = 978_307_200.0;

pub const GOOGLE_SEARCH_ID: Uuid = Uuid::from_bytes([0x47, 0x53, 0x45, 0x41, 0x52, 0x43, 0x48, 0x21, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
pub const GOOGLE_SEARCH_HOME: &str = "https://www.google.com/";
const GOOGLE_BLUE: &str = "#4285f4";

pub fn now_reference() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) - REFERENCE_DATE_OFFSET
}

mod upper_uuid {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    pub fn serialize<S: Serializer>(id: &Uuid, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&id.hyphenated().to_string().to_uppercase())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
        let s = String::deserialize(d)?;
        Uuid::parse_str(&s).map_err(serde::de::Error::custom)
    }

    pub mod option {
        use super::*;
        pub fn serialize<S: Serializer>(id: &Option<Uuid>, s: S) -> Result<S::Ok, S::Error> {
            match id {
                Some(id) => super::serialize(id, s),
                None => s.serialize_none(),
            }
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
            let s = Option::<String>::deserialize(d)?;
            s.map(|s| Uuid::parse_str(&s).map_err(serde::de::Error::custom)).transpose()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Workspace {
    Zoho,
    GoogleSearch,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ClientKind {
    GoogleSearch,
}

fn default_title() -> String {
    "New Tab".into()
}

/// One browser tab. `url == None` is the Zoho app launcher.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Tab {
    #[serde(with = "upper_uuid")]
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(rename = "pdfPath", default, skip_serializing_if = "Option::is_none")]
    pub pdf_path: Option<String>,
    #[serde(rename = "pdfSourceURL", default, skip_serializing_if = "Option::is_none")]
    pub pdf_source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Workspace>,
    /// Search tabs: the profile the search was started from. Zoho and Meet
    /// results open in that profile, never in the Search workspace.
    #[serde(rename = "originClientId", with = "upper_uuid::option", default, skip_serializing_if = "Option::is_none")]
    pub origin_client: Option<Uuid>,
}

impl Default for Tab {
    fn default() -> Self {
        Tab { id: Uuid::new_v4(), url: None, title: default_title(), pdf_path: None, pdf_source_url: None, workspace: None, origin_client: None }
    }
}

impl Tab {
    pub fn with_url(url: impl Into<String>, title: impl Into<String>, workspace: Option<Workspace>) -> Self {
        Tab { url: Some(url.into()), title: title.into(), workspace, ..Tab::default() }
    }

    pub fn effective_workspace(&self) -> Workspace {
        self.workspace.unwrap_or(Workspace::Zoho)
    }

    pub fn parsed_url(&self) -> Option<url::Url> {
        self.url.as_deref().and_then(policy::parse)
    }

    pub fn display_title(&self) -> String {
        if self.url.is_none() {
            return "New Tab".into();
        }
        normalized_page_title(Some(&self.title)).unwrap_or_else(|| fallback_title(self.url.as_deref()))
    }

    fn is_google_search_tab(&self) -> bool {
        self.effective_workspace() == Workspace::GoogleSearch && self.parsed_url().is_some_and(|u| policy::is_google_search(&u))
    }
}

pub use crate::apps::{ZOHO_APPS, ZohoApp};

pub fn matched_zoho_app(url: Option<&str>) -> Option<&'static ZohoApp> {
    let host = url.and_then(policy::parse)?.host_str()?.to_lowercase();
    crate::apps::app_for_host(&host)
}

/// Keep page titles readable: strip control characters, collapse whitespace,
/// cap the length, and treat placeholder titles as "no title yet".
pub fn normalized_page_title(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let cleaned: String = raw.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let placeholders = ["undefined", "null", "about:blank", "loading...", "loading…", "new tab"];
    if placeholders.contains(&collapsed.to_lowercase().as_str()) {
        return None;
    }
    Some(collapsed.chars().take(256).collect())
}

pub fn fallback_title(url: Option<&str>) -> String {
    let Some(parsed) = url.and_then(policy::parse) else { return "Untitled".into() };
    if let Some(app) = matched_zoho_app(url) {
        return app.full_name();
    }
    if policy::is_zoho_widget(&parsed) {
        return "Zoho Widget".into();
    }
    if policy::is_google_meet(&parsed) {
        return "Google Meet".into();
    }
    if policy::is_google_search(&parsed) {
        return "Google Search".into();
    }
    parsed.host_str().map(str::to_lowercase).unwrap_or_else(|| "Untitled".into())
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Client {
    #[serde(with = "upper_uuid")]
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "colorHex", default = "default_color")]
    pub color_hex: String,
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(rename = "activeTabId", with = "upper_uuid::option", default, skip_serializing_if = "Option::is_none")]
    pub active_tab_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ClientKind>,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(rename = "symbolName", default, skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    /// "Keep me signed in" (absent = on, the behaviour before this setting).
    #[serde(rename = "keepSignedIn", default, skip_serializing_if = "Option::is_none")]
    pub keep_signed_in: Option<bool>,
    /// How long a kept session lasts without use, in days.
    #[serde(rename = "sessionRetentionDays", default, skip_serializing_if = "Option::is_none")]
    pub session_days: Option<u32>,
}

fn default_color() -> String {
    "#e42527".into()
}

impl Client {
    pub fn new(name: impl Into<String>, color_hex: impl Into<String>) -> Self {
        let tab = Tab::default();
        Client {
            id: Uuid::new_v4(),
            name: name.into(),
            color_hex: color_hex.into(),
            active_tab_id: Some(tab.id),
            tabs: vec![tab],
            kind: None,
            created_at: Some(now_reference()),
            symbol_name: None,
            keep_signed_in: None,
            session_days: None,
        }
    }

    pub fn retention(&self) -> crate::session::Retention {
        crate::session::Retention {
            keep: self.keep_signed_in.unwrap_or(true),
            days: self.session_days.filter(|d| (1..=400).contains(d)).unwrap_or(crate::session::DEFAULT_RETENTION_DAYS),
        }
    }

    pub fn is_search(&self) -> bool {
        self.kind == Some(ClientKind::GoogleSearch)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| Some(t.id) == self.active_tab_id).or(self.tabs.first())
    }

    pub fn active_index(&self) -> Option<usize> {
        self.tabs.iter().position(|t| Some(t.id) == self.active_tab_id)
    }

    fn effective_created_at(&self) -> f64 {
        self.created_at.unwrap_or(f64::MIN)
    }

    /// Case- and accent-insensitive key so "acme" and "Ácme" sort together.
    pub fn sort_key(&self) -> String {
        fold(&self.name)
    }

    pub fn initials(&self) -> String {
        initials(&self.name)
    }

    /// Drag semantics of a browser tab strip: moving right lands after the
    /// target, moving left lands before it.
    pub fn move_tab_over(&mut self, tab: Uuid, target: Uuid) -> bool {
        if tab == target {
            return false;
        }
        let (Some(src), Some(dst)) = (self.tabs.iter().position(|t| t.id == tab), self.tabs.iter().position(|t| t.id == target)) else {
            return false;
        };
        let moved = self.tabs.remove(src);
        self.tabs.insert(dst, moved);
        true
    }

    pub fn move_tab_by(&mut self, tab: Uuid, offset: isize) -> bool {
        let Some(src) = self.tabs.iter().position(|t| t.id == tab) else { return false };
        let dst = (src as isize + offset).clamp(0, self.tabs.len() as isize - 1) as usize;
        if dst == src {
            return false;
        }
        let moved = self.tabs.remove(src);
        self.tabs.insert(dst, moved);
        true
    }
}

pub fn initials(name: &str) -> String {
    let s: String = name.split_whitespace().take(2).filter_map(|w| w.chars().next()).collect();
    if s.is_empty() { "?".into() } else { s.to_uppercase() }
}

fn fold(s: &str) -> String {
    // A small diacritic fold covering Latin accents; enough for sorting names.
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
            'ç' | 'Ç' => 'c',
            'ñ' | 'Ñ' => 'n',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum SortMode {
    #[default]
    Manual,
    NameAscending,
    NameDescending,
    NewestFirst,
    OldestFirst,
}

impl SortMode {
    pub const ALL: [SortMode; 5] = [SortMode::Manual, SortMode::NameAscending, SortMode::NameDescending, SortMode::NewestFirst, SortMode::OldestFirst];

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Manual => "Custom Order",
            SortMode::NameAscending => "Name (A → Z)",
            SortMode::NameDescending => "Name (Z → A)",
            SortMode::NewestFirst => "Date Added (Newest First)",
            SortMode::OldestFirst => "Date Added (Oldest First)",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    #[serde(rename = "activeClientId", with = "upper_uuid::option", default, skip_serializing_if = "Option::is_none")]
    active_client_id: Option<Uuid>,
    clients: Vec<Client>,
    #[serde(rename = "sortMode", default, skip_serializing_if = "Option::is_none")]
    sort_mode: Option<SortMode>,
}

pub struct ClosedTab {
    client_id: Uuid,
    index: usize,
    tab: Tab,
}

/// Work the model asks its caller to do outside the model.
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// Release this tab's web view and forget its saved page state.
    DropTab(Uuid),
    /// Release the web view but keep the saved page state (the tab moved on
    /// to a new page and the state no longer applies, or it is re-created).
    ResetTab(Uuid),
    /// Delete every trace of a profile: web views, data stores, cookie
    /// backup, saved login and page state.
    DeleteClient {
        client_id: Uuid,
        tab_ids: Vec<Uuid>,
    },
    RemoveFile(String),
}

#[derive(Default)]
pub struct Store {
    pub clients: Vec<Client>,
    pub active_client_id: Option<Uuid>,
    pub sort_mode: SortMode,
    closed: Vec<ClosedTab>,
}

/// The `version` this build writes. A larger one is a newer ZClients's
/// file: it is never rewritten by this build.
pub const SCHEMA_VERSION: u32 = 4;

/// Why the profile file could not be used. Each one leaves the file exactly
/// as it is.
#[derive(Debug, PartialEq)]
pub enum LoadProblem {
    /// It exists but could not be read (permissions, I/O error).
    Unreadable(String),
    /// It was read but is not a profile list.
    Corrupt(String),
    /// Written by a newer ZClients.
    FutureVersion(u32),
}

impl std::fmt::Display for LoadProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadProblem::Unreadable(e) => write!(f, "it could not be read ({e})"),
            LoadProblem::Corrupt(e) => write!(f, "it is damaged ({e})"),
            LoadProblem::FutureVersion(v) => write!(f, "it was saved by a newer version of ZClients (format {v})"),
        }
    }
}

/// Turn the result of reading `clients.json` into a store. `Ok(None)`: there
/// is no file (first launch). Anything else that fails is a problem the
/// caller must surface; it never becomes an empty profile list on its own.
pub fn classify_read(read: std::io::Result<Vec<u8>>) -> Result<Option<Store>, LoadProblem> {
    match read {
        Ok(data) => decode_store(&data).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LoadProblem::Unreadable(e.to_string())),
    }
}

pub fn decode_store(data: &[u8]) -> Result<Store, LoadProblem> {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(data)
        && let Some(version) = v.get("version").and_then(|x| x.as_u64())
        && version > SCHEMA_VERSION as u64
    {
        return Err(LoadProblem::FutureVersion(version as u32));
    }
    Store::from_json(data).map_err(|e| LoadProblem::Corrupt(e.to_string()))
}

impl Store {
    /// Decode a saved session. Accepts the current snapshot format and the
    /// bare client array older builds wrote.
    pub fn from_json(data: &[u8]) -> Result<Store, serde_json::Error> {
        let mut store = Store::default();
        match serde_json::from_slice::<Snapshot>(data) {
            Ok(s) => {
                store.clients = s.clients;
                store.sort_mode = s.sort_mode.unwrap_or_default();
                store.active_client_id = s.active_client_id;
            }
            // A snapshot (an object) that fails to decode is reported as
            // such, not as "not an array".
            Err(e) if data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') => return Err(e),
            Err(_) => {
                store.clients = serde_json::from_slice::<Vec<Client>>(data)?;
            }
        }
        store.repair();
        Ok(store)
    }

    pub fn to_json(&self) -> Vec<u8> {
        let snapshot = Snapshot { version: SCHEMA_VERSION, active_client_id: self.active_client_id, clients: self.clients.clone(), sort_mode: Some(self.sort_mode) };
        serde_json::to_vec(&snapshot).unwrap_or_default()
    }

    /// Fix up anything a crash or an older build could have left behind.
    pub fn repair(&mut self) {
        self.repair_duplicate_ids();
        // Clients saved before ordering existed get dates derived from their
        // position, so "oldest first" reproduces the order the user had.
        let mut offset = 0.0;
        for c in self.clients.iter_mut().filter(|c| c.created_at.is_none()) {
            c.created_at = Some(-REFERENCE_DATE_OFFSET + offset);
            offset += 60.0;
        }
        for c in &mut self.clients {
            if c.tabs.is_empty() {
                c.tabs.push(if c.is_search() { search_tab(GOOGLE_SEARCH_HOME) } else { Tab::default() });
            }
            for t in &mut c.tabs {
                // Older builds saved the sign-in form as "where the user
                // was". Zoho's sign-in URL carries the real page in
                // `serviceurl`; put the tab back there.
                if let Some(real) = t.parsed_url().and_then(|u| policy::sign_in_destination(&u)) {
                    t.title = fallback_title(Some(real.as_str()));
                    t.url = Some(real.to_string());
                }
                if t.pdf_path.as_deref().is_some_and(|p| !std::path::Path::new(p).exists()) {
                    t.pdf_path = None;
                    t.pdf_source_url = None;
                }
            }
            if !c.tabs.iter().any(|t| Some(t.id) == c.active_tab_id) {
                c.active_tab_id = c.tabs.first().map(|t| t.id);
            }
        }
        self.ensure_search_client();
        if !self.active_client_id.is_some_and(|id| self.index(id).is_some()) {
            self.active_client_id = self.clients.last().map(|c| c.id);
        }
    }

    /// Two profiles with one id would share one WebKit data store (the id
    /// *is* the store), and two tabs with one id share saved page state. The
    /// first keeps its id; a later duplicate gets a fresh one, so nothing is
    /// ever merged: the duplicate profile starts signed out in a new, empty
    /// store and is marked "(recovered)". Returns how many ids changed.
    pub fn repair_duplicate_ids(&mut self) -> usize {
        let mut fixed = 0;
        let mut clients = std::collections::HashSet::new();
        let mut tabs = std::collections::HashSet::new();
        for c in &mut self.clients {
            if !clients.insert(c.id) && !c.is_search() {
                c.id = Uuid::new_v4();
                clients.insert(c.id);
                c.name = format!("{} (recovered)", c.name);
                fixed += 1;
            }
            for t in &mut c.tabs {
                if !tabs.insert(t.id) {
                    let was_active = c.active_tab_id == Some(t.id);
                    t.id = Uuid::new_v4();
                    tabs.insert(t.id);
                    if was_active {
                        c.active_tab_id = Some(t.id);
                    }
                    fixed += 1;
                }
            }
        }
        // Two Search clients: keep the first; ensure_search_client drops
        // the rest's duplicate pages.
        let mut seen_search = false;
        self.clients.retain(|c| !(c.is_search() || c.id == GOOGLE_SEARCH_ID) || !std::mem::replace(&mut seen_search, true));
        fixed
    }

    /// The permanent Google Search workspace: always present, always first,
    /// always the system id, and never a duplicate of a Zoho page.
    pub fn ensure_search_client(&mut self) {
        let existing = self.clients.iter().position(|c| c.id == GOOGLE_SEARCH_ID || c.is_search());
        let Some(i) = existing else {
            let tab = search_tab(GOOGLE_SEARCH_HOME);
            self.clients.insert(
                0,
                Client {
                    id: GOOGLE_SEARCH_ID,
                    name: "Google Search".into(),
                    color_hex: GOOGLE_BLUE.into(),
                    active_tab_id: Some(tab.id),
                    tabs: vec![tab],
                    kind: Some(ClientKind::GoogleSearch),
                    created_at: Some(-REFERENCE_DATE_OFFSET),
                    symbol_name: None,
                    keep_signed_in: None,
                    session_days: None,
                },
            );
            if self.active_client_id.is_none() {
                self.active_client_id = Some(GOOGLE_SEARCH_ID);
            }
            return;
        };
        let mut c = self.clients.remove(i);
        c.id = GOOGLE_SEARCH_ID;
        c.kind = Some(ClientKind::GoogleSearch);
        c.name = "Google Search".into();
        c.color_hex = GOOGLE_BLUE.into();

        let mut seen = std::collections::HashSet::new();
        let mut repaired = Vec::new();
        for mut t in c.tabs.drain(..) {
            let Some(u) = t.parsed_url() else { continue };
            if policy::is_google_search(&u) {
                t.workspace = Some(Workspace::GoogleSearch);
                t.title = normalized_page_title(Some(&t.title)).unwrap_or_else(|| "Google Search".into());
            } else if policy::should_open_in_client_workspace(&u) {
                t.workspace = Some(Workspace::Zoho);
            }
            let key = format!("{:?}|{}|{}", t.effective_workspace(), policy::page_key(&u), t.pdf_path.clone().unwrap_or_default());
            if seen.insert(key) {
                repaired.push(t);
            }
        }
        if !repaired.iter().any(Tab::is_google_search_tab) {
            repaired.insert(0, search_tab(GOOGLE_SEARCH_HOME));
        }
        if !repaired.iter().any(|t| Some(t.id) == c.active_tab_id) {
            c.active_tab_id = repaired.iter().find(|t| t.is_google_search_tab()).or(repaired.first()).map(|t| t.id);
        }
        c.tabs = repaired;
        self.clients.insert(0, c);
    }

    pub fn index(&self, id: Uuid) -> Option<usize> {
        self.clients.iter().position(|c| c.id == id)
    }

    pub fn client(&self, id: Uuid) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }

    fn client_mut(&mut self, id: Uuid) -> Option<&mut Client> {
        self.clients.iter_mut().find(|c| c.id == id)
    }

    pub fn active_client(&self) -> Option<&Client> {
        self.active_client_id.and_then(|id| self.client(id))
    }

    pub fn active_tab(&self) -> Option<(&Client, &Tab)> {
        let c = self.active_client()?;
        Some((c, c.active_tab()?))
    }

    pub fn find_tab(&self, tab_id: Uuid) -> Option<&Tab> {
        self.clients.iter().flat_map(|c| c.tabs.iter()).find(|t| t.id == tab_id)
    }

    fn find_tab_mut(&mut self, tab_id: Uuid) -> Option<&mut Tab> {
        self.clients.iter_mut().find_map(|c| c.tabs.iter_mut().find(|t| t.id == tab_id))
    }

    // ---- ordering ----------------------------------------------------------

    /// The clients in the order the sidebar draws them: the Search row pinned
    /// first, the rest in the chosen sort.
    pub fn ordered(&self) -> Vec<&Client> {
        let mut pinned: Vec<&Client> = self.clients.iter().filter(|c| c.is_search()).collect();
        let mut rest: Vec<&Client> = self.clients.iter().filter(|c| !c.is_search()).collect();
        sort_clients(&mut rest, self.sort_mode);
        pinned.append(&mut rest);
        pinned
    }

    fn freeze_order(&mut self) {
        let order: Vec<Uuid> = self.ordered().iter().map(|c| c.id).collect();
        self.clients.sort_by_key(|c| order.iter().position(|id| *id == c.id).unwrap_or(usize::MAX));
    }

    pub fn set_sort_mode(&mut self, mode: SortMode) {
        if self.sort_mode == mode {
            return;
        }
        // Switching to the custom order keeps what is on screen.
        if mode == SortMode::Manual {
            self.freeze_order();
        }
        self.sort_mode = mode;
    }

    /// Drop `id` onto `target` as the user sees the list. An active system
    /// sort is first frozen into a custom order, so a drag always wins.
    pub fn move_client_over(&mut self, id: Uuid, target: Uuid) -> bool {
        if id == target {
            return false;
        }
        let pinned = |c: Option<&Client>| c.is_none_or(Client::is_search);
        if pinned(self.client(id)) || pinned(self.client(target)) {
            return false;
        }
        if self.sort_mode != SortMode::Manual {
            self.freeze_order();
            self.sort_mode = SortMode::Manual;
        }
        let (Some(src), Some(dst)) = (self.index(id), self.index(target)) else { return false };
        let c = self.clients.remove(src);
        self.clients.insert(dst, c);
        true
    }

    pub fn move_client_by(&mut self, id: Uuid, offset: isize) -> bool {
        let movable: Vec<Uuid> = self.ordered().iter().filter(|c| !c.is_search()).map(|c| c.id).collect();
        let Some(pos) = movable.iter().position(|c| *c == id) else { return false };
        let dst = pos as isize + offset;
        if dst < 0 || dst as usize >= movable.len() || offset == 0 {
            return false;
        }
        self.move_client_over(id, movable[dst as usize])
    }

    pub fn can_move_client(&self, id: Uuid, offset: isize) -> bool {
        let movable: Vec<Uuid> = self.ordered().iter().filter(|c| !c.is_search()).map(|c| c.id).collect();
        movable.iter().position(|c| *c == id).is_some_and(|p| {
            let d = p as isize + offset;
            d >= 0 && (d as usize) < movable.len()
        })
    }

    // ---- clients -----------------------------------------------------------

    pub fn add_client(&mut self, name: &str, color: &str, symbol: Option<String>) -> Uuid {
        let mut c = Client::new(name.trim(), normalized_hex(color).unwrap_or_else(default_color));
        c.symbol_name = symbol;
        let id = c.id;
        self.clients.push(c);
        self.active_client_id = Some(id);
        id
    }

    pub fn delete_client(&mut self, id: Uuid) -> Vec<Effect> {
        let Some(i) = self.index(id) else { return vec![] };
        if self.clients[i].is_search() {
            return vec![];
        }
        let c = self.clients.remove(i);
        let mut effects: Vec<Effect> = c.tabs.iter().filter_map(|t| t.pdf_path.clone()).map(Effect::RemoveFile).collect();
        for closed in self.closed.iter().filter(|e| e.client_id == id) {
            if let Some(p) = &closed.tab.pdf_path {
                effects.push(Effect::RemoveFile(p.clone()));
            }
        }
        let mut tab_ids: Vec<Uuid> = c.tabs.iter().map(|t| t.id).collect();
        tab_ids.extend(self.closed.iter().filter(|e| e.client_id == id).map(|e| e.tab.id));
        self.closed.retain(|e| e.client_id != id);
        effects.push(Effect::DeleteClient { client_id: id, tab_ids });
        if self.active_client_id == Some(id) {
            self.active_client_id = self.clients.last().map(|c| c.id);
        }
        effects
    }

    /// Set a profile's "Keep me signed in". True when it changed.
    pub fn set_retention(&mut self, id: Uuid, r: crate::session::Retention) -> bool {
        let Some(c) = self.client_mut(id) else { return false };
        let before = c.retention();
        c.keep_signed_in = Some(r.keep);
        c.session_days = Some(r.days);
        before != c.retention()
    }

    /// Every PDF preview a tab refers to, open or reopenable.
    pub fn preview_paths(&self) -> Vec<String> {
        let open = self.clients.iter().flat_map(|c| c.tabs.iter());
        let closed = self.closed.iter().map(|e| &e.tab);
        open.chain(closed).filter_map(|t| t.pdf_path.clone()).collect()
    }

    pub fn set_pdf_path(&mut self, tab: Uuid, path: Option<String>) -> bool {
        let Some(t) = self.find_tab_mut(tab) else { return false };
        if path.is_none() {
            t.pdf_source_url.get_or_insert_with(|| t.url.clone().unwrap_or_default());
        }
        t.pdf_path = path;
        true
    }

    /// Drop the reopen-closed-tab list (on quit, or when clearing previews).
    pub fn forget_closed_tabs(&mut self) {
        self.closed.clear();
    }

    pub fn select_client(&mut self, id: Uuid) -> bool {
        if self.index(id).is_none() || self.active_client_id == Some(id) {
            return false;
        }
        self.active_client_id = Some(id);
        true
    }

    /// Apply the customise sheet in one go.
    pub fn update_client(&mut self, id: Uuid, name: &str, color: &str, symbol: Option<String>) -> bool {
        let Some(c) = self.client_mut(id) else { return false };
        if c.is_search() {
            return false;
        }
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            c.name = trimmed.to_string();
        }
        if let Some(hex) = normalized_hex(color) {
            c.color_hex = hex;
        }
        c.symbol_name = symbol;
        true
    }

    // ---- tabs --------------------------------------------------------------

    pub fn add_tab(&mut self, client_id: Uuid, url: Option<String>, title: Option<String>, workspace: Option<Workspace>, activate: bool) -> Option<Uuid> {
        let c = self.client_mut(client_id)?;
        let search = c.is_search();
        let url = url.or_else(|| search.then(|| GOOGLE_SEARCH_HOME.to_string()));
        let title = title.unwrap_or_else(|| if search { "Google Search".into() } else { default_title() });
        let workspace = workspace.or(search.then_some(Workspace::GoogleSearch));
        let tab = Tab { url, title, workspace, ..Tab::default() };
        let id = tab.id;
        match c.active_index() {
            Some(i) => c.tabs.insert(i + 1, tab),
            None => c.tabs.push(tab),
        }
        if activate {
            c.active_tab_id = Some(id);
        }
        Some(id)
    }

    /// A background tab (⌘-click) placed right after `after`, so links opened
    /// one after another from a page line up in the order they were clicked.
    pub fn add_tab_after(&mut self, client_id: Uuid, after: Uuid, url: String, title: String, workspace: Option<Workspace>) -> Option<Uuid> {
        let c = self.client_mut(client_id)?;
        let i = c.tabs.iter().position(|t| t.id == after)?;
        let tab = Tab { url: Some(url), title, workspace, ..Tab::default() };
        let id = tab.id;
        c.tabs.insert(i + 1, tab);
        Some(id)
    }

    pub fn can_close_tab(&self, client_id: Uuid, tab_id: Uuid) -> bool {
        let Some(c) = self.client(client_id) else { return false };
        let Some(t) = c.tabs.iter().find(|t| t.id == tab_id) else { return false };
        if !(c.is_search() && t.is_google_search_tab()) {
            return true;
        }
        c.tabs.iter().filter(|t| t.is_google_search_tab()).count() > 1
    }

    pub fn close_tab(&mut self, client_id: Uuid, tab_id: Uuid) -> Vec<Effect> {
        if !self.can_close_tab(client_id, tab_id) {
            // The Search workspace keeps its last search tab.
            if let Some(c) = self.client_mut(client_id) {
                c.active_tab_id = Some(tab_id);
            }
            return vec![];
        }
        let Some(c) = self.client_mut(client_id) else { return vec![] };
        let Some(i) = c.tabs.iter().position(|t| t.id == tab_id) else { return vec![] };
        let tab = c.tabs.remove(i);
        if c.tabs.is_empty() {
            let t = if c.is_search() { search_tab(GOOGLE_SEARCH_HOME) } else { Tab::default() };
            c.active_tab_id = Some(t.id);
            c.tabs.push(t);
        } else if c.active_tab_id == Some(tab_id) {
            c.active_tab_id = Some(c.tabs[i.min(c.tabs.len() - 1)].id);
        }
        // The web view goes now; the saved page state stays with the closed
        // tab so ⌘⇧T brings back the same place.
        let mut effects = vec![Effect::ResetTab(tab_id)];
        self.closed.push(ClosedTab { client_id, index: i, tab });
        if self.closed.len() > 25 {
            let evicted = self.closed.remove(0);
            if let Some(p) = evicted.tab.pdf_path {
                effects.push(Effect::RemoveFile(p));
            }
            effects.push(Effect::DropTab(evicted.tab.id));
        }
        effects
    }

    /// Close a tab without remembering it for ⌘⇧T: a tab a page opened only
    /// to start a download has nothing worth bringing back, and reopening it
    /// would download the file again.
    pub fn discard_tab(&mut self, client_id: Uuid, tab_id: Uuid) -> Vec<Effect> {
        let mut effects = self.close_tab(client_id, tab_id);
        if let Some(entry) = self.closed.pop_if(|e| e.tab.id == tab_id) {
            if let Some(p) = entry.tab.pdf_path {
                effects.push(Effect::RemoveFile(p));
            }
            effects.push(Effect::DropTab(tab_id));
        }
        effects
    }

    /// Show a PDF a page opened as its own tab of `client_id`, right after
    /// the current tab. `source_url` is where it came from, kept so the tab
    /// still means something if the saved file is ever gone.
    pub fn add_pdf_tab(&mut self, client_id: Uuid, pdf_path: &str, title: &str, source_url: Option<String>) -> Option<Uuid> {
        let c = self.client_mut(client_id)?;
        let file_url = url::Url::from_file_path(pdf_path).ok().map(|u| u.to_string());
        let tab = Tab {
            url: source_url.clone().or(file_url),
            title: normalized_page_title(Some(title)).unwrap_or_else(|| "PDF".into()),
            pdf_path: Some(pdf_path.to_string()),
            pdf_source_url: source_url,
            workspace: c.is_search().then_some(Workspace::GoogleSearch),
            ..Tab::default()
        };
        let id = tab.id;
        match c.active_index() {
            Some(i) => c.tabs.insert(i + 1, tab),
            None => c.tabs.push(tab),
        }
        c.active_tab_id = Some(id);
        self.active_client_id = Some(client_id);
        Some(id)
    }

    pub fn reopen_closed_tab(&mut self) -> bool {
        while let Some(entry) = self.closed.pop() {
            let Some(c) = self.client_mut(entry.client_id) else { continue };
            let at = entry.index.min(c.tabs.len());
            c.active_tab_id = Some(entry.tab.id);
            c.tabs.insert(at, entry.tab);
            self.active_client_id = Some(entry.client_id);
            return true;
        }
        false
    }

    pub fn select_tab(&mut self, client_id: Uuid, tab_id: Uuid) -> bool {
        let Some(c) = self.client_mut(client_id) else { return false };
        if !c.tabs.iter().any(|t| t.id == tab_id) || c.active_tab_id == Some(tab_id) {
            return false;
        }
        c.active_tab_id = Some(tab_id);
        true
    }

    pub fn move_tab_over(&mut self, client_id: Uuid, tab: Uuid, target: Uuid) -> bool {
        self.client_mut(client_id).is_some_and(|c| c.move_tab_over(tab, target))
    }

    pub fn move_tab_by(&mut self, client_id: Uuid, tab: Uuid, offset: isize) -> bool {
        self.client_mut(client_id).is_some_and(|c| c.move_tab_by(tab, offset))
    }

    /// Point a tab at a new page, discarding its old web view and page state.
    pub fn navigate_tab(&mut self, tab_id: Uuid, url: Option<String>, workspace: Option<Workspace>) -> Vec<Effect> {
        let Some(t) = self.find_tab_mut(tab_id) else { return vec![] };
        let mut effects = vec![Effect::DropTab(tab_id)];
        if let Some(p) = t.pdf_path.take() {
            effects.push(Effect::RemoveFile(p));
        }
        t.pdf_source_url = None;
        t.title = if url.is_some() { fallback_title(url.as_deref()) } else { default_title() };
        t.url = url;
        t.workspace = workspace;
        effects
    }

    pub fn open_app(&mut self, tab_id: Uuid, app: &ZohoApp) -> Vec<Effect> {
        self.navigate_tab(tab_id, Some(app.url.to_string()), None)
    }

    pub fn go_home(&mut self) -> Vec<Effect> {
        let Some((c, t)) = self.active_tab() else { return vec![] };
        let (search, tab_id) = (c.is_search(), t.id);
        if search { self.navigate_tab(tab_id, Some(GOOGLE_SEARCH_HOME.into()), Some(Workspace::GoogleSearch)) } else { self.navigate_tab(tab_id, None, None) }
    }

    /// Open a Zoho/Meet page that came from Search as a normal tab of the
    /// client, reusing a tab already showing it.
    pub fn open_in_client_workspace(&mut self, client_id: Uuid, url: &url::Url, title: Option<String>) {
        let key = policy::page_key(url);
        if let Some(c) = self.client_mut(client_id)
            && let Some(existing) = c.tabs.iter().find(|t| t.effective_workspace() == Workspace::Zoho && t.parsed_url().is_some_and(|u| policy::page_key(&u) == key))
        {
            c.active_tab_id = Some(existing.id);
            self.active_client_id = Some(client_id);
            return;
        }
        let title = title.unwrap_or_else(|| fallback_title(Some(url.as_str())));
        self.add_tab(client_id, Some(url.to_string()), Some(title), Some(Workspace::Zoho), true);
    }

    /// Meet in the active client's own store, reusing an open Meet tab.
    /// False when the active client is the Search workspace: Meet needs a
    /// real profile, and the caller asks which.
    pub fn open_google_meet(&mut self) -> bool {
        let Some(id) = self.active_client_id else { return false };
        self.open_google_meet_in(id)
    }

    pub fn open_google_meet_in(&mut self, id: Uuid) -> bool {
        let Some(c) = self.client_mut(id) else { return false };
        if c.is_search() {
            return false;
        }
        self.active_client_id = Some(id);
        let Some(c) = self.client_mut(id) else { return false };
        if let Some(t) = c.tabs.iter().find(|t| t.pdf_path.is_none() && t.parsed_url().is_some_and(|u| policy::is_google_meet(&u))) {
            c.active_tab_id = Some(t.id);
            return true;
        }
        self.add_tab(id, Some("https://meet.google.com/".into()), Some("Google Meet".into()), None, true);
        true
    }

    /// The profile a Search tab was started from, if it still exists.
    pub fn search_origin(&self, tab_id: Uuid) -> Option<Uuid> {
        let origin = self.find_tab(tab_id)?.origin_client?;
        self.client(origin).filter(|c| !c.is_search()).map(|c| c.id)
    }

    /// Open Google Search. `origin` is the profile it was started from
    /// (None when started from the Search workspace itself, which keeps the
    /// tab's existing origin).
    pub fn open_google_search(&mut self, destination: &url::Url, origin: Option<Uuid>) -> Vec<Effect> {
        let origin = origin.filter(|o| *o != GOOGLE_SEARCH_ID);
        self.ensure_search_client();
        self.active_client_id = Some(GOOGLE_SEARCH_ID);
        let target = self.client(GOOGLE_SEARCH_ID).and_then(|c| c.tabs.iter().find(|t| t.pdf_path.is_none() && t.is_google_search_tab()).map(|t| t.id));
        match target {
            Some(tab_id) => {
                let effects = self.navigate_tab(tab_id, Some(destination.to_string()), Some(Workspace::GoogleSearch));
                if let Some(c) = self.client_mut(GOOGLE_SEARCH_ID) {
                    c.active_tab_id = Some(tab_id);
                    if let Some(t) = c.tabs.iter_mut().find(|t| t.id == tab_id) {
                        t.title = "Google Search".into();
                        if origin.is_some() {
                            t.origin_client = origin;
                        }
                    }
                }
                effects
            }
            None => {
                let id = self.add_tab(GOOGLE_SEARCH_ID, Some(destination.to_string()), Some("Google Search".into()), Some(Workspace::GoogleSearch), true);
                if let Some(t) = id.and_then(|id| self.find_tab_mut(id)) {
                    t.origin_client = origin;
                }
                vec![]
            }
        }
    }

    pub fn duplicate_active_tab(&mut self, live_url: Option<String>) -> Option<Uuid> {
        let (c, t) = self.active_tab()?;
        if t.url.is_none() {
            return self.add_tab(c.id, None, None, None, true);
        }
        let (cid, url, title, ws) = (c.id, live_url.or(t.url.clone()), t.title.clone(), t.workspace);
        self.add_tab(cid, url, Some(title), ws, true)
    }

    /// A live page reported a new URL and/or title. Sign-in pages never
    /// replace the remembered URL: after signing in Zoho returns the user to
    /// the page they were on, and if the app quits mid-login the tab still
    /// reopens on that page rather than on the login form.
    pub fn update_tab_meta(&mut self, tab_id: Uuid, url: Option<&str>, title: Option<&str>) -> bool {
        let Some(t) = self.find_tab_mut(tab_id) else { return false };
        if t.pdf_path.is_some() {
            return false;
        }
        let mut changed = false;
        let mut on_auth_page = false;
        if let Some(u) = url.filter(|u| !u.is_empty() && *u != "about:blank") {
            let parsed = policy::parse(u);
            on_auth_page = parsed.as_ref().is_some_and(policy::is_auth_page);
            if !on_auth_page && t.url.as_deref() != Some(u) {
                t.url = Some(u.to_string());
                changed = true;
            }
        }
        if !on_auth_page
            && let Some(title) = normalized_page_title(title)
            && t.title != title
        {
            t.title = title;
            changed = true;
        }
        changed
    }

    pub fn select_tab_index(&mut self, i: usize) -> bool {
        let Some(id) = self.active_client_id else { return false };
        let Some(c) = self.client_mut(id) else { return false };
        let Some(t) = c.tabs.get(i) else { return false };
        if c.active_tab_id == Some(t.id) {
            return false;
        }
        c.active_tab_id = Some(t.id);
        true
    }

    pub fn select_last_tab(&mut self) -> bool {
        let n = self.active_client().map(|c| c.tabs.len()).unwrap_or(0);
        n > 0 && self.select_tab_index(n - 1)
    }

    pub fn cycle_tab(&mut self, dir: isize) -> bool {
        let Some(c) = self.active_client() else { return false };
        let n = c.tabs.len() as isize;
        let Some(cur) = c.active_index() else { return false };
        let next = ((cur as isize + dir) % n + n) % n;
        self.select_tab_index(next as usize)
    }
}

fn search_tab(url: &str) -> Tab {
    Tab::with_url(url, "Google Search", Some(Workspace::GoogleSearch))
}

pub fn sort_clients(clients: &mut [&Client], mode: SortMode) {
    use std::cmp::Ordering;
    let by_date = |a: &Client, b: &Client| a.effective_created_at().partial_cmp(&b.effective_created_at()).unwrap_or(Ordering::Equal);
    match mode {
        SortMode::Manual => {}
        SortMode::NameAscending => clients.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()).then_with(|| by_date(a, b))),
        SortMode::NameDescending => clients.sort_by(|a, b| b.sort_key().cmp(&a.sort_key()).then_with(|| by_date(a, b))),
        SortMode::NewestFirst => clients.sort_by(|a, b| by_date(b, a).then_with(|| a.sort_key().cmp(&b.sort_key()))),
        SortMode::OldestFirst => clients.sort_by(|a, b| by_date(a, b).then_with(|| a.sort_key().cmp(&b.sort_key()))),
    }
}

/// Accepts "#rgb", "rgb", "#rrggbb" and "rrggbb"; returns "#rrggbb".
pub fn normalized_hex(hex: &str) -> Option<String> {
    let raw = hex.trim().trim_start_matches('#').to_lowercase();
    if !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match raw.len() {
        3 => Some(format!("#{}", raw.chars().flat_map(|c| [c, c]).collect::<String>())),
        6 => Some(format!("#{raw}")),
        _ => None,
    }
}

pub fn hex_to_rgb(hex: &str) -> (f64, f64, f64) {
    let h = normalized_hex(hex).unwrap_or_else(|| "#7a7f8c".into());
    let v = u32::from_str_radix(&h[1..], 16).unwrap_or(0x7a7f8c);
    (((v >> 16) & 0xff) as f64 / 255.0, ((v >> 8) & 0xff) as f64 / 255.0, (v & 0xff) as f64 / 255.0)
}

pub const PROFILE_COLORS: &[(&str, &str)] = &[
    ("Red", "#e42527"),
    ("Orange", "#ea580c"),
    ("Amber", "#f2a516"),
    ("Green", "#22a447"),
    ("Lime", "#65a30d"),
    ("Teal", "#0ea5a4"),
    ("Cyan", "#0891b2"),
    // One step darker than 1.5 (#2b7de9), which no text colour could read
    // on at 4.5:1. Clients that already use the old blue keep it.
    ("Blue", "#2470dd"),
    ("Indigo", "#4f46e5"),
    ("Violet", "#7a5cf0"),
    ("Purple", "#9333ea"),
    ("Pink", "#db2777"),
    ("Rose", "#be123c"),
    ("Brown", "#8b5e34"),
    ("Slate", "#475569"),
    ("Graphite", "#1e293b"),
];

#[cfg(test)]
mod tests {
    use super::*;

    const SWIFT_SAMPLE: &str = r##"{"version":4,"activeClientId":"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11","sortMode":"manual","clients":[
      {"id":"47534541-5243-4821-8000-000000000001","name":"Google Search","colorHex":"#4285f4","kind":"googleSearch","createdAt":-978307200,
       "activeTabId":"8FE3D8AB-1190-4E0C-A1C1-111111111111","tabs":[{"id":"8FE3D8AB-1190-4E0C-A1C1-111111111111","url":"https://www.google.com/","title":"Google Search","workspace":"googleSearch"}]},
      {"id":"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11","name":"Acme","colorHex":"#22a447","createdAt":800000000.5,"symbolName":"hammer.fill",
       "activeTabId":"11111111-2222-3333-4444-555555555555","tabs":[{"id":"11111111-2222-3333-4444-555555555555","url":"https://crm.zoho.com/crm/org1/tab/Leads","title":"Leads"},{"id":"21111111-2222-3333-4444-555555555555","title":"New Tab"}]}]}"##;

    #[test]
    fn reads_and_round_trips_the_swift_session_file() {
        let store = Store::from_json(SWIFT_SAMPLE.as_bytes()).unwrap();
        assert_eq!(store.clients.len(), 2);
        let acme = &store.clients[1];
        assert_eq!(acme.name, "Acme");
        assert_eq!(acme.tabs[0].url.as_deref(), Some("https://crm.zoho.com/crm/org1/tab/Leads"));
        assert_eq!(acme.symbol_name.as_deref(), Some("hammer.fill"));
        let json = String::from_utf8(store.to_json()).unwrap();
        assert!(json.contains("\"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11\""), "UUIDs stay upper-case");
        assert!(json.contains("\"createdAt\":800000000.5"));
        assert!(!json.contains("pdfPath"), "absent optionals stay absent");
        let again = Store::from_json(json.as_bytes()).unwrap();
        assert_eq!(again.clients, store.clients);
    }

    /// `ZCLIENTS_REAL_FILE=<path> cargo test real_file` checks a live
    /// clients.json decodes and re-encodes without losing anything.
    #[test]
    fn real_file_round_trips_when_given() {
        let Some(path) = std::env::var_os("ZCLIENTS_REAL_FILE") else { return };
        let data = std::fs::read(path).unwrap();
        let store = Store::from_json(&data).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let again: serde_json::Value = serde_json::from_slice(&store.to_json()).unwrap();
        assert_eq!(original["clients"].as_array().unwrap().len(), again["clients"].as_array().unwrap().len());
        for (a, b) in original["clients"].as_array().unwrap().iter().zip(again["clients"].as_array().unwrap()) {
            assert_eq!(a["id"], b["id"]);
            assert_eq!(a["name"], b["name"]);
            assert_eq!(a["tabs"].as_array().map(|t| t.len()), b["tabs"].as_array().map(|t| t.len()));
            for (ta, tb) in a["tabs"].as_array().unwrap().iter().zip(b["tabs"].as_array().unwrap()) {
                assert_eq!(ta["url"], tb["url"], "tab urls preserved");
            }
        }
        eprintln!("real file OK: {} clients", store.clients.len());
    }

    #[test]
    fn reads_the_bare_array_format() {
        let store = Store::from_json(br#"[{"id":"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11","name":"Old","tabs":[]}]"#).unwrap();
        assert!(store.clients[0].is_search(), "search client is created and pinned first");
        assert_eq!(store.clients[1].tabs.len(), 1, "empty tab list is repaired");
        assert!(store.clients[1].created_at.is_some());
    }

    #[test]
    fn sign_in_pages_do_not_replace_the_remembered_page() {
        let mut store = Store::from_json(SWIFT_SAMPLE.as_bytes()).unwrap();
        let tab = store.clients[1].tabs[0].id;
        assert!(!store.update_tab_meta(tab, Some("https://accounts.zoho.com/signin?serviceurl=x"), Some("Zoho Accounts")));
        assert_eq!(store.clients[1].tabs[0].url.as_deref(), Some("https://crm.zoho.com/crm/org1/tab/Leads"));
        assert_eq!(store.clients[1].tabs[0].title, "Leads");
        assert!(store.update_tab_meta(tab, Some("https://crm.zoho.com/crm/org1/tab/Deals"), Some("Deals")));
        assert_eq!(store.clients[1].tabs[0].url.as_deref(), Some("https://crm.zoho.com/crm/org1/tab/Deals"));
    }

    #[test]
    fn tabs_saved_on_the_sign_in_form_are_sent_back_to_their_page() {
        let json = r#"[{"id":"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11","name":"E&V","tabs":[{"id":"11111111-2222-3333-4444-555555555555","url":"https://accounts.zoho.com/signin?servicename=ZohoCRM&context=&serviceurl=https%3A%2F%2Fcrm.zoho.com%2Fcrm%2Forg1%2Ftab%2FLeads","title":"Zoho Accounts"}]}]"#;
        let store = Store::from_json(json.as_bytes()).unwrap();
        let t = &store.clients[1].tabs[0];
        assert_eq!(t.url.as_deref(), Some("https://crm.zoho.com/crm/org1/tab/Leads"));
        assert_eq!(t.title, "Zoho CRM");
    }

    #[test]
    fn tab_drag_semantics() {
        let mut c = Client::new("x", "#000000");
        c.tabs = (0..4).map(|_| Tab::default()).collect();
        let ids: Vec<Uuid> = c.tabs.iter().map(|t| t.id).collect();
        assert!(c.move_tab_over(ids[0], ids[2]));
        assert_eq!(c.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), vec![ids[1], ids[2], ids[0], ids[3]]);
        assert!(c.move_tab_over(ids[3], ids[1]));
        assert_eq!(c.tabs[0].id, ids[3]);
        assert!(!c.move_tab_over(ids[3], ids[3]));
        assert!(!c.move_tab_by(ids[3], -1), "clamped at the edge");
    }

    #[test]
    fn closing_and_reopening_tabs() {
        let mut store = Store::default();
        let id = store.add_client("Acme", "#123", None);
        let first = store.client(id).unwrap().tabs[0].id;
        let second = store.add_tab(id, Some("https://crm.zoho.com".into()), None, None, true).unwrap();
        let effects = store.close_tab(id, second);
        assert_eq!(effects, vec![Effect::ResetTab(second)]);
        assert_eq!(store.client(id).unwrap().active_tab_id, Some(first));
        assert!(store.reopen_closed_tab());
        assert_eq!(store.client(id).unwrap().active_tab_id, Some(second));
    }

    #[test]
    fn background_tabs_line_up_in_click_order() {
        let mut store = Store::default();
        let id = store.add_client("Acme", "#123", None);
        let page = store.client(id).unwrap().tabs[0].id;
        let first = store.add_tab(id, Some("https://crm.zoho.com/a".into()), None, None, false).unwrap();
        let second = store.add_tab_after(id, first, "https://crm.zoho.com/b".into(), "Loading…".into(), None).unwrap();
        let c = store.client(id).unwrap();
        assert_eq!(c.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), vec![page, first, second]);
        assert_eq!(c.active_tab_id, Some(page), "the user stays where they were");
        assert!(store.add_tab_after(id, Uuid::new_v4(), "https://x".into(), "x".into(), None).is_none());
    }

    #[test]
    fn discarded_tabs_cannot_be_reopened() {
        let mut store = Store::default();
        let id = store.add_client("Acme", "#123", None);
        let first = store.client(id).unwrap().tabs[0].id;
        let blank = store.add_tab(id, Some("https://workdrive.zoho.com/download/x".into()), None, None, true).unwrap();
        let effects = store.discard_tab(id, blank);
        assert_eq!(effects, vec![Effect::ResetTab(blank), Effect::DropTab(blank)]);
        assert_eq!(store.client(id).unwrap().active_tab_id, Some(first));
        assert!(!store.reopen_closed_tab(), "nothing to bring back");
    }

    #[test]
    fn pdfs_open_as_their_own_tab_after_the_current_one() {
        let mut store = Store::default();
        let id = store.add_client("Acme", "#123", None);
        let first = store.client(id).unwrap().tabs[0].id;
        let _third = store.add_tab(id, Some("https://crm.zoho.com".into()), None, None, false).unwrap();
        let pdf = store.add_pdf_tab(id, "/tmp/PDF Previews/a.pdf", "Invoice 12.pdf", Some("https://books.zoho.com/api/inv.pdf".into())).unwrap();
        let c = store.client(id).unwrap();
        assert_eq!(c.active_tab_id, Some(pdf));
        assert_eq!(c.tabs[0].id, first);
        assert_eq!(c.tabs[1].id, pdf, "inserted next to the tab it came from");
        let t = &c.tabs[1];
        assert_eq!(t.display_title(), "Invoice 12.pdf");
        assert_eq!(t.pdf_path.as_deref(), Some("/tmp/PDF Previews/a.pdf"));
        assert!(!store.update_tab_meta(pdf, Some("file:///x"), Some("other")), "a PDF tab keeps its name");
        let effects = store.close_tab(id, pdf);
        assert_eq!(effects, vec![Effect::ResetTab(pdf)], "file stays while ⌘⇧T can bring it back");

        let blob = store.add_pdf_tab(id, "/tmp/PDF Previews/b.pdf", "", None).unwrap();
        let t = store.find_tab_mut(blob).unwrap();
        assert_eq!(t.url.as_deref(), Some("file:///tmp/PDF%20Previews/b.pdf"));
        assert_eq!(t.title, "PDF");
        assert!(store.add_pdf_tab(Uuid::new_v4(), "/tmp/x.pdf", "x", None).is_none());
    }

    #[test]
    fn search_workspace_keeps_its_last_search_tab() {
        let mut store = Store::default();
        store.ensure_search_client();
        let tab = store.client(GOOGLE_SEARCH_ID).unwrap().tabs[0].id;
        assert!(!store.can_close_tab(GOOGLE_SEARCH_ID, tab));
        assert!(store.close_tab(GOOGLE_SEARCH_ID, tab).is_empty());
        assert_eq!(store.client(GOOGLE_SEARCH_ID).unwrap().tabs.len(), 1);
    }

    #[test]
    fn sorting_and_dragging_clients() {
        let mut store = Store::default();
        store.ensure_search_client();
        let b = store.add_client("beta", "#111", None);
        let a = store.add_client("Álpha", "#222", None);
        store.set_sort_mode(SortMode::NameAscending);
        let names: Vec<_> = store.ordered().iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["Google Search", "Álpha", "beta"]);
        assert!(store.move_client_over(b, a), "drag freezes the sort into a custom order");
        assert_eq!(store.sort_mode, SortMode::Manual);
        let names: Vec<_> = store.ordered().iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["Google Search", "beta", "Álpha"]);
        assert!(!store.move_client_over(b, GOOGLE_SEARCH_ID), "search row is pinned");
    }

    #[test]
    fn deleting_a_client_asks_for_full_cleanup() {
        let mut store = Store::default();
        let id = store.add_client("Gone", "#333", None);
        let tabs: Vec<Uuid> = store.client(id).unwrap().tabs.iter().map(|t| t.id).collect();
        let effects = store.delete_client(id);
        assert!(effects.contains(&Effect::DeleteClient { client_id: id, tab_ids: tabs }));
        assert!(store.client(id).is_none());
        assert!(store.delete_client(GOOGLE_SEARCH_ID).is_empty());
    }

    #[test]
    fn duplicate_ids_are_split_never_merged() {
        let json = r#"[{"id":"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11","name":"A","tabs":[{"id":"11111111-2222-3333-4444-555555555555","url":"https://crm.zoho.com/a"}]},
                       {"id":"EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11","name":"B","activeTabId":"11111111-2222-3333-4444-555555555555","tabs":[{"id":"11111111-2222-3333-4444-555555555555","url":"https://crm.zoho.com/b"}]}]"#;
        let store = Store::from_json(json.as_bytes()).unwrap();
        let ids: std::collections::HashSet<Uuid> = store.clients.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), store.clients.len(), "every profile has its own id (its own data store)");
        let b = store.clients.iter().find(|c| c.name.starts_with('B')).unwrap();
        assert_eq!(b.name, "B (recovered)");
        assert_ne!(b.id, Uuid::parse_str("EF893E42-DD8A-4C7E-9A63-1F6C3B0E2F11").unwrap(), "A keeps the original store");
        let tab_ids: Vec<Uuid> = store.clients.iter().flat_map(|c| c.tabs.iter().map(|t| t.id)).collect();
        let unique: std::collections::HashSet<_> = tab_ids.iter().collect();
        assert_eq!(unique.len(), tab_ids.len());
        assert_eq!(b.active_tab_id, Some(b.tabs[0].id), "the active tab follows its new id");
        let two_search = format!(
            r#"[{{"id":"{0}","name":"S","kind":"googleSearch","tabs":[]}},{{"id":"{0}","name":"S2","kind":"googleSearch","tabs":[]}}]"#,
            GOOGLE_SEARCH_ID.hyphenated().to_string().to_uppercase()
        );
        let store = Store::from_json(two_search.as_bytes()).unwrap();
        assert_eq!(store.clients.iter().filter(|c| c.is_search()).count(), 1);
    }

    #[test]
    fn search_remembers_the_profile_it_came_from() {
        let mut store = Store::default();
        store.ensure_search_client();
        let a = store.add_client("A", "#111", None);
        let b = store.add_client("B", "#222", None);
        store.active_client_id = Some(a);
        store.open_google_search(&url::Url::parse("https://www.google.com/search?q=x").unwrap(), Some(a));
        let tab = store.active_tab().unwrap().1.id;
        assert_eq!(store.search_origin(tab), Some(a));
        // A search started from inside the Search workspace keeps the origin.
        store.open_google_search(&url::Url::parse("https://www.google.com/search?q=y").unwrap(), Some(GOOGLE_SEARCH_ID));
        assert_eq!(store.search_origin(tab), Some(a));
        store.active_client_id = Some(b);
        store.open_google_search(&url::Url::parse("https://www.google.com/search?q=z").unwrap(), Some(b));
        assert_eq!(store.search_origin(tab), Some(b), "the last profile to search owns the results");
        let json = String::from_utf8(store.to_json()).unwrap();
        let restored = Store::from_json(json.as_bytes()).unwrap();
        assert_eq!(restored.search_origin(tab), Some(b), "survives a restart");
        let mut store = restored;
        store.delete_client(b);
        assert_eq!(store.search_origin(tab), None, "a deleted profile is never used");
        assert!(!store.open_google_meet_in(GOOGLE_SEARCH_ID), "Meet never runs in the Search pseudo-profile");
        assert!(store.open_google_meet_in(a));
    }

    #[test]
    fn unreadable_or_damaged_profiles_are_problems_not_empty_lists() {
        use std::io::{Error, ErrorKind};
        assert!(matches!(classify_read(Err(Error::from(ErrorKind::NotFound))), Ok(None)));
        assert!(matches!(classify_read(Err(Error::from(ErrorKind::PermissionDenied))), Err(LoadProblem::Unreadable(_))));
        assert!(matches!(classify_read(Err(Error::other("I/O error"))), Err(LoadProblem::Unreadable(_))));
        assert!(matches!(classify_read(Ok(b"{\"version\":4,\"clients\":[".to_vec())), Err(LoadProblem::Corrupt(_))), "truncated / partial write");
        assert!(matches!(classify_read(Ok(Vec::new())), Err(LoadProblem::Corrupt(_))), "empty file");
        assert!(matches!(classify_read(Ok(b"{\"version\":4,\"clients\":7}".to_vec())), Err(LoadProblem::Corrupt(_))), "schema error");
        assert_eq!(classify_read(Ok(b"{\"version\":9,\"clients\":[]}".to_vec())).err(), Some(LoadProblem::FutureVersion(9)));
        assert!(matches!(classify_read(Ok(SWIFT_SAMPLE.as_bytes().to_vec())), Ok(Some(_))));
    }

    #[test]
    fn new_profile_fields_are_optional_and_round_trip() {
        let mut store = Store::from_json(SWIFT_SAMPLE.as_bytes()).unwrap();
        let json = String::from_utf8(store.to_json()).unwrap();
        assert!(!json.contains("keepSignedIn") && !json.contains("originClientId"), "absent unless set: the Swift app reads this file unchanged");
        store.clients[1].keep_signed_in = Some(false);
        store.clients[1].session_days = Some(30);
        let again = Store::from_json(&store.to_json()).unwrap();
        assert_eq!(again.clients[1].retention(), crate::session::Retention { keep: false, days: 30 });
    }

    #[test]
    fn retention_defaults_and_bounds() {
        let mut c = Client::new("x", "#000");
        assert_eq!(c.retention(), crate::session::Retention { keep: true, days: crate::session::DEFAULT_RETENTION_DAYS });
        c.keep_signed_in = Some(false);
        c.session_days = Some(9999);
        assert_eq!(c.retention(), crate::session::Retention { keep: false, days: crate::session::DEFAULT_RETENTION_DAYS });
    }

    #[test]
    fn titles_and_colours() {
        assert_eq!(normalized_page_title(Some("  Leads \n\t| Zoho  CRM ")).as_deref(), Some("Leads | Zoho CRM"));
        assert_eq!(normalized_page_title(Some("undefined")), None);
        assert_eq!(fallback_title(Some("https://books.zoho.com/app")), "Zoho Books");
        assert_eq!(fallback_title(Some("https://1-a.zappsusercontent.com/w")), "Zoho Widget");
        assert_eq!(normalized_hex("ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(normalized_hex("#12345g"), None);
    }
}
