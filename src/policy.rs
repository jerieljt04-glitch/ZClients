//! Which sites ZClients will load, trust and remember.
//!
//! Everything here is pure string/URL logic so it can be unit-tested without
//! a window or a web view. The WebKit delegates in `web.rs` ask these
//! functions before every navigation, dialog, file picker and download.

use url::Url;

use crate::model::Workspace;

/// Zoho's own data centres. A host matches when it is the domain itself or any
/// subdomain of it.
const ZOHO_DOMAINS: &[&str] = &[
    "zoho.com", "zoho.eu", "zoho.in", "zoho.com.au", "zoho.jp", "zoho.ca", "zoho.sa",
    "zoho.com.cn", "zoho.uk", "zohocloud.ca",
];

/// Where Zoho serves CRM widgets, extensions and Sigma apps from. These run
/// inside an iframe on crm.zoho.*, so they need the same trust as Zoho itself:
/// without it a widget's `confirm()` silently answers "Cancel", its file
/// upload never opens and "open in new tab" is cancelled.
const ZOHO_WIDGET_DOMAINS: &[&str] = &[
    "zappsusercontent.com", "zappsusercontent.eu", "zappsusercontent.in",
    "zappsusercontent.com.au", "zappsusercontent.jp", "zappsusercontent.ca",
    "zappsusercontent.sa", "zappsusercontent.com.cn", "zappsusercontent.uk",
];

/// Zoho-owned static, API and public-content hosts that widgets and embedded
/// Zoho views load from.
const ZOHO_CONTENT_DOMAINS: &[&str] = &[
    "zohostatic.com", "zohocdn.com", "zohowebstatic.com", "zohoexternal.com",
    "zohopublic.com", "zohopublic.eu", "zohopublic.in", "zohopublic.com.au",
    "zohopublic.jp", "zohopublic.ca", "zohopublic.sa", "zohopublic.com.cn",
    "zohoapis.com", "zohoapis.eu", "zohoapis.in", "zohoapis.com.au", "zohoapis.jp",
    "zohoapis.ca", "zohoapis.sa", "zohoapis.com.cn",
];

/// Zoho's Log360 Cloud runs under ManageEngine (Zoho's IT brand), one host
/// per data centre.
const ZOHO_PARTNER_DOMAINS: &[&str] = &[
    "log360cloud.manageengine.com", "log360cloud.manageengine.eu", "log360cloud.manageengine.in",
    "log360cloud.manageengine.com.au", "log360cloud.manageengine.jp", "log360cloud.manageengine.ca",
    "log360cloud.manageengine.uk", "log360cloud.manageengine.ae",
];

const GOOGLE_AUTH_HOSTS: &[&str] = &["accounts.google.com", "consent.google.com"];

const GOOGLE_SEARCH_HOSTS: &[&str] = &[
    "google.com", "www.google.com", "images.google.com", "news.google.com",
    "lens.google.com", "google.ae", "www.google.ae", "images.google.ae",
];

pub fn parse(text: &str) -> Option<Url> {
    Url::parse(text).ok()
}

fn host_lower(url: &Url) -> Option<String> {
    url.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase())
}

fn host_in(host: &str, domains: &[&str]) -> bool {
    domains
        .iter()
        .any(|d| host == *d || (host.len() > d.len() && host.ends_with(d) && host.as_bytes()[host.len() - d.len() - 1] == b'.'))
}

/// Every host this app trusts serves TLS. Requiring it means a downgraded or
/// injected cleartext response can never be treated as a trusted page.
pub fn is_https(url: &Url) -> bool {
    url.scheme() == "https"
}

/// The Zoho application itself (crm.zoho.com, accounts.zoho.eu, ...).
pub fn is_zoho_app(url: &Url) -> bool {
    is_https(url) && host_lower(url).is_some_and(|h| host_in(&h, ZOHO_DOMAINS))
}

/// Zoho plus every Zoho-owned host widgets are served from.
pub fn is_zoho_family(url: &Url) -> bool {
    is_https(url)
        && host_lower(url).is_some_and(|h| {
            host_in(&h, ZOHO_DOMAINS)
                || host_in(&h, ZOHO_WIDGET_DOMAINS)
                || host_in(&h, ZOHO_CONTENT_DOMAINS)
                || host_in(&h, ZOHO_PARTNER_DOMAINS)
        })
}

pub fn is_zoho_widget(url: &Url) -> bool {
    is_https(url) && host_lower(url).is_some_and(|h| host_in(&h, ZOHO_WIDGET_DOMAINS))
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

/// Zoho and Meet results found in Google Search open as normal client tabs, so
/// they use that client's signed-in profile instead of the Search store.
pub fn should_open_in_client_workspace(url: &Url) -> bool {
    is_zoho_family(url) || is_google_meet(url)
}

/// A frame's live security origin (scheme + host + port), as WebKit reports
/// it. Opaque origins (sandboxed frames, about:blank, data:) report an empty
/// scheme or host and are never trusted.
pub fn is_allowed_origin(scheme: &str, host: &str, port: i64, workspace: Workspace) -> bool {
    if scheme.is_empty() || host.is_empty() {
        return false;
    }
    let text = if port > 0 { format!("{scheme}://{host}:{port}/") } else { format!("{scheme}://{host}/") };
    parse(&text).is_some_and(|u| is_allowed_web(&u, workspace))
}

/// Sign-in pages. A tab that lands here (because a session ended) must not
/// overwrite where the user really was, or the place is lost after signing in.
pub fn is_auth_page(url: &Url) -> bool {
    if is_google_auth(url) {
        return true;
    }
    if !is_zoho_app(url) {
        return false;
    }
    let host = host_lower(url).unwrap_or_default();
    host.starts_with("accounts.")
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

/// Zoho's own sign-in host, where saved credentials may be filled in.
pub fn is_zoho_sign_in(url: &Url) -> bool {
    is_zoho_app(url) && host_lower(url).is_some_and(|h| h.starts_with("accounts."))
}

/// Whether a cookie for `domain` may be mirrored to the session backup. The
/// backup holds live credentials, so it is restricted to hosts this app signs
/// in to; third-party and tracking cookies are never stored by us.
pub fn is_persistable_cookie_domain(domain: &str) -> bool {
    let host = domain.trim_start_matches('.');
    if host.is_empty() || host.contains('/') || host.contains('@') {
        return false;
    }
    parse(&format!("https://{host}/")).is_some_and(|u| is_zoho_family(&u) || is_allowed_google_search(&u))
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
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && s.find(':').is_some_and(|i| s[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)))
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
        for s in ["https://zoho.com", "https://crm.zoho.com/crm/org1/tab/Home", "https://books.zoho.eu",
                  "https://crm.zoho.in", "https://accounts.zohocloud.ca", "https://crm.zoho.uk"] {
            assert!(is_allowed_web(&u(s), Workspace::Zoho), "{s}");
        }
    }

    #[test]
    fn crm_widgets_are_trusted() {
        let widget = u("https://123456-abcdef.zappsusercontent.com/appfiles/widget.html");
        assert!(is_zoho_widget(&widget));
        assert!(is_allowed_web(&widget, Workspace::Zoho));
        assert!(is_allowed_origin("https", "123456-abcdef.zappsusercontent.eu", 0, Workspace::Zoho));
        assert!(is_allowed_web(&u("https://static.zohocdn.com/x.js"), Workspace::Zoho));
        assert!(is_allowed_web(&u("https://log360cloud.manageengine.com/"), Workspace::Zoho));
        assert!(!is_allowed_web(&u("https://www.manageengine.com/"), Workspace::Zoho));
    }

    #[test]
    fn lookalike_hosts_and_userinfo_are_rejected() {
        for s in ["https://evilzoho.com", "https://zoho.com.evil.net", "https://crm.zoho.com@evil.net/",
                  "https://zappsusercontent.com.evil.net", "https://notzappsusercontent.com"] {
            assert!(!is_allowed_web(&u(s), Workspace::Zoho), "{s}");
        }
    }

    #[test]
    fn cleartext_is_never_trusted() {
        assert!(!is_allowed_web(&u("http://crm.zoho.com"), Workspace::Zoho));
        assert!(!is_allowed_web(&u("http://www.google.com"), Workspace::GoogleSearch));
        assert!(!is_allowed_origin("http", "crm.zoho.com", 0, Workspace::Zoho));
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
        assert!(!is_persistable_cookie_domain(""));
        assert!(!is_persistable_cookie_domain("evil.net/zoho.com"));
    }

    #[test]
    fn page_key_ignores_fragment_and_trailing_slash() {
        assert_eq!(page_key(&u("https://CRM.zoho.com/#x")), page_key(&u("https://crm.zoho.com")));
        assert_ne!(page_key(&u("https://crm.zoho.com/a")), page_key(&u("https://crm.zoho.com/b")));
    }
}
