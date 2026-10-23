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
//! Each record is bound to its app, kind and profile with AES-GCM associated
//! data (format 2, below), so records cannot be swapped between profiles or
//! kinds.
//!
//! The Swift build kept cookie backups as plaintext JSON; `session.rs` imports
//! those on first use and deletes the plaintext.

use std::cell::RefCell;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit, Payload};
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

/// Unit tests never touch the login Keychain, whatever order they run in.
fn test_mode() -> bool {
    cfg!(test) || util::is_test_build()
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
        Ok(key) if key.len() == 32 => Aes256Gcm::new_from_slice(&key).map(|c| KeyState::Ready(Box::new(c))).unwrap_or(KeyState::Unavailable),
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

// ---- sealed records -----------------------------------------------------
//
// Format 2 (this build):
//
//   "ZCV" 0x02 | kind (1 byte) | nonce (12) | AES-256-GCM ciphertext + tag
//
// The associated data — authenticated, not stored — is
//
//   "ZClients vault" 0 bundle-id 0 "2" 0 kind-name 0 PROFILE-UUID
//
// so a record only opens as the kind, for the profile/store, in the app it
// was written for. Copying a session backup over another profile's file, or
// over a saved login, fails authentication instead of being accepted.
//
// Format 1 (≤ 1.4.0) was nonce | ciphertext with no associated data. Those
// records are re-sealed as format 2 at launch (`migrate`); once every one
// has been, the vault is marked (`Vault/format` = 2) and format-1 files are
// refused from then on, so an old record cannot be planted back.

const MAGIC: &[u8; 3] = b"ZCV";
const FORMAT: u8 = 2;
const HEADER: usize = 3 + 1 + 1 + 12;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Session,
    Login,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Session => "session",
            Kind::Login => "login",
        }
    }
    fn byte(self) -> u8 {
        match self {
            Kind::Session => 1,
            Kind::Login => 2,
        }
    }
    fn from_name(n: &str) -> Option<Kind> {
        match n {
            "session" => Some(Kind::Session),
            "login" => Some(Kind::Login),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    Missing,
    /// Too short, wrong magic for format 2, or the header disagrees with
    /// where the file is.
    Malformed,
    /// Written by a newer ZClients. Never opened, never overwritten.
    FutureVersion,
    /// A format-1 record after the vault was marked migrated.
    LegacyRefused,
    /// Authentication failed: tampered, swapped, or the wrong key.
    Rejected,
    KeyUnavailable,
}

fn bundle_id() -> String {
    objc2_foundation::NSBundle::mainBundle().bundleIdentifier().map(|b| b.to_string()).unwrap_or_else(|| "zclients-unbundled".into())
}

fn aad(bundle: &str, kind: Kind, id: Uuid) -> Vec<u8> {
    let id = id.hyphenated().to_string().to_uppercase();
    [b"ZClients vault\0".as_slice(), bundle.as_bytes(), b"\0", FORMAT.to_string().as_bytes(), b"\0", kind.name().as_bytes(), b"\0", id.as_bytes()].concat()
}

fn vault_dir() -> PathBuf {
    util::private_dir("Vault")
}

fn file_in(dir: &std::path::Path, kind: Kind, id: Uuid) -> PathBuf {
    dir.join(format!("{}-{}.sealed", kind.name(), id.hyphenated().to_string().to_uppercase()))
}

fn path(kind: Kind, id: Uuid) -> PathBuf {
    file_in(&vault_dir(), kind, id)
}

fn marker(dir: &std::path::Path) -> PathBuf {
    dir.join("format")
}

fn migrated(dir: &std::path::Path) -> bool {
    std::fs::read_to_string(marker(dir)).is_ok_and(|s| s.trim().parse::<u8>().is_ok_and(|v| v >= FORMAT))
}

/// Seal `plain` as format 2 for (kind, id).
pub fn seal_record(c: &Aes256Gcm, bundle: &str, kind: Kind, id: Uuid, plain: &[u8]) -> Option<Vec<u8>> {
    let nonce_bytes = random::<12>();
    let nonce = Nonce::try_from(&nonce_bytes[..]).ok()?;
    let ct = c.encrypt(&nonce, Payload { msg: plain, aad: &aad(bundle, kind, id) }).ok()?;
    Some([MAGIC.as_slice(), &[FORMAT, kind.byte()], &nonce_bytes, &ct].concat())
}

/// Open a record read from the file for (kind, id). `allow_legacy` is false
/// once the vault has been migrated.
pub fn open_record(c: &Aes256Gcm, bundle: &str, kind: Kind, id: Uuid, data: &[u8], allow_legacy: bool) -> Result<(Vec<u8>, bool), OpenError> {
    if data.len() >= 4 && &data[..3] == MAGIC {
        let version = data[3];
        if version > FORMAT {
            return Err(OpenError::FutureVersion);
        }
        if version != FORMAT || data.len() < HEADER + 16 || data[4] != kind.byte() {
            return Err(OpenError::Malformed);
        }
        let nonce = Nonce::try_from(&data[5..HEADER]).map_err(|_| OpenError::Malformed)?;
        return c.decrypt(&nonce, Payload { msg: &data[HEADER..], aad: &aad(bundle, kind, id) }).map(|p| (p, false)).map_err(|_| OpenError::Rejected);
    }
    if !allow_legacy {
        return Err(OpenError::LegacyRefused);
    }
    if data.len() < 12 + 16 {
        return Err(OpenError::Malformed);
    }
    let nonce = Nonce::try_from(&data[..12]).map_err(|_| OpenError::Malformed)?;
    c.decrypt(&nonce, &data[12..]).map(|p| (p, true)).map_err(|_| OpenError::Rejected)
}

/// The version byte of an existing record, if it is a format-2-style file.
fn existing_version(path: &std::path::Path) -> Option<u8> {
    let mut head = [0u8; 4];
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    f.read_exact(&mut head).ok()?;
    (&head[..3] == MAGIC).then_some(head[3])
}

fn seal_to(dir: &std::path::Path, kind: Kind, id: Uuid, plain: &[u8]) -> bool {
    let target = file_in(dir, kind, id);
    if existing_version(&target).is_some_and(|v| v > FORMAT) {
        util::log(format!("not replacing a {} record written by a newer ZClients", kind.name()));
        return false;
    }
    match with_cipher(|c| seal_record(c, &bundle_id(), kind, id, plain)).flatten() {
        Some(data) => match util::write_atomic(&target, &data, 0o600) {
            Ok(()) => true,
            Err(e) => {
                util::log(format!("could not write sealed {}: {e}", kind.name()));
                false
            }
        },
        None => false,
    }
}

fn open_from(dir: &std::path::Path, kind: Kind, id: Uuid) -> Result<Vec<u8>, OpenError> {
    let file = file_in(dir, kind, id);
    let data = std::fs::read(&file).map_err(|_| OpenError::Missing)?;
    let allow_legacy = !migrated(dir);
    let (plain, legacy) = with_cipher(|c| open_record(c, &bundle_id(), kind, id, &data, allow_legacy)).ok_or(OpenError::KeyUnavailable)??;
    if legacy {
        // Upgrade in place; the old bytes stay until the new file is
        // completely on disk (write_atomic), so a crash keeps one or the other.
        seal_to(dir, kind, id, &plain);
    }
    Ok(plain)
}

fn open(kind: Kind, id: Uuid) -> Option<Vec<u8>> {
    match open_from(&vault_dir(), kind, id) {
        Ok(p) => Some(p),
        Err(OpenError::Missing) => None,
        Err(e) => {
            util::log(format!("sealed {} record refused: {e:?}", kind.name()));
            None
        }
    }
}

/// Re-seal every format-1 record as format 2, then mark the vault so
/// format-1 files are refused from now on. Records that cannot be read (no
/// key this run) are left alone and the marker is not written, so the next
/// launch tries again.
pub fn migrate() -> usize {
    migrate_in(&vault_dir())
}

fn migrate_in(dir: &std::path::Path) -> usize {
    if migrated(dir) {
        return 0;
    }
    let mut upgraded = 0;
    let mut pending = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".sealed") else { continue };
            let Some((kind, id)) = stem.split_once('-').and_then(|(k, rest)| Some((Kind::from_name(k)?, Uuid::parse_str(rest).ok()?))) else { continue };
            if existing_version(&e.path()).is_some() {
                continue;
            }
            match open_from(dir, kind, id) {
                Ok(_) if existing_version(&e.path()) == Some(FORMAT) => upgraded += 1,
                Err(OpenError::Rejected) | Err(OpenError::Malformed) => {
                    // Unreadable with the vault's own key: not a record we
                    // wrote. Kept aside, never trusted.
                    let _ = std::fs::rename(e.path(), e.path().with_extension("rejected"));
                }
                _ => pending += 1,
            }
        }
    }
    if pending == 0 && util::write_atomic(&marker(dir), FORMAT.to_string().as_bytes(), 0o600).is_err() {
        util::log("could not mark the vault as migrated");
    }
    upgraded
}

pub fn read_session(id: Uuid) -> Option<Vec<u8>> {
    open(Kind::Session, id)
}

pub fn write_session(id: Uuid, data: &[u8]) -> bool {
    seal_to(&vault_dir(), Kind::Session, id, data)
}

/// Seal now, write on the background writer (routine backups while the app
/// is in use). False only if sealing failed.
pub fn write_session_soon(id: Uuid, data: &[u8]) -> bool {
    match with_cipher(|c| seal_record(c, &bundle_id(), Kind::Session, id, data)).flatten() {
        Some(s) => {
            util::write_soon(path(Kind::Session, id), s, 0o600);
            true
        }
        None => false,
    }
}

/// Queued behind any pending backup write, so a deleted profile's backup
/// can never be written back after it was removed.
pub fn delete_session(id: Uuid) {
    util::remove_soon(path(Kind::Session, id));
}

pub fn session_exists(id: Uuid) -> bool {
    path(Kind::Session, id).exists()
}

pub fn login_exists(id: Uuid) -> bool {
    path(Kind::Login, id).exists()
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

impl Drop for Login {
    fn drop(&mut self) {
        let pw = std::mem::take(&mut self.password);
        let mut bytes = pw.into_bytes();
        wipe(&mut bytes);
    }
}

pub fn read_login(client: Uuid) -> Option<Login> {
    let mut data = open(Kind::Login, client)?;
    let login = serde_json::from_slice(&data).ok().filter(|l: &Login| !l.is_empty());
    wipe(&mut data);
    login
}

/// Only whether a login is saved, and for which address: what the UI shows.
pub fn saved_login_email(client: Uuid) -> Option<String> {
    read_login(client).map(|l| l.email.clone())
}

pub fn write_login(client: Uuid, login: &Login) -> bool {
    let Ok(mut data) = serde_json::to_vec(login) else { return false };
    let ok = seal_to(&vault_dir(), Kind::Login, client, &data);
    wipe(&mut data);
    ok
}

/// Remove the saved login. True when no login file remains.
pub fn delete_login(client: Uuid) -> bool {
    let p = path(Kind::Login, client);
    match std::fs::remove_file(&p) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            util::log(format!("could not remove a saved login: {e}"));
            false
        }
    }
}

/// Overwrite secret bytes before they are freed.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: a valid, aligned, exclusive reference.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> Aes256Gcm {
        Aes256Gcm::new_from_slice(&[7u8; 32]).unwrap()
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("zc-vault-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn setup() {
        // SAFETY: every test sets the same value; set before first use.
        unsafe { std::env::set_var("ZCLIENTS_SUPPORT_DIR", std::env::temp_dir().join("zc-vault-support")) };
    }

    #[test]
    fn records_open_only_for_their_kind_profile_and_app() {
        let c = cipher();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let sealed = seal_record(&c, "com.zport.swift", Kind::Session, a, b"cookies").unwrap();
        assert_eq!(open_record(&c, "com.zport.swift", Kind::Session, a, &sealed, false).unwrap().0, b"cookies");
        assert_eq!(open_record(&c, "com.zport.swift", Kind::Session, b, &sealed, false), Err(OpenError::Rejected), "swapped to another profile");
        assert_eq!(open_record(&c, "com.zport.swift", Kind::Login, a, &sealed, false), Err(OpenError::Malformed), "swapped to another kind");
        let mut relabelled = sealed.clone();
        relabelled[4] = Kind::Login.byte();
        assert_eq!(open_record(&c, "com.zport.swift", Kind::Login, a, &relabelled, false), Err(OpenError::Rejected), "kind byte forged");
        assert_eq!(open_record(&c, "com.zport.swift.test", Kind::Session, a, &sealed, false), Err(OpenError::Rejected), "other app");
    }

    #[test]
    fn truncation_tampering_and_future_versions_are_refused() {
        let c = cipher();
        let id = Uuid::new_v4();
        let sealed = seal_record(&c, "x", Kind::Login, id, b"secret").unwrap();
        for cut in [0, 3, 4, HEADER, sealed.len() - 1] {
            assert!(open_record(&c, "x", Kind::Login, id, &sealed[..cut], true).is_err(), "cut at {cut}");
        }
        for i in [5, HEADER, sealed.len() - 1] {
            let mut t = sealed.clone();
            t[i] ^= 1;
            assert_eq!(open_record(&c, "x", Kind::Login, id, &t, false), Err(OpenError::Rejected), "byte {i}");
        }
        let mut future = sealed.clone();
        future[3] = FORMAT + 1;
        assert_eq!(open_record(&c, "x", Kind::Login, id, &future, true), Err(OpenError::FutureVersion));
    }

    #[test]
    fn legacy_records_open_until_the_vault_is_migrated() {
        let c = cipher();
        let id = Uuid::new_v4();
        let nonce = [1u8; 12];
        let legacy = [nonce.as_slice(), &c.encrypt(&Nonce::try_from(&nonce[..]).unwrap(), b"old".as_slice()).unwrap()].concat();
        assert_eq!(open_record(&c, "x", Kind::Session, id, &legacy, true), Ok((b"old".to_vec(), true)));
        assert_eq!(open_record(&c, "x", Kind::Session, id, &legacy, false), Err(OpenError::LegacyRefused));
    }

    #[test]
    fn migration_reseals_marks_and_then_refuses_replanted_legacy_files() {
        setup();
        let dir = scratch();
        let id = Uuid::new_v4();
        let legacy = with_cipher(|c| {
            let nonce = [9u8; 12];
            [nonce.as_slice(), &c.encrypt(&Nonce::try_from(&nonce[..]).unwrap(), b"jar".as_slice()).unwrap()].concat()
        })
        .unwrap();
        std::fs::write(file_in(&dir, Kind::Session, id), &legacy).unwrap();
        assert_eq!(migrate_in(&dir), 1);
        assert!(migrated(&dir));
        assert_eq!(existing_version(&file_in(&dir, Kind::Session, id)), Some(FORMAT));
        assert_eq!(open_from(&dir, Kind::Session, id).unwrap(), b"jar");
        // Rollback attack: the old file is put back after migration.
        std::fs::write(file_in(&dir, Kind::Session, id), &legacy).unwrap();
        assert_eq!(open_from(&dir, Kind::Session, id), Err(OpenError::LegacyRefused));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_moved_between_profiles_or_kinds_do_not_open() {
        setup();
        let dir = scratch();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(seal_to(&dir, Kind::Login, a, b"{\"email\":\"a@b.co\",\"password\":\"pw\"}"));
        std::fs::copy(file_in(&dir, Kind::Login, a), file_in(&dir, Kind::Login, b)).unwrap();
        std::fs::copy(file_in(&dir, Kind::Login, a), file_in(&dir, Kind::Session, a)).unwrap();
        std::fs::write(marker(&dir), b"2").unwrap();
        assert!(open_from(&dir, Kind::Login, a).is_ok());
        assert_eq!(open_from(&dir, Kind::Login, b), Err(OpenError::Rejected));
        assert_eq!(open_from(&dir, Kind::Session, a), Err(OpenError::Malformed));
        let raw = std::fs::read(file_in(&dir, Kind::Login, a)).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("a@b.co"), "nothing readable on disk");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_interrupted_write_keeps_the_previous_record() {
        setup();
        let dir = scratch();
        let id = Uuid::new_v4();
        assert!(seal_to(&dir, Kind::Session, id, b"first"));
        util::FAIL_NEXT_WRITE.set(true);
        assert!(!seal_to(&dir, Kind::Session, id, b"second"));
        assert_eq!(open_from(&dir, Kind::Session, id).unwrap(), b"first");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn records_from_a_newer_version_are_never_overwritten() {
        setup();
        let dir = scratch();
        let id = Uuid::new_v4();
        std::fs::write(file_in(&dir, Kind::Session, id), [b"ZCV".as_slice(), &[FORMAT + 1], &[0u8; 40]].concat()).unwrap();
        assert!(!seal_to(&dir, Kind::Session, id, b"older app"));
        assert_eq!(open_from(&dir, Kind::Session, id), Err(OpenError::FutureVersion));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wipe_zeroes() {
        let mut b = *b"secret";
        wipe(&mut b);
        assert_eq!(b, [0u8; 6]);
    }
}
