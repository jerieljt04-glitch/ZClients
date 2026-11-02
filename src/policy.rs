//! Which sites ZClients will load, trust and remember.
//!
//! Everything here is pure string/URL logic so it can be unit-tested without
//! a window or a web view. The WebKit delegates in `web.rs` ask these
//! functions before every navigation, dialog, file picker and download.

use url::Url;

use crate::model::Workspace;

/// Zoho's own data centres. A host matches when it is the domain itself or any
/// subdomain of it.
const ZOHO_DOMAINS: &[&str] = &["zoho.com", "zoho.eu", "zoho.in", "zoho.com.au", "zoho.jp", "zoho.ca", "zoho.sa", "zoho.com.cn", "zoho.uk", "zohocloud.ca", "zoho.ae", "zoho.sg"];

/// One entry per Zoho data centre, as Zoho's multi-DC documentation lists
/// them (accounts.<this> is that centre's sign-in server): US, EU, IN, AU,
/// JP, CA, SA, UK, CN, plus the newer UAE and Singapore centres.
/// <https://www.zoho.com/accounts/protocol/oauth/multi-dc.html>
pub const ZOHO_DATA_CENTRES: &[&str] = &["zoho.com", "zoho.eu", "zoho.in", "zoho.com.au", "zoho.jp", "zohocloud.ca", "zoho.sa", "zoho.uk", "zoho.com.cn", "zoho.ae", "zoho.sg"];

/// Zoho products that place calls in the browser (Zoho Meeting, Cliq).
/// Their exact origin in each data centre may use the camera and microphone
/// without WebKit asking on every call.
const ZOHO_CALL_PRODUCTS: &[&str] = &["meeting", "cliq"];

/// The cookie domains whose sign-in cookies the session keeper may back up
/// and restore: Zoho's own data centres and Google's account cookies (Meet
/// and the Search workspace sign in through them).
const SESSION_GOOGLE_DOMAINS: &[&str] = &["google.com", "google.ae"];

/// The country part a data centre's other Zoho hosts end in: `eu` for
/// zoho.eu, `com.au` for zoho.com.au, `ca` for Canada's zohocloud.ca.
pub fn dc_suffix(dc: &str) -> &str {
    dc.strip_prefix("zoho.").or_else(|| dc.strip_prefix("zohocloud.")).unwrap_or(dc)
}

/// `<base>.<suffix>` for every data centre, built once. Every host these
/// lists produce was checked to be live on Zoho's servers with a valid
/// certificate (2026-10-26), including the UK, UAE and Singapore ones the
/// hand-written lists missed.
fn per_dc(base: &str) -> Vec<String> {
    ZOHO_DATA_CENTRES.iter().map(|dc| format!("{base}.{}", dc_suffix(dc))).collect()
}

/// Where Zoho serves CRM widgets, extensions and Sigma apps from. These run
/// inside an iframe on crm.zoho.*, so they need the same trust as Zoho itself:
/// without it a widget's `confirm()` silently answers "Cancel", its file
/// upload never opens and "open in new tab" is cancelled.
fn widget_domains() -> &'static [String] {
    static D: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    D.get_or_init(|| per_dc("zappsusercontent"))
}

/// Zoho-owned static, API and public-content hosts that widgets and embedded
/// Zoho views load from.
fn content_domains() -> &'static [String] {
    static D: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let mut v: Vec<String> = ["zohostatic.com", "zohocdn.com", "zohowebstatic.com", "zohoexternal.com"].iter().map(|s| s.to_string()).collect();
        v.extend(per_dc("zohopublic"));
        v.extend(per_dc("zohoapis"));
        v
    })
}

/// Data centres where Log360 Cloud does not answer (checked 2026-10-26:
/// no HTTPS service at log360cloud.manageengine.<sa|sg|com.cn>). Not
/// trusted until Zoho runs it there.
const LOG360_UNAVAILABLE: &[&str] = &["sa", "sg", "com.cn"];

/// Zoho's Log360 Cloud runs under ManageEngine (Zoho's IT brand), one host
/// per data centre.
fn partner_domains() -> &'static [String] {
    static D: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    D.get_or_init(|| per_dc("log360cloud.manageengine").into_iter().filter(|h| !LOG360_UNAVAILABLE.iter().any(|x| h.ends_with(&format!(".{x}")))).collect())
}

/// The data centre (an entry of `ZOHO_DATA_CENTRES`) a Zoho app page is
/// served from: crm.zoho.eu -> zoho.eu. Only a known app's own host counts;
/// www.zoho.com and the shared sign-in page say nothing about where the
/// user's account lives.
pub fn data_centre_of(url: &Url) -> Option<&'static str> {
    if !is_https(url) {
        return None;
    }
    let host = host_lower(url)?;
    crate::apps::app_for_host(&host)?;
    let (label, _) = host.split_once('.')?;
    if matches!(label, "www" | "accounts") {
        return None;
    }
    ZOHO_DATA_CENTRES.iter().copied().find(|dc| host.ends_with(&format!(".{dc}")))
}

/// Where to open a Zoho app's bare address outside the US data centre.
/// Signed out, Zoho sends most of them (crm.zoho.ae, desk.zoho.eu,
/// books.zoho.ae...) to its marketing site on www.zoho.com, whose Sign In is
/// the US one, so the client never reaches its own region. That data
/// centre's sign-in page with the app as `serviceurl` stays in the region,
/// and goes straight on to the app when already signed in. Only
/// `https://<app>.<data centre>/` itself; any other address loads as typed.
pub fn regional_entry(url: &Url) -> Option<Url> {
    if !is_https(url) || !matches!(url.path(), "" | "/") || url.query().is_some() || url.port().is_some() {
        return None;
    }
    let host = host_lower(url)?;
    let dc = ZOHO_DATA_CENTRES.iter().copied().filter(|dc| *dc != "zoho.com").find(|dc| host.ends_with(&format!(".{dc}")))?;
    let app = crate::apps::app_for_host(&host)?;
    if host != format!("{}.{dc}", app.slug) {
        return None;
    }
    let mut entry = Url::parse(&format!("https://accounts.{dc}/signin")).ok()?;
    entry.query_pairs_mut().append_pair("serviceurl", &format!("https://{host}/"));
    Some(entry)
}

/// Identity providers a Zoho organisation can federate its sign-in to
/// (Microsoft Entra / personal Microsoft accounts, Sign in with Apple, Okta,
/// OneLogin). Exact hosts, or every host under a provider's own tenant
/// domain. Google's sign-in is already allowed everywhere.
const IDENTITY_PROVIDER_HOSTS: &[&str] = &["login.microsoftonline.com", "login.live.com", "appleid.apple.com"];
const IDENTITY_PROVIDER_TENANT_DOMAINS: &[&str] = &["okta.com", "onelogin.com"];

pub fn is_identity_provider(url: &Url) -> bool {
    is_https(url)
        && Origin::of(url).is_some_and(|o| {
            o.port == 443 && (IDENTITY_PROVIDER_HOSTS.contains(&o.host.as_str()) || IDENTITY_PROVIDER_TENANT_DOMAINS.iter().any(|d| o.host.ends_with(&format!(".{d}"))))
        })
}

/// May a tab's main frame go to `url` as one step of a single sign-on? Only
/// to an identity provider, and only when the chain started on Zoho's own
/// sign-in page: either the page the tab is on now (`from`) is
/// `https://accounts.<data centre>`, or an earlier step of the same chain
/// was (`chain_active`). Nothing is filled in or bridged on those hosts
/// (they are not pages this app shows), and the provider sends the user
/// back to Zoho's sign-in, which is allowed anyway.
pub fn sso_step_allowed(url: &Url, from: Option<&Url>, chain_active: bool) -> bool {
    is_identity_provider(url) && (chain_active || from.is_some_and(is_zoho_sign_in))
}

const GOOGLE_AUTH_HOSTS: &[&str] = &["accounts.google.com", "consent.google.com"];

const GOOGLE_SEARCH_HOSTS: &[&str] = &["google.com", "www.google.com", "images.google.com", "news.google.com", "lens.google.com", "google.ae", "www.google.ae", "images.google.ae"];

/// A web origin (scheme, host, port) normalised the way browsers compare
/// them. Built only from a parsed URL or WebKit's `WKSecurityOrigin`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    /// The effective port (443 for https with no explicit port).
    pub port: u16,
}

impl Origin {
    /// The origin of `url`. URLs that carry credentials (`user@host`) are
    /// refused outright: they are only ever used to disguise a host.
    pub fn of(url: &Url) -> Option<Origin> {
        // blob:https://crm.zoho.com/uuid belongs to the origin inside it.
        if url.scheme() == "blob" {
            return parse(&url.as_str()["blob:".len()..]).filter(|inner| inner.scheme() != "blob").and_then(|inner| Origin::of(&inner));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let port = url.port_or_known_default()?;
        Some(Origin { scheme: url.scheme().to_ascii_lowercase(), host, port })
    }

    /// From WebKit's security-origin parts. An empty scheme or host is an
    /// opaque origin (sandboxed frame, data:, about:blank) and has none.
    pub fn from_parts(scheme: &str, host: &str, port: i64) -> Option<Origin> {
        if scheme.is_empty() || host.is_empty() {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let port = match port {
            p if p > 0 && p <= u16::MAX as i64 => p as u16,
            _ if scheme == "https" => 443,
            _ if scheme == "http" => 80,
            _ => return None,
        };
        Some(Origin { scheme, host: host.to_ascii_lowercase(), port })
    }

    /// `https://<one of hosts>` on port 443 and nothing else: no other
    /// port, no trailing dot, no subdomain, no look-alike.
    pub fn is_exact_https(&self, hosts: &[String]) -> bool {
        self.scheme == "https" && self.port == 443 && hosts.contains(&self.host)
    }

    pub fn url(&self) -> Option<Url> {
        let default = (self.scheme == "https" && self.port == 443) || (self.scheme == "http" && self.port == 80);
        let text = if default { format!("{}://{}/", self.scheme, self.host) } else { format!("{}://{}:{}/", self.scheme, self.host, self.port) };
        parse(&text)
    }

    pub fn display(&self) -> String {
        match self.url() {
            Some(u) => u.as_str().trim_end_matches('/').to_string(),
            None => self.host.clone(),
        }
    }
}

/// `accounts.<data centre>` for every Zoho data centre (built once).
pub fn sign_in_hosts() -> &'static [String] {
    static HOSTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    HOSTS.get_or_init(|| ZOHO_DATA_CENTRES.iter().map(|d| format!("accounts.{d}")).collect())
}

/// The exact origins that may use the camera and microphone (and get the
/// screen picker) without an extra prompt: Google Meet, and Zoho Meeting
/// and Cliq in every data centre.
pub fn call_hosts() -> &'static [String] {
    static HOSTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    HOSTS.get_or_init(|| {
        let mut v = vec!["meet.google.com".to_string()];
        for p in ZOHO_CALL_PRODUCTS {
            for d in ZOHO_DATA_CENTRES {
                v.push(format!("{p}.{d}"));
            }
        }
        v
    })
}

/// What a page is asking to do. Being allowed to *display* a page grants
/// none of the others: each is decided on its own, by exact origin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capability {
    Display,
    Camera,
    Microphone,
    ScreenCapture,
    Autofill,
    Bridge,
    Download,
    FileChooser,
    ExternalLink,
    Popup,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Granted without asking the user again (macOS still asks once per app
    /// for the camera/microphone).
    Allow,
    /// Allowed only once the user says so (WebKit's prompt, the system
    /// screen picker, or the app's own confirmation).
    Prompt,
    Deny,
}

/// Where a request comes from.
#[derive(Clone, Debug)]
pub struct Request {
    /// The requesting frame's live security origin (None = opaque).
    pub origin: Option<Origin>,
    /// The tab's main-frame origin right now.
    pub top: Option<Origin>,
    pub main_frame: bool,
    /// A real keyboard or mouse event reached this web view recently.
    pub user_gesture: bool,
    pub workspace: Workspace,
}

impl Request {
    fn origin_url(&self) -> Option<Url> {
        self.origin.as_ref().and_then(Origin::url)
    }

    /// The request comes from the main frame, and that frame is the origin
    /// the tab is showing (not a frame that navigated away underneath).
    fn is_top_level(&self) -> bool {
        self.main_frame && self.origin.is_some() && self.origin == self.top
    }

    fn displayable(&self) -> bool {
        self.origin_url().is_some_and(|u| is_allowed_web(&u, self.workspace))
    }
}

/// The single place a capability is decided.
pub fn decide(cap: Capability, r: &Request) -> Decision {
    let Some(origin) = r.origin.as_ref() else { return Decision::Deny };
    let secure = origin.scheme == "https";
    match cap {
        Capability::Display => {
            if r.displayable() {
                Decision::Allow
            } else {
                Decision::Deny
            }
        }
        Capability::Camera | Capability::Microphone => {
            if r.is_top_level() && origin.is_exact_https(call_hosts()) {
                Decision::Allow
            } else if secure {
                // WebKit's own prompt names the origin asking.
                Decision::Prompt
            } else {
                Decision::Deny
            }
        }
        Capability::ScreenCapture => {
            if !secure || !r.main_frame || !r.is_top_level() {
                Decision::Deny
            } else if origin.is_exact_https(call_hosts()) {
                // The system picker is itself the user's choice.
                Decision::Allow
            } else if r.displayable() {
                Decision::Prompt
            } else {
                Decision::Deny
            }
        }
        Capability::Autofill => {
            if r.is_top_level() && origin.is_exact_https(sign_in_hosts()) && r.workspace == Workspace::Zoho {
                Decision::Allow
            } else {
                Decision::Deny
            }
        }
        // The app's own isolated-world scripts may report to it only from
        // pages this app shows.
        Capability::Bridge => {
            if secure && r.displayable() {
                Decision::Allow
            } else {
                Decision::Deny
            }
        }
        // A file panel is always the user's own choice; pages this app shows
        // may open one.
        Capability::FileChooser => {
            if r.displayable() {
                Decision::Allow
            } else {
                Decision::Deny
            }
        }
        Capability::Download | Capability::ExternalLink | Capability::Popup => {
            if !r.displayable() {
                // An outside page (OAuth provider, ad frame) never acts on
                // its own; a user click still gets a confirmation.
                if r.user_gesture && secure { Decision::Prompt } else { Decision::Deny }
            } else if r.user_gesture {
                Decision::Allow
            } else {
                Decision::Prompt
            }
        }
    }
}

pub fn parse(text: &str) -> Option<Url> {
    Url::parse(text).ok()
}

fn host_lower(url: &Url) -> Option<String> {
    url.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase())
}

fn host_in<S: AsRef<str>>(host: &str, domains: &[S]) -> bool {
    domains.iter().map(AsRef::as_ref).any(|d| host == d || (host.len() > d.len() && host.ends_with(d) && host.as_bytes()[host.len() - d.len() - 1] == b'.'))
}

/// Every host this app trusts serves TLS. Requiring it means a downgraded or
/// injected cleartext response can never be treated as a trusted page.
pub fn is_https(url: &Url) -> bool {
    url.scheme() == "https"
}

/// Zoho plus every Zoho-owned host widgets are served from.
pub fn is_zoho_family(url: &Url) -> bool {
    is_https(url) && host_lower(url).is_some_and(|h| host_in(&h, ZOHO_DOMAINS) || host_in(&h, widget_domains()) || host_in(&h, content_domains()) || host_in(&h, partner_domains()))
}

pub fn is_zoho_widget(url: &Url) -> bool {
    is_https(url) && host_lower(url).is_some_and(|h| host_in(&h, widget_domains()))
}

pub fn is_google_meet(url: &Url) -> bool {
    let Some(host) = host_lower(url) else { return false };
    let path = url.path().to_ascii_lowercase();
    match host.as_str() {
        "meet.google.com" => true,
        "apps.google.com" => path == "/meet" || path.starts_with("/meet/"),
        "workspace.google.com" => path == "/products/meet" || path.starts_with("/products/meet/"),
        _ => false,
    }
}

pub fn is_google_search(url: &Url) -> bool {
    host_lower(url).is_some_and(|h| GOOGLE_SEARCH_HOSTS.contains(&h.as_str()))
}

fn is_google_auth(url: &Url) -> bool {
    host_lower(url).is_some_and(|h| GOOGLE_AUTH_HOSTS.contains(&h.as_str()))
}

pub fn is_allowed_google_meet(url: &Url) -> bool {
    is_https(url) && (is_google_meet(url) || is_google_auth(url))
}

/// Google Meet's "start an instant meeting" address: Meet creates a new
/// meeting and sends the tab straight into it.
pub const GOOGLE_MEET_INSTANT: &str = "https://meet.google.com/new";

/// A Meet meeting page (meet.google.com/abc-defg-hij, or
/// meet.google.com/meet/abc-defg-hij, where Meet's "new meeting" lands), as
/// opposed to Meet's home or landing page.
pub fn is_google_meet_call(url: &Url) -> bool {
    if !is_https(url) || host_lower(url).as_deref() != Some("meet.google.com") {
        return false;
    }
    let path = url.path().trim_matches('/');
    let code = path.strip_prefix("meet/").unwrap_or(path);
    let parts: Vec<&str> = code.split('-').collect();
    parts.len() == 3 && [3, 4, 3].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.bytes().all(|b| b.is_ascii_lowercase()))
}

/// A link to a pane of the Mac's System Settings, as Google Meet's "Open
/// Sound Settings" uses when macOS has the microphone turned down. WebKit
/// cannot show it; it belongs to System Settings.
pub fn is_system_settings_link(url: &Url) -> bool {
    url.scheme().eq_ignore_ascii_case("x-apple.systempreferences")
}

pub fn is_allowed_google_search(url: &Url) -> bool {
    is_https(url) && (is_google_search(url) || is_allowed_google_meet(url))
}

/// May this URL be shown as a page in a tab of `workspace`?
pub fn is_allowed_web(url: &Url, workspace: Workspace) -> bool {
    if is_zoho_family(url) || is_allowed_google_meet(url) {
        return true;
    }
    workspace == Workspace::GoogleSearch && is_allowed_google_search(url)
}

pub fn is_allowed_blob(url: &Url, workspace: Workspace) -> bool {
    if url.scheme() != "blob" {
        return false;
    }
    // blob:https://crm.zoho.com/uuid -> the origin is the URL after "blob:".
    let inner = &url.as_str()["blob:".len()..];
    parse(inner).is_some_and(|origin| is_allowed_web(&origin, workspace))
}

pub fn is_allowed_main_frame(url: &Url, workspace: Workspace) -> bool {
    url.as_str() == "about:blank" || is_allowed_web(url, workspace) || is_allowed_blob(url, workspace)
}

pub fn is_safe_download(url: &Url, workspace: Workspace) -> bool {
    is_https(url) || is_allowed_blob(url, workspace) || url.scheme() == "data"
}

/// May an embedded frame turn this response into a download? Only files
/// served by Zoho itself (WorkDrive folder zips, exports) or a Zoho page's
/// own blob:/data: URLs. Third-party frames inside a widget (ads, embeds)
/// cannot drop files into ~/Downloads without a click.
pub fn is_frame_download_allowed(url: &Url, workspace: Workspace) -> bool {
    is_zoho_family(url) || is_allowed_blob(url, workspace) || url.scheme() == "data"
}

/// Zoho and Meet pages that belong to a client rather than the Search
/// store. Only used to repair Search tabs saved by older builds; live
/// routing from the Search workspace is `search_route`.
pub fn should_open_in_client_workspace(url: &Url) -> bool {
    is_zoho_family(url) || is_google_meet(url)
}

/// What the Google Search workspace does with a web address it is asked to
/// open as a page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchRoute {
    /// Shown right there, in the Search workspace's own sign-in: Google
    /// Search and Google's sign-in pages, and Zoho pages.
    Here,
    /// Google Meet: a call belongs to a real client, so it opens in one.
    Client,
    /// Any other website: not opened at all (not in the default browser
    /// either).
    Blocked,
}

/// The Search workspace roams Google Search and may open Zoho pages in
/// place; Meet goes to a client; nothing else is opened. Only web
/// addresses are routed: the tab's own PDF (file:), mailto: and similar
/// links and downloads keep their own rules.
pub fn search_route(url: &Url) -> SearchRoute {
    if is_https(url) && is_google_meet(url) {
        SearchRoute::Client
    } else if is_allowed_main_frame(url, Workspace::GoogleSearch) {
        SearchRoute::Here
    } else {
        SearchRoute::Blocked
    }
}

/// An http(s) address, the only kind `search_route` blocks.
pub fn is_web_address(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// Sign-in pages. A tab that lands here (because a session ended) must not
/// overwrite where the user really was, or the place is lost after signing in.
pub fn is_auth_page(url: &Url) -> bool {
    if is_google_auth(url) {
        return true;
    }
    is_zoho_sign_in(url)
}

/// For a Zoho sign-in URL, the page it will return to afterwards (its
/// `serviceurl`), when that is a Zoho page this app shows.
pub fn sign_in_destination(url: &Url) -> Option<Url> {
    if !is_zoho_sign_in(url) {
        return None;
    }
    let target = url.query_pairs().find(|(k, _)| k.eq_ignore_ascii_case("serviceurl")).map(|(_, v)| v.into_owned())?;
    parse(&target).filter(|t| is_zoho_family(t) && !is_auth_page(t))
}

/// Zoho's own sign-in origin, where saved credentials may be filled in:
/// exactly `https://accounts.<data centre>` on the default port. Never a
/// prefix match: `accounts.` + anything is not an authentication boundary.
pub fn is_zoho_sign_in(url: &Url) -> bool {
    Origin::of(url).is_some_and(|o| o.is_exact_https(sign_in_hosts()))
}

/// Whether a cookie for `domain` may be mirrored to the session backup. The
/// backup holds live credentials, so it is restricted to hosts this app signs
/// in to; third-party and tracking cookies are never stored by us.
pub fn is_persistable_cookie_domain(domain: &str) -> bool {
    let host = domain.trim_start_matches('.');
    if host.is_empty() || host.contains('/') || host.contains('@') {
        return false;
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !host.is_empty() && parse(&format!("https://{host}/")).is_some() && (host_in(&host, ZOHO_DOMAINS) || host_in(&host, SESSION_GOOGLE_DOMAINS))
}

/// Turn typed text into an https URL, or None when it is not address-like.
/// `http://` is upgraded rather than rejected.
pub fn normalized_http_url(text: &str) -> Option<Url> {
    let s = text.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    let has_scheme = {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && s.find(':').is_some_and(|i| s[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)))
    };
    let candidate = if has_scheme {
        let (scheme, rest) = s.split_once(':')?;
        match scheme.to_ascii_lowercase().as_str() {
            "https" => format!("https:{rest}"),
            "http" => format!("https:{rest}"),
            // "localhost:8080" or "crm.zoho.com:443" have no real scheme.
            _ if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) => format!("https://{s}"),
            _ => return None,
        }
    } else {
        format!("https://{s}")
    };
    let url = parse(&candidate)?;
    // A bare word like "books" parses as https://books/ - not an address.
    let host = url.host_str()?;
    if !host.contains('.') {
        return None;
    }
    Some(url)
}

pub fn normalized_web_url(text: &str, workspace: Workspace) -> Option<Url> {
    normalized_http_url(text).filter(|u| is_allowed_web(u, workspace))
}

/// Comparison key for "is this the same page": lower-case scheme/host, no
/// fragment, and "/" treated as empty.
pub fn page_key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    let mut s = u.as_str().to_string();
    if u.path() == "/" && u.query().is_none() {
        s = s.trim_end_matches('/').to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        parse(s).unwrap()
    }

    #[test]
    fn meet_call_pages_are_told_apart_from_meet_home() {
        for call in [
            "https://meet.google.com/tuq-iabd-onj",
            "https://meet.google.com/abc-defg-hij?authuser=1",
            "https://meet.google.com/abc-defg-hij/",
            "https://meet.google.com/meet/puw-zkac-gmm",
        ] {
            assert!(is_google_meet_call(&u(call)), "{call}");
        }
        for not_call in [
            "https://meet.google.com/",
            "https://meet.google.com/home",
            "https://meet.google.com/new",
            "https://meet.google.com/meet/new",
            "https://meet.google.com/meet/",
            "https://meet.google.com/x/abc-defg-hij",
            "https://meet.google.com/landing",
            "https://meet.google.com/ABC-DEFG-HIJ",
            "https://meet.google.com/abcd-efg-hij",
            "http://meet.google.com/abc-defg-hij",
            "https://meet.google.com.evil.net/abc-defg-hij",
            "https://evil.net/abc-defg-hij",
        ] {
            assert!(!is_google_meet_call(&u(not_call)), "{not_call}");
        }
        assert!(is_google_meet(&u(GOOGLE_MEET_INSTANT)) && is_allowed_google_meet(&u(GOOGLE_MEET_INSTANT)));
    }

    #[test]
    fn system_settings_links_are_recognised_and_nothing_else() {
        assert!(is_system_settings_link(&u("x-apple.systempreferences:com.apple.preference.sound?input")));
        assert!(is_system_settings_link(&u("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")));
        for other in ["https://meet.google.com/", "mailto:a@b.c", "file:///System/Applications/System%20Settings.app", "x-apple-helpviewer:foo"] {
            assert!(!is_system_settings_link(&u(other)), "{other}");
        }
    }

    fn req(url: &str, main: bool, gesture: bool) -> Request {
        let o = Origin::of(&parse(url).unwrap());
        Request { origin: o.clone(), top: if main { o } else { Origin::of(&u("https://crm.zoho.com/")) }, main_frame: main, user_gesture: gesture, workspace: Workspace::Zoho }
    }

    #[test]
    fn bare_app_addresses_outside_the_us_sign_in_in_their_own_region() {
        let e = regional_entry(&u("https://crm.zoho.ae")).unwrap();
        assert_eq!(e.as_str(), "https://accounts.zoho.ae/signin?serviceurl=https%3A%2F%2Fcrm.zoho.ae%2F");
        assert_eq!(regional_entry(&u("https://desk.zoho.eu/")).unwrap().host_str(), Some("accounts.zoho.eu"));
        assert_eq!(regional_entry(&u("https://crm.zohocloud.ca/")).unwrap().host_str(), Some("accounts.zohocloud.ca"));
        for kept in [
            "https://crm.zoho.com/",
            "https://crm.zoho.ae/crm/org1/tab/Home",
            "https://crm.zoho.ae/?a=1",
            "https://www.zoho.ae/",
            "https://accounts.zoho.ae/",
            "https://evil.crm.zoho.ae/",
            "https://crm.zoho.ae.evil.net/",
            "http://crm.zoho.ae/",
            "https://notanapp.zoho.ae/",
        ] {
            assert_eq!(regional_entry(&u(kept)), None, "{kept} loads as typed");
        }
    }

    #[test]
    fn meet_and_zoho_call_products_keep_media_without_prompts() {
        for s in ["https://meet.google.com/abc-defg-hij", "https://meeting.zoho.com/meeting/x", "https://cliq.zoho.eu/", "https://meeting.zohocloud.ca/", "https://cliq.zoho.sa/"] {
            assert_eq!(decide(Capability::Camera, &req(s, true, false)), Decision::Allow, "{s}");
            assert_eq!(decide(Capability::Microphone, &req(s, true, false)), Decision::Allow, "{s}");
            assert_eq!(decide(Capability::ScreenCapture, &req(s, true, false)), Decision::Allow, "{s}");
        }
    }

    #[test]
    fn no_zoho_family_host_is_granted_media_automatically() {
        for s in [
            "https://crm.zoho.com/",
            "https://mail.zoho.eu/",
            "https://accounts.zoho.com/",
            "https://123-abc.zappsusercontent.com/",
            "https://forms.zohopublic.com/x",
            "https://acme.zohosites.com/",
            "https://static.zohocdn.com/",
            "https://www.google.com/",
            "https://log360cloud.manageengine.com/",
        ] {
            assert_ne!(decide(Capability::Camera, &req(s, true, true)), Decision::Allow, "{s}");
            assert_ne!(decide(Capability::Microphone, &req(s, true, true)), Decision::Allow, "{s}");
            assert_ne!(decide(Capability::ScreenCapture, &req(s, true, true)), Decision::Allow, "{s}");
        }
    }

    #[test]
    fn disguised_meeting_origins_get_no_privilege() {
        for s in [
            "https://x.meet.google.com/",
            "https://meet.google.com.evil.net/",
            "https://meet.google.com:8443/",
            "http://meet.google.com/",
            "https://meet.google.com./",
            "https://user@meet.google.com/",
            "https://user:pw@meeting.zoho.com/",
            "https://meeting.zoho.com.evil.net/",
            "https://evil-meeting.zoho.com/",
            "https://a.meeting.zoho.com/",
            "https://xn--meet-goo-k3a.google.com/",
            "https://meeting.zoho.com:444/",
            "https://meetíng.zoho.com/",
        ] {
            let Some(url) = parse(s) else { continue };
            let o = Origin::of(&url);
            let r = Request { origin: o.clone(), top: o, main_frame: true, user_gesture: true, workspace: Workspace::Zoho };
            assert_ne!(decide(Capability::Camera, &r), Decision::Allow, "{s}");
            assert_ne!(decide(Capability::ScreenCapture, &r), Decision::Allow, "{s}");
            assert_ne!(decide(Capability::Autofill, &r), Decision::Allow, "{s}");
        }
    }

    #[test]
    fn media_in_a_subframe_or_after_the_top_moved_is_never_automatic() {
        assert_eq!(decide(Capability::Camera, &req("https://meet.google.com/", false, true)), Decision::Prompt, "Meet framed inside CRM");
        let meet = Origin::of(&u("https://meet.google.com/"));
        let moved = Request { origin: meet, top: Origin::of(&u("https://evil.example/")), main_frame: true, user_gesture: true, workspace: Workspace::Zoho };
        assert_ne!(decide(Capability::Camera, &moved), Decision::Allow);
        assert_eq!(decide(Capability::ScreenCapture, &req("https://meet.google.com/", false, true)), Decision::Deny);
        let opaque = Request { origin: None, top: None, main_frame: true, user_gesture: true, workspace: Workspace::Zoho };
        for cap in [Capability::Camera, Capability::Autofill, Capability::Download, Capability::Popup, Capability::Bridge] {
            assert_eq!(decide(cap, &opaque), Decision::Deny, "{cap:?} from an opaque origin");
        }
    }

    #[test]
    fn credentials_only_on_exact_sign_in_origins() {
        for s in ["https://accounts.zoho.com/signin", "https://accounts.zoho.eu/", "https://accounts.zohocloud.ca/", "https://accounts.zoho.com.cn/"] {
            assert_eq!(decide(Capability::Autofill, &req(s, true, false)), Decision::Allow, "{s}");
            assert!(is_zoho_sign_in(&u(s)), "{s}");
        }
        for s in [
            "https://accounts.evil.zoho.com/",
            "https://x.accounts.zoho.com/",
            "https://accounts.zoho.com.evil.net/",
            "https://accounts.zohopublic.com/",
            "https://accounts.zappsusercontent.com/",
            "http://accounts.zoho.com/",
            "https://accounts.zoho.com:8443/",
            "https://accounts.zoho.com./",
            "https://u@accounts.zoho.com/",
            "https://accountsx.zoho.com/",
            "https://accounts.zoho.ca/",
        ] {
            let url = u(s);
            assert!(!is_zoho_sign_in(&url), "{s}");
            assert_ne!(decide(Capability::Autofill, &req(s, true, true)), Decision::Allow, "{s}");
        }
        assert_eq!(decide(Capability::Autofill, &req("https://accounts.zoho.com/", false, true)), Decision::Deny, "never a subframe");
        let mut search = req("https://accounts.zoho.com/", true, true);
        search.workspace = Workspace::GoogleSearch;
        assert_eq!(decide(Capability::Autofill, &search), Decision::Deny, "never in the Search workspace");
    }

    #[test]
    fn downloads_popups_and_deep_links_need_a_gesture_or_consent() {
        for cap in [Capability::Download, Capability::Popup, Capability::ExternalLink] {
            assert_eq!(decide(cap, &req("https://crm.zoho.com/", true, true)), Decision::Allow);
            assert_eq!(decide(cap, &req("https://crm.zoho.com/", true, false)), Decision::Prompt, "{cap:?} without a click asks");
            assert_eq!(decide(cap, &req("https://ads.example.com/", true, false)), Decision::Deny, "{cap:?} outside page, no click");
            assert_eq!(decide(cap, &req("https://ads.example.com/", true, true)), Decision::Prompt, "{cap:?} outside page asks even after a click");
            assert_eq!(decide(cap, &req("http://crm.zoho.com/", true, true)), Decision::Deny, "{cap:?} over cleartext");
        }
    }

    #[test]
    fn origins_compare_like_browsers() {
        assert_eq!(Origin::of(&u("https://Meet.Google.com:443/x")), Origin::of(&u("https://meet.google.com/")));
        assert_eq!(Origin::from_parts("https", "meet.google.com", 0), Origin::of(&u("https://meet.google.com/")));
        assert_eq!(Origin::from_parts("", "x", 0), None);
        assert_eq!(Origin::from_parts("https", "", 0), None);
        assert_ne!(Origin::from_parts("https", "meet.google.com", 444), Origin::of(&u("https://meet.google.com/")));
        assert!(Origin::of(&u("https://a@b.com/")).is_none());
        assert_eq!(Origin::of(&u("blob:https://workdrive.zoho.com/1234")), Origin::of(&u("https://workdrive.zoho.com/")));
        assert_eq!(Origin::of(&u("blob:blob:https://x.com/1")), None);
        assert_eq!(Origin::of(&u("data:text/plain,hi")), None, "data: is opaque");
    }

    #[test]
    fn only_zoho_frames_may_download_without_a_click() {
        assert!(is_frame_download_allowed(&u("https://download.zoho.com/v1/workdrive/download/abc"), Workspace::Zoho));
        assert!(is_frame_download_allowed(&u("https://workdrive.zohoexternal.com/file/x"), Workspace::Zoho));
        assert!(is_frame_download_allowed(&u("blob:https://workdrive.zoho.com/1234"), Workspace::Zoho));
        assert!(!is_frame_download_allowed(&u("https://ads.example.com/setup.dmg"), Workspace::Zoho));
        assert!(!is_frame_download_allowed(&u("blob:https://evil.example/1234"), Workspace::Zoho));
        assert!(!is_frame_download_allowed(&u("http://download.zoho.com/x.zip"), Workspace::Zoho));
    }

    #[test]
    fn zoho_regions_and_subdomains_are_allowed() {
        for s in
            ["https://zoho.com", "https://crm.zoho.com/crm/org1/tab/Home", "https://books.zoho.eu", "https://crm.zoho.in", "https://accounts.zohocloud.ca", "https://crm.zoho.uk"]
        {
            assert!(is_allowed_web(&u(s), Workspace::Zoho), "{s}");
        }
    }

    #[test]
    fn every_data_centre_is_a_first_class_citizen() {
        assert_eq!(ZOHO_DATA_CENTRES.len(), 11);
        for dc in ZOHO_DATA_CENTRES {
            let sfx = dc_suffix(dc);
            assert!(ZOHO_DOMAINS.contains(dc), "{dc}");
            for host in [
                format!("https://crm.{dc}/"),
                format!("https://accounts.{dc}/"),
                format!("https://1-abc.zappsusercontent.{sfx}/w.html"),
                format!("https://www.zohoapis.{sfx}/crm/v6"),
                format!("https://forms.zohopublic.{sfx}/x"),
            ] {
                assert!(is_allowed_web(&u(&host), Workspace::Zoho), "{host}");
            }
            assert!(is_zoho_widget(&u(&format!("https://1-abc.zappsusercontent.{sfx}/"))), "{dc}");
            let widget = Request {
                origin: Origin::from_parts("https", &format!("1-abc.zappsusercontent.{sfx}"), 0),
                top: None,
                main_frame: false,
                user_gesture: false,
                workspace: Workspace::Zoho,
            };
            assert_eq!(decide(Capability::Display, &widget), Decision::Allow, "{dc} widgets may show dialogs");
            assert_eq!(data_centre_of(&u(&format!("https://crm.{dc}/crm/org1"))), Some(*dc));
        }
        assert_eq!(dc_suffix("zohocloud.ca"), "ca");
        assert_eq!(dc_suffix("zoho.com.au"), "com.au");
        assert!(is_allowed_web(&u("https://log360cloud.manageengine.uk/"), Workspace::Zoho));
        assert!(!is_allowed_web(&u("https://log360cloud.manageengine.sa/"), Workspace::Zoho), "not served there");
        assert!(!is_allowed_web(&u("https://zappsusercontent.ae.evil.net/"), Workspace::Zoho));
    }

    #[test]
    fn only_app_hosts_reveal_a_profiles_data_centre() {
        assert_eq!(data_centre_of(&u("https://books.zoho.com.au/app")), Some("zoho.com.au"));
        assert_eq!(data_centre_of(&u("https://crm.zohocloud.ca/")), Some("zohocloud.ca"));
        assert_eq!(data_centre_of(&u("https://www.zoho.com/crm/")), None, "the marketing site");
        assert_eq!(data_centre_of(&u("https://accounts.zoho.com/signin")), None, "everyone signs in through the US first");
        assert_eq!(data_centre_of(&u("https://www.google.com/")), None);
        assert_eq!(data_centre_of(&u("http://crm.zoho.eu/")), None);
    }

    #[test]
    fn search_workspace_opens_google_and_zoho_sends_meet_to_a_client_and_blocks_the_rest() {
        for here in [
            "https://www.google.com/search?q=zoho+crm",
            "https://www.google.com/url?q=https://example.com",
            "https://accounts.google.com/signin",
            "https://crm.zoho.com/crm/org1/tab/Leads",
            "https://books.zoho.eu/app",
            "https://www.zoho.com/crm/pricing/",
            "https://forms.zohopublic.in/x",
            "https://123-abc.zappsusercontent.com/w.html",
            "about:blank",
        ] {
            assert_eq!(search_route(&u(here)), SearchRoute::Here, "{here}");
        }
        for meet in ["https://meet.google.com/abc-defg-hij", "https://workspace.google.com/products/meet/"] {
            assert_eq!(search_route(&u(meet)), SearchRoute::Client, "{meet}");
        }
        for blocked in [
            "https://example.com/",
            "https://www.youtube.com/watch?v=1",
            "https://mail.google.com/",
            "https://zoho.com.evil.net/",
            "http://crm.zoho.com/",
            "http://meet.google.com/",
            "https://login.microsoftonline.com/",
        ] {
            assert_eq!(search_route(&u(blocked)), SearchRoute::Blocked, "{blocked}");
            assert!(is_web_address(&u(blocked)));
        }
        assert!(!is_web_address(&u("mailto:a@b.com")), "mailto keeps its own confirmation");
        assert!(!is_web_address(&u("file:///tmp/a.pdf")), "the tab's own PDF keeps its own check");
    }

    #[test]
    fn single_sign_on_only_from_zohos_sign_in_page() {
        let sign_in = u("https://accounts.zoho.eu/signin?servicename=ZohoCRM");
        for idp in [
            "https://login.microsoftonline.com/common/saml2",
            "https://login.live.com/oauth20",
            "https://appleid.apple.com/auth/authorize",
            "https://acme.okta.com/app/zoho/sso/saml",
            "https://acme.onelogin.com/trust/saml2",
        ] {
            assert!(sso_step_allowed(&u(idp), Some(&sign_in), false), "{idp} from Zoho's sign-in");
            assert!(sso_step_allowed(&u(idp), Some(&u("https://login.microsoftonline.com/x")), true), "{idp} later in the same chain");
            assert!(!sso_step_allowed(&u(idp), Some(&u("https://crm.zoho.eu/crm/org1")), false), "{idp} from an ordinary Zoho page");
            assert!(!sso_step_allowed(&u(idp), None, false));
            assert!(!is_allowed_web(&u(idp), Workspace::Zoho), "never a page the app trusts");
            let r = Request { origin: Origin::of(&u(idp)), top: Origin::of(&u(idp)), main_frame: true, user_gesture: true, workspace: Workspace::Zoho };
            for cap in [Capability::Autofill, Capability::Bridge, Capability::Display] {
                assert_eq!(decide(cap, &r), Decision::Deny, "{cap:?} on {idp}");
            }
        }
        for bad in [
            "http://login.microsoftonline.com/",
            "https://login.microsoftonline.com.evil.net/",
            "https://okta.com.evil.net/",
            "https://evilokta.com/",
            "https://login.microsoftonline.com:8443/",
            "https://user@acme.okta.com/",
            "https://evil.example/",
        ] {
            assert!(!sso_step_allowed(&u(bad), Some(&sign_in), true), "{bad}");
        }
        assert!(!sso_step_allowed(&u("https://acme.okta.com/"), Some(&u("https://accounts.zoho.com.evil.net/")), false));
    }

    #[test]
    fn crm_widgets_are_trusted() {
        let widget = u("https://123456-abcdef.zappsusercontent.com/appfiles/widget.html");
        assert!(is_zoho_widget(&widget));
        assert!(is_allowed_web(&widget, Workspace::Zoho));
        let widget_frame =
            Request { origin: Origin::from_parts("https", "123456-abcdef.zappsusercontent.eu", 0), top: None, main_frame: false, user_gesture: false, workspace: Workspace::Zoho };
        assert_eq!(decide(Capability::Display, &widget_frame), Decision::Allow);
        assert!(is_allowed_web(&u("https://static.zohocdn.com/x.js"), Workspace::Zoho));
        assert!(is_allowed_web(&u("https://log360cloud.manageengine.com/"), Workspace::Zoho));
        assert!(!is_allowed_web(&u("https://www.manageengine.com/"), Workspace::Zoho));
    }

    #[test]
    fn lookalike_hosts_and_userinfo_are_rejected() {
        for s in ["https://evilzoho.com", "https://zoho.com.evil.net", "https://crm.zoho.com@evil.net/", "https://zappsusercontent.com.evil.net", "https://notzappsusercontent.com"]
        {
            assert!(!is_allowed_web(&u(s), Workspace::Zoho), "{s}");
        }
    }

    #[test]
    fn cleartext_is_never_trusted() {
        assert!(!is_allowed_web(&u("http://crm.zoho.com"), Workspace::Zoho));
        assert!(!is_allowed_web(&u("http://www.google.com"), Workspace::GoogleSearch));
        let cleartext = Request { origin: Origin::from_parts("http", "crm.zoho.com", 0), top: None, main_frame: true, user_gesture: true, workspace: Workspace::Zoho };
        assert_eq!(decide(Capability::Display, &cleartext), Decision::Deny);
        assert!(!is_safe_download(&u("http://files.example.com/a.pdf"), Workspace::Zoho));
    }

    #[test]
    fn google_search_only_in_search_workspace_but_meet_everywhere() {
        assert!(!is_allowed_web(&u("https://www.google.com/search?q=x"), Workspace::Zoho));
        assert!(is_allowed_web(&u("https://www.google.com/search?q=x"), Workspace::GoogleSearch));
        assert!(is_allowed_web(&u("https://meet.google.com/abc-defg-hij"), Workspace::Zoho));
        assert!(!is_allowed_web(&u("https://mail.google.com"), Workspace::Zoho));
    }

    #[test]
    fn blob_inherits_origin_trust() {
        assert!(is_allowed_blob(&u("blob:https://crm.zoho.com/1234"), Workspace::Zoho));
        assert!(!is_allowed_blob(&u("blob:https://evil.net/1234"), Workspace::Zoho));
        assert!(is_safe_download(&u("data:text/plain,hi"), Workspace::Zoho));
    }

    #[test]
    fn typed_addresses_are_upgraded_and_words_are_not_urls() {
        assert_eq!(normalized_http_url("http://crm.zoho.com").unwrap().as_str(), "https://crm.zoho.com/");
        assert_eq!(normalized_http_url("crm.zoho.com/crm").unwrap().as_str(), "https://crm.zoho.com/crm");
        assert!(normalized_http_url("books").is_none());
        assert!(normalized_http_url("zoho books").is_none());
        assert!(normalized_http_url("javascript:alert(1)").is_none());
        assert!(normalized_web_url("example.com", Workspace::Zoho).is_none());
    }

    #[test]
    fn sign_in_pages_are_recognised() {
        assert!(is_auth_page(&u("https://accounts.zoho.com/signin?servicename=ZohoCRM")));
        assert!(is_auth_page(&u("https://accounts.google.com/o/oauth2")));
        assert!(!is_auth_page(&u("https://crm.zoho.com/crm/org1/tab/Leads")));
        assert!(is_zoho_sign_in(&u("https://accounts.zoho.eu/signin")));
    }

    #[test]
    fn sign_in_destination_only_returns_zoho_pages() {
        let ok = u("https://accounts.zoho.com/signin?serviceurl=https%3A%2F%2Fcrm.zoho.com%2Fcrm%2Forg1");
        assert_eq!(sign_in_destination(&ok).unwrap().as_str(), "https://crm.zoho.com/crm/org1");
        let evil = u("https://accounts.zoho.com/signin?serviceurl=https%3A%2F%2Fevil.net%2F");
        assert!(sign_in_destination(&evil).is_none());
        assert!(sign_in_destination(&u("https://crm.zoho.com/?serviceurl=https%3A%2F%2Fcrm.zoho.com")).is_none());
    }

    #[test]
    fn only_sign_in_domains_are_mirrored() {
        assert!(is_persistable_cookie_domain(".zoho.com"));
        assert!(is_persistable_cookie_domain("accounts.zoho.eu"));
        assert!(is_persistable_cookie_domain(".google.com"));
        assert!(!is_persistable_cookie_domain(".doubleclick.net"));
        assert!(!is_persistable_cookie_domain(".zappsusercontent.com"), "customer widget hosts never hold sign-ins");
        assert!(!is_persistable_cookie_domain(".zohopublic.com"));
        assert!(!is_persistable_cookie_domain("zoho.com.evil.net"));
        assert!(!is_persistable_cookie_domain(""));
        assert!(!is_persistable_cookie_domain("evil.net/zoho.com"));
    }

    #[test]
    fn page_key_ignores_fragment_and_trailing_slash() {
        assert_eq!(page_key(&u("https://CRM.zoho.com/#x")), page_key(&u("https://crm.zoho.com")));
        assert_ne!(page_key(&u("https://crm.zoho.com/a")), page_key(&u("https://crm.zoho.com/b")));
    }
}
