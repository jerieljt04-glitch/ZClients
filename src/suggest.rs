//! What the address bar does with text that is not a URL.

use url::Url;

use crate::model::ZOHO_APPS;

const IGNORED: &[&str] = &["zoho", "pricing", "price", "open", "website", "app", "login", "signin", "sign-in"];

pub fn google_search_url(query: &str) -> Option<Url> {
    let q = query.trim();
    if q.is_empty() {
        return Url::parse("https://www.google.com/").ok();
    }
    let mut u = Url::parse("https://www.google.com/search").ok()?;
    u.query_pairs_mut().append_pair("q", q);
    Some(u)
}

fn words(input: &str) -> Vec<String> {
    input.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

fn terms(words: &[String]) -> Vec<&str> {
    words.iter().map(String::as_str).filter(|w| !IGNORED.contains(w)).collect()
}

/// The launcher app the typed words name, e.g. "books" or "zoho crm".
fn matching_app(input: &str) -> Option<&'static crate::model::ZohoApp> {
    let w = words(input);
    let t = terms(&w);
    if t.is_empty() {
        return None;
    }
    ZOHO_APPS.iter().find(|app| {
        let searchable = format!("{} {}", app.name.to_lowercase(), app.slug);
        t.iter().all(|term| searchable.contains(term))
    })
}

/// Where Enter should go for typed words: a Zoho app's portal, Zoho's home
/// for "zoho", or None (the caller falls back to a Google search).
pub fn best_zoho_url(input: &str) -> Option<Url> {
    if input.contains("://") {
        return None;
    }
    let w = words(input);
    if w == ["zoho"] {
        return Url::parse("https://www.zoho.com/").ok();
    }
    let pricing = w.iter().any(|x| x == "pricing" || x == "price");
    let app = matching_app(input)?;
    if pricing { Url::parse(&format!("https://www.zoho.com/{}/pricing/", app.slug)).ok() } else { Url::parse(app.url).ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names_go_to_their_portal() {
        assert_eq!(best_zoho_url("books").unwrap().as_str(), "https://books.zoho.com/");
        assert_eq!(best_zoho_url("Zoho CRM").unwrap().as_str(), "https://crm.zoho.com/");
        assert_eq!(best_zoho_url("books pricing").unwrap().as_str(), "https://www.zoho.com/books/pricing/");
        assert_eq!(best_zoho_url("zoho").unwrap().as_str(), "https://www.zoho.com/");
        assert!(best_zoho_url("weather tomorrow").is_none());
    }

    #[test]
    fn search_queries_are_encoded() {
        let u = google_search_url("a&b c").unwrap();
        assert_eq!(u.as_str(), "https://www.google.com/search?q=a%26b+c");
    }
}
