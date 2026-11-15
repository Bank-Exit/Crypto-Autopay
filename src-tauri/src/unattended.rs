//! Payments while AutoPay is locked: the wallet's own password (random, never the master
//! password) and the history key live in the OS keyring (Keychain, Windows Credential
//! Manager, Secret Service), so the scheduler can start at login with the interface locked.
//! Whoever can read the user's keyring can then spend up to the wallet caps: that is the
//! trade-off the setting states, and turning it off deletes the keyring entry.

use std::path::Path;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const SERVICE: &str = "com.libre-market.autopay";
const ACCOUNT: &str = "unattended-payments";
const SETTINGS_FILE: &str = "unattended.json";

#[derive(Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

/// On unless the user turned it off.
pub fn enabled(data_dir: &Path) -> bool {
    std::fs::read_to_string(data_dir.join(SETTINGS_FILE)).ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .is_none_or(|s| s.enabled)
}

pub fn set_enabled(data_dir: &Path, enabled: bool) -> Result<(), String> {
    let json = serde_json::to_string(&Settings { enabled }).map_err(|e| e.to_string())?;
    std::fs::write(data_dir.join(SETTINGS_FILE), json).map_err(|e| e.to_string())
}

pub struct Secrets {
    pub wallet_password: String,
    pub history_key: [u8; 32],
}

impl Drop for Secrets {
    fn drop(&mut self) {
        self.wallet_password.zeroize();
        self.history_key.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    w: String,
    h: String,
}

// The Secret Service backend blocks on its own async runtime: keep it off tokio threads.
fn on_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    std::thread::spawn(f).join().ok()
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
}

pub fn store(secrets: &Secrets) -> Result<(), String> {
    use base64::Engine;
    let mut json = serde_json::to_string(&Stored {
        w: secrets.wallet_password.clone(),
        h: base64::engine::general_purpose::STANDARD.encode(secrets.history_key),
    }).map_err(|e| e.to_string())?;
    let payload = json.clone();
    json.zeroize();
    on_thread(move || {
        let mut payload = payload;
        let r = entry().and_then(|e| e.set_password(&payload).map_err(|e| e.to_string()));
        payload.zeroize();
        r
    }).unwrap_or_else(|| Err("Keyring thread panicked".into()))
}

pub fn load() -> Result<Option<Secrets>, String> {
    use base64::Engine;
    let got = on_thread(|| entry().and_then(|e| match e.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    })).unwrap_or_else(|| Err("Keyring thread panicked".into()))?;
    let Some(mut raw) = got else { return Ok(None) };
    let parsed = serde_json::from_str::<Stored>(&raw);
    raw.zeroize();
    let mut stored = parsed.map_err(|_| "Keyring entry unreadable".to_string())?;
    let key = base64::engine::general_purpose::STANDARD.decode(&stored.h).ok()
        .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok());
    stored.h.zeroize();
    let Some(history_key) = key else {
        stored.w.zeroize();
        return Err("Keyring entry unreadable".into());
    };
    Ok(Some(Secrets { wallet_password: std::mem::take(&mut stored.w), history_key }))
}

pub fn forget() {
    let r = on_thread(|| entry().and_then(|e| match e.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }));
    if let Some(Err(e)) = r {
        log::warn!("[Unattended] Keyring entry not deleted: {}", e);
    }
}

#[cfg(test)]
mod tests {
    /// Needs a desktop session keyring: `cargo test -- --ignored keyring_roundtrip`.
    #[test]
    #[ignore]
    fn keyring_roundtrip() {
        let r = super::on_thread(|| {
            let e = keyring::Entry::new(super::SERVICE, "selftest").unwrap();
            e.set_password("probe").unwrap();
            let got = e.get_password().unwrap();
            e.delete_credential().unwrap();
            got
        });
        assert_eq!(r.as_deref(), Some("probe"));
    }
}
