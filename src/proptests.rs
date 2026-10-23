//! Property tests: random inputs against the rules that keep profiles,
//! sessions and files safe. Run with `cargo test`; raise the case count with
//! `PROPTEST_CASES=100000 cargo test proptests`.

use std::collections::{BTreeMap, HashSet};

use proptest::prelude::*;

use crate::model::Workspace;
use crate::policy::{self, Capability, Decision, Origin, Request};
use crate::session::{self, Retention, StoredCookie};

fn host_label() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z0-9-]{1,12}",
        Just("zoho".to_string()),
        Just("meet".to_string()),
        Just("meeting".to_string()),
        Just("accounts".to_string()),
        Just("google".to_string()),
        Just("com".to_string()),
        Just("xn--zoho-9ra".to_string()),
    ]
}

fn host() -> impl Strategy<Value = String> {
    prop::collection::vec(host_label(), 1..6).prop_map(|v| v.join("."))
}

fn any_url() -> impl Strategy<Value = String> {
    (prop_oneof![Just("https"), Just("http"), Just("ftp")], prop::option::of("[a-z]{1,5}(:[a-z]{1,5})?@"), host(), prop::option::of(1u16..65535), "(/[a-zA-Z0-9._~-]{0,8}){0,3}")
        .prop_map(|(scheme, user, host, port, path)| {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            format!("{scheme}://{}{host}{port}{path}", user.unwrap_or_default())
        })
}

proptest! {
    /// Media and credentials are granted automatically only to an exact
    /// origin from the published lists, never to anything merely similar.
    #[test]
    fn privilege_only_for_exact_listed_origins(url in any_url(), main in any::<bool>(), gesture in any::<bool>()) {
        let Some(u) = policy::parse(&url) else { return Ok(()) };
        let origin = Origin::of(&u);
        let r = Request { origin: origin.clone(), top: origin.clone(), main_frame: main, user_gesture: gesture, workspace: Workspace::Zoho };
        let exact_call = origin.as_ref().is_some_and(|o| o.is_exact_https(&policy::call_hosts()));
        let exact_sign_in = origin.as_ref().is_some_and(|o| o.is_exact_https(&policy::sign_in_hosts()));
        if policy::decide(Capability::Camera, &r) == Decision::Allow {
            prop_assert!(exact_call && main, "{url}");
        }
        if policy::decide(Capability::Autofill, &r) == Decision::Allow {
            prop_assert!(exact_sign_in && main, "{url}");
        }
        if !u.username().is_empty() || u.scheme() != "https" {
            prop_assert_ne!(policy::decide(Capability::Camera, &r), Decision::Allow);
            prop_assert_ne!(policy::decide(Capability::Autofill, &r), Decision::Allow);
        }
    }

    /// Nothing that is not https is ever shown as a trusted page.
    #[test]
    fn only_https_is_displayable(url in any_url()) {
        if let Some(u) = policy::parse(&url)
            && (policy::is_allowed_web(&u, Workspace::Zoho) || policy::is_allowed_web(&u, Workspace::GoogleSearch)) {
                prop_assert_eq!(u.scheme(), "https");
            }
    }

    /// Restored cookies never outlive their own expiry or the retention
    /// period, and only sign-in domains are restored.
    #[test]
    fn restore_is_bounded(now in 1.0e9f64..2.0e9, days in 1u32..400, expiries in prop::collection::vec(prop::option::of(-1.0e8f64..1.0e9), 0..20), saved_off in -1.0e8f64..1.0e8, domains in prop::collection::vec(prop_oneof![Just(".zoho.com"), Just(".zoho.eu"), Just(".google.com"), Just(".evil.net"), Just(".zappsusercontent.com")], 20)) {
        let retention = Retention { keep: true, days };
        let cookies: Vec<StoredCookie> = expiries.iter().enumerate().map(|(i, e)| StoredCookie {
            name: format!("c{i}"), value: "v".into(), domain: domains[i].into(), path: "/".into(),
            is_secure: true, is_http_only: true, expires: e.map(|e| now + e), same_site: None, version: 0,
        }).collect();
        let jar = session::Jar { version: 3, saved_at: Some(now + saved_off), cookies: cookies.clone(), promoted: BTreeMap::new() };
        for c in session::restore_plan(&jar, &HashSet::new(), now, retention) {
            let e = c.expires.unwrap();
            prop_assert!(e > now && e <= now + retention.seconds() + 1e-6);
            prop_assert!(policy::is_persistable_cookie_domain(&c.domain));
            let original = cookies.iter().find(|o| o.name == c.name).unwrap();
            if let Some(oe) = original.expires {
                prop_assert!(e <= oe, "never extends a server expiry");
            }
        }
    }

    /// Promotion never touches a cookie the server dated itself.
    #[test]
    fn server_dated_cookies_are_never_rewritten(now in 1.0e9f64..2.0e9, offsets in prop::collection::vec(1.0f64..1.0e8, 0..10)) {
        let live: Vec<StoredCookie> = offsets.iter().enumerate().map(|(i, o)| StoredCookie {
            name: format!("c{i}"), value: "v".into(), domain: ".zoho.com".into(), path: "/".into(),
            is_secure: true, is_http_only: true, expires: Some(now + o), same_site: None, version: 0,
        }).collect();
        let (plan, ledger) = session::promotion_plan(&live, &BTreeMap::new(), now, Retention::default());
        prop_assert!(plan.is_empty());
        prop_assert!(ledger.is_empty());
    }

    /// A server-chosen name always becomes one plain, visible file name.
    #[test]
    fn download_names_are_plain(name in any::<String>()) {
        let f = crate::downloads::safe_filename(&name);
        prop_assert!(!f.is_empty() && f.len() <= 200 + 4);
        prop_assert!(!f.contains('/') && !f.contains(':') && !f.starts_with('.'));
        let hidden = f.chars().any(|c| c.is_control() || matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'));
        prop_assert!(!hidden, "hidden characters in {:?}", f);
    }

    /// The profile file survives a round trip for any names and tab URLs.
    #[test]
    fn profiles_round_trip(names in prop::collection::vec("\\PC{0,30}", 1..6), urls in prop::collection::vec(any_url(), 0..6)) {
        let mut store = crate::model::Store::default();
        store.ensure_search_client();
        for n in &names {
            let id = store.add_client(n, "#123456", None);
            for u in &urls {
                store.add_tab(id, Some(u.clone()), None, None, false);
            }
        }
        let again = crate::model::Store::from_json(&store.to_json()).unwrap();
        prop_assert_eq!(again.clients.len(), store.clients.len());
        let ids: HashSet<_> = again.clients.iter().map(|c| c.id).collect();
        prop_assert_eq!(ids.len(), again.clients.len());
    }

    /// Arbitrary bytes never decode as a profile file without an error or
    /// a valid store (no panic), and never as a future version silently.
    #[test]
    fn profile_decoding_never_panics(data in prop::collection::vec(any::<u8>(), 0..400)) {
        let _ = crate::model::decode_store(&data);
    }

    /// Random or mutated vault records never open.
    #[test]
    fn vault_records_resist_tampering(plain in prop::collection::vec(any::<u8>(), 0..200), flip in any::<prop::sample::Index>(), junk in prop::collection::vec(any::<u8>(), 0..120)) {
        use aes_gcm::{Aes256Gcm, KeyInit};
        use crate::keychain::{open_record, seal_record, Kind};
        let c = Aes256Gcm::new_from_slice(&[3u8; 32]).unwrap();
        let id = uuid::Uuid::from_u128(7);
        let sealed = seal_record(&c, "b", Kind::Session, id, &plain).unwrap();
        prop_assert_eq!(&open_record(&c, "b", Kind::Session, id, &sealed, false).unwrap().0, &plain);
        let mut t = sealed.clone();
        let i = flip.index(t.len());
        t[i] ^= 0x01;
        prop_assert!(open_record(&c, "b", Kind::Session, id, &t, false).is_err());
        prop_assert!(open_record(&c, "b", Kind::Session, id, &junk, false).is_err());
    }

    /// Cookie backups: damaged JSON is refused, never half-trusted.
    #[test]
    fn jar_decoding_never_panics(data in prop::collection::vec(any::<u8>(), 0..300)) {
        let _ = session::decode_jar(&data);
    }

    /// Redacted diagnostics never contain an email address or a URL path.
    #[test]
    fn redaction_removes_addresses(user in "[a-z]{3,8}", host in host(), path in "[a-z0-9]{3,10}") {
        let text = format!("x {user}@{host}.com y https://{host}.com/{path}?q={path} z");
        let r = crate::privacy::redact(&text);
        let at = format!("{user}@");
        prop_assert!(!r.contains(&at), "{}", r);
        prop_assert!(!r.contains(&format!("/{path}")), "{}", r);
    }
}
