//! Gemini API key in the OS credential store (Windows Credential Manager, Linux Secret Service —
//! GNOME Keyring / KWallet — via the `keyring` crate), never in `soul-settings.json`.
//!
//! The key is read once and cached: `load_settings` runs on hotkey paths and must not wait on a
//! credential call (a locked Linux keyring can show an unlock prompt). The first read waits at most
//! `FIRST_READ_TIMEOUT`; if it is still pending, callers get `Lookup::Pending` and the cache fills
//! when the read finishes. Without a usable store (no Secret Service, headless session) config.rs
//! keeps the key in the settings file as before and Settings says so (`storage()`).
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::sync::mpsc;
use std::time::Duration;

const SERVICE: &str = "com.oskar.soul";
const ACCOUNT: &str = "gemini-api-key";
const FIRST_READ_TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(String),
    Missing,
    /// First read still running (e.g. keyring unlock prompt) — try again later.
    Pending,
    /// No usable credential store; the message says why.
    Unavailable(String),
}

#[derive(Default)]
struct State {
    /// `Some` once a read or write finished: the key (`None` = no key stored).
    cached: Option<Option<String>>,
    /// Last store error (the store is treated as unavailable while set).
    unavailable: Option<String>,
    reading: bool,
}

static STATE: Lazy<Mutex<State>> = Lazy::new(|| Mutex::new(State::default()));

fn entry() -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(SERVICE, ACCOUNT)
}

fn read_store() -> Result<Option<String>, String> {
    match entry().and_then(|e| e.get_password()) {
        Ok(k) if !k.trim().is_empty() => Ok(Some(k.trim().to_string())),
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(describe(&e)),
    }
}

/// Error text without any secret (keyring errors never contain the password, but keep it short).
fn describe(e: &keyring::Error) -> String {
    match e {
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            format!("credential store unavailable ({e})")
        }
        other => other.to_string(),
    }
}

/// The stored key (cached after the first read).
pub fn load() -> Lookup {
    {
        let mut st = STATE.lock();
        if let Some(c) = &st.cached {
            return c.clone().map(Lookup::Found).unwrap_or(Lookup::Missing);
        }
        if let Some(e) = &st.unavailable {
            return Lookup::Unavailable(e.clone());
        }
        if st.reading {
            return Lookup::Pending;
        }
        st.reading = true;
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("soul-secrets".into())
        .spawn(move || {
            let result = read_store();
            {
                let mut st = STATE.lock();
                st.reading = false;
                match &result {
                    Ok(k) => st.cached = Some(k.clone()),
                    Err(e) => {
                        eprintln!("[soul][secrets] read failed: {e}");
                        st.unavailable = Some(e.clone());
                    }
                }
            }
            let _ = tx.send(result);
        });
    if let Err(e) = spawned {
        STATE.lock().reading = false;
        return Lookup::Unavailable(e.to_string());
    }
    match rx.recv_timeout(FIRST_READ_TIMEOUT) {
        Ok(Ok(Some(k))) => Lookup::Found(k),
        Ok(Ok(None)) => Lookup::Missing,
        Ok(Err(e)) => Lookup::Unavailable(e),
        Err(_) => {
            eprintln!("[soul][secrets] credential store is slow to answer (locked keyring?) — using no key for now");
            Lookup::Pending
        }
    }
}

/// Save (or, for an empty key, delete) the key. No-op when the cache already holds it.
pub fn store(key: &str) -> Result<(), String> {
    let key = key.trim();
    let want = (!key.is_empty()).then(|| key.to_string());
    if STATE.lock().cached.as_ref() == Some(&want) {
        return Ok(());
    }
    let res = entry().and_then(|e| match &want {
        Some(k) => e.set_password(k),
        None => match e.delete_credential() {
            Err(keyring::Error::NoEntry) => Ok(()),
            r => r,
        },
    });
    let mut st = STATE.lock();
    match res {
        Ok(()) => {
            st.cached = Some(want);
            st.unavailable = None;
            Ok(())
        }
        Err(e) => {
            let e = describe(&e);
            st.unavailable = Some(e.clone());
            Err(e)
        }
    }
}

/// The configured key if it is known (for redaction), without touching the store.
pub fn cached_key() -> Option<String> {
    STATE.lock().cached.clone().flatten()
}

/// Where the key lives, for Settings: `keychain` | `file` (store unavailable) | `pending`.
pub fn storage() -> &'static str {
    let st = STATE.lock();
    if st.unavailable.is_some() {
        "file"
    } else if st.cached.is_some() {
        "keychain"
    } else {
        "pending"
    }
}

/// Human-readable store name for logs / Settings.
pub fn store_name() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else {
        "Secret Service (GNOME Keyring / KWallet)"
    }
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    *STATE.lock() = State::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test touches the global mock store, so the steps run in order.
    #[test]
    fn mock_store_roundtrip_and_cache() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        reset_for_tests();
        // The mock store keeps nothing between Entry instances, so check the write path + cache.
        assert_eq!(storage(), "pending");
        store("  AQ.test-key-value  ").unwrap();
        assert_eq!(storage(), "keychain");
        assert_eq!(load(), Lookup::Found("AQ.test-key-value".into()));
        assert_eq!(cached_key().as_deref(), Some("AQ.test-key-value"));
        // Same value again: no store call (cache hit).
        store("AQ.test-key-value").unwrap();
        store("").unwrap();
        assert_eq!(load(), Lookup::Missing);
        assert_eq!(cached_key(), None);
    }

    /// Real OS store (Credential Manager / Secret Service) under a throwaway account; cleans up.
    ///   cargo test --manifest-path src-tauri/Cargo.toml --lib secrets::tests::os_store -- --ignored
    #[test]
    #[ignore]
    fn os_store_roundtrip() {
        let e = keyring::Entry::new(SERVICE, "soul-selftest").unwrap();
        e.set_password("AQ.selftest-not-a-real-key").unwrap();
        let again = keyring::Entry::new(SERVICE, "soul-selftest").unwrap();
        assert_eq!(again.get_password().unwrap(), "AQ.selftest-not-a-real-key");
        again.delete_credential().unwrap();
        assert!(matches!(
            keyring::Entry::new(SERVICE, "soul-selftest").unwrap().get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }
}
