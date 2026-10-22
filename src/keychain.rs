//! Encrypted storage for each profile's sign-in material.
//!
//! Two kinds of secret, both keyed by UUID and both removed with the profile:
//!   * each data store's cookie backup (what keeps a profile signed in), and
//!   * each profile's saved Zoho email/password (only ever typed back into
//!     Zoho's own sign-in form, if Zoho ends the session server-side).
//!
//! They are sealed with AES-256-GCM in `ZPortSwift/Vault/` using **one**
//! random key kept in the macOS login Keychain. One key rather than one
//! Keychain item per secret matters for a self-signed app: macOS ties each
//! item to the exact build that created it, so after an update every item
//! would prompt separately. With one key there is at most one prompt per
//! update (choose "Always Allow").
//!
//! The Swift build kept cookie backups as plaintext JSON; `session.rs` imports
//! those on first use and deletes the plaintext.

use std::cell::RefCell;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use security_framework::passwords;
use security_framework::random::SecRandom;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::util;

const KEY_SERVICE: &str = "com.zport.swift.vault";
const KEY_ACCOUNT: &str = "ZClients";

/// errSecItemNotFound: the key does not exist yet (first run).
const NOT_FOUND: i32 = -25300;

enum KeyState {
    Unknown,
    Ready(Box<Aes256Gcm>),
    /// The Keychain refused (e.g. the user chose Deny). Nothing is written
    /// this run, so existing sealed files are never replaced by unreadable
    /// ones; WebKit's own cookie store still keeps profiles signed in.
    Unavailable,
}

thread_local! {
    static KEY: RefCell<KeyState> = const { RefCell::new(KeyState::Unknown) };
}

fn test_mode() -> bool {
    util::is_test_build()
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    SecRandom::default().copy_bytes(&mut b).expect("system random source");
    b
}

fn load_key() -> KeyState {
    // Test runs keep their key next to their scratch data instead of the
    // Keychain, so unsigned debug builds never raise Keychain prompts.
    if test_mode() {
        let path = util::support_dir().join("test-vault.key");
        let key = match std::fs::read(&path) {
            Ok(k) if k.len() == 32 => k,
            _ => {
                let k = random::<32>().to_vec();
                let _ = util::write_atomic(&path, &k, 0o600);
                k
            }
        };
        return Aes256Gcm::new_from_slice(&key).map(|c| KeyState::Ready(Box::new(c))).unwrap_or(KeyState::Unavailable);
    }
    match passwords::get_generic_password(KEY_SERVICE, KEY_ACCOUNT) {
        Ok(key) if key.len() == 32 => {
            Aes256Gcm::new_from_slice(&key).map(|c| KeyState::Ready(Box::new(c))).unwrap_or(KeyState::Unavailable)
        }
        Err(e) if e.code() == NOT_FOUND => {
            let key = random::<32>();
            if passwords::set_generic_password(KEY_SERVICE, KEY_ACCOUNT, &key).is_ok() {
                Aes256Gcm::new_from_slice(&key).map(|c| KeyState::Ready(Box::new(c))).unwrap_or(KeyState::Unavailable)
            } else {
                KeyState::Unavailable
            }
        }
        other => {
            util::log(format!("Keychain key unavailable: {:?}", other.err()));
            KeyState::Unavailable
        }
    }
}

fn with_cipher<R>(f: impl FnOnce(&Aes256Gcm) -> R) -> Option<R> {
    KEY.with_borrow_mut(|k| {
        if matches!(k, KeyState::Unknown) {
            *k = load_key();
        }
        match k {
            KeyState::Ready(c) => Some(f(c)),
            _ => None,
        }
    })
}

fn path(kind: &str, id: Uuid) -> PathBuf {
    util::private_dir("Vault").join(format!("{kind}-{}.sealed", id.hyphenated().to_string().to_uppercase()))
}

fn seal(kind: &str, id: Uuid, plain: &[u8]) -> bool {
    let sealed = with_cipher(|c| {
        let nonce_bytes = random::<12>();
        let nonce = Nonce::try_from(&nonce_bytes[..]).expect("12-byte nonce");
        c.encrypt(&nonce, plain).ok().map(|ct| [nonce_bytes.as_slice(), &ct].concat())
    })
    .flatten();
    match sealed {
        Some(data) => match util::write_atomic(&path(kind, id), &data, 0o600) {
            Ok(()) => true,
            Err(e) => {
                util::log(format!("could not write sealed {kind}: {e}"));
                false
            }
        },
        None => false,
    }
}

fn open(kind: &str, id: Uuid) -> Option<Vec<u8>> {
    let data = std::fs::read(path(kind, id)).ok()?;
    if data.len() < 12 + 16 {
        return None;
    }
    with_cipher(|c| {
        let nonce = Nonce::try_from(&data[..12]).ok()?;
        c.decrypt(&nonce, &data[12..]).ok()
    })
    .flatten()
}

fn remove(kind: &str, id: Uuid) {
    let _ = std::fs::remove_file(path(kind, id));
}

pub fn read_session(id: Uuid) -> Option<Vec<u8>> {
    open("session", id)
}

pub fn write_session(id: Uuid, data: &[u8]) -> bool {
    seal("session", id, data)
}

pub fn delete_session(id: Uuid) {
    remove("session", id);
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct Login {
    pub email: String,
    pub password: String,
}

impl Login {
    pub fn is_empty(&self) -> bool {
        self.email.trim().is_empty() || self.password.is_empty()
    }
}

pub fn read_login(client: Uuid) -> Option<Login> {
    let data = open("login", client)?;
    serde_json::from_slice(&data).ok().filter(|l: &Login| !l.is_empty())
}

pub fn write_login(client: Uuid, login: &Login) -> bool {
    let Ok(data) = serde_json::to_vec(login) else { return false };
    seal("login", client, &data)
}

pub fn delete_login(client: Uuid) {
    remove("login", client);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_round_trip_and_tamper_detection() {
        let dir = std::env::temp_dir().join(format!("zc-vault-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test setup before any other access.
        unsafe { std::env::set_var("ZCLIENTS_SUPPORT_DIR", &dir) };
        let id = Uuid::new_v4();
        let login = Login { email: "a@b.co".into(), password: "pw".into() };
        assert!(write_login(id, &login));
        assert_eq!(read_login(id), Some(login));
        let raw = std::fs::read(path("login", id)).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("a@b.co"), "nothing readable on disk");
        let mut bad = raw.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        std::fs::write(path("login", id), bad).unwrap();
        assert_eq!(read_login(id), None, "tampered file is rejected");
        delete_login(id);
        assert!(!path("login", id).exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
