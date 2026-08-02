//! Secret storage.
//!
//! Primary backend: the OS keychain via the `keyring` crate (macOS Keychain,
//! Windows Credential Manager, Linux Secret Service). Fallback backend (for
//! environments with no keychain, or when built with
//! `--no-default-features`): a `0600` JSON file in the config dir, loudly
//! flagged so the UI can warn (MISSION.md §7: "if a token is ever exported,
//! write it 0600 and warn").
//!
//! Vault entries:
//!   - `account:{id}`  → serialized [`TokenBundle`]
//!   - `local-keys`    → JSON map of key id → key secret (for reveal/copy)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Keychain service name (also used as the file-vault namespace).
pub const SERVICE: &str = "com.bloxystudios.starfish";

/// The OAuth token bundle we persist per account (MISSION.md §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBundle {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds when the access token expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    pub client_id: String,
    pub token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl TokenBundle {
    /// Seconds until expiry (negative if already expired). `None` = unknown.
    pub fn expires_in_secs(&self) -> Option<i64> {
        self.expires_at
            .map(|at| at - chrono::Utc::now().timestamp())
    }

    /// True when the access token should be refreshed (≤30s of life left).
    pub fn needs_refresh(&self) -> bool {
        match self.expires_in_secs() {
            Some(secs) => secs <= 30,
            None => false,
        }
    }
}

/// Token/key storage backend.
pub trait Vault: Send + Sync {
    fn get(&self, entry: &str) -> Result<Option<String>>;
    fn set(&self, entry: &str, value: &str) -> Result<()>;
    fn delete(&self, entry: &str) -> Result<()>;
    /// Human-readable backend name ("os-keychain" | "file").
    fn backend(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// Typed helpers on top of the raw string vault
// ---------------------------------------------------------------------------

pub fn account_entry(account_id: &str) -> String {
    format!("account:{account_id}")
}

pub const LOCAL_KEYS_ENTRY: &str = "local-keys";

pub fn load_tokens(vault: &dyn Vault, account_id: &str) -> Result<Option<TokenBundle>> {
    match vault.get(&account_entry(account_id))? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw).map_err(|e| {
            CoreError::Vault(format!("corrupt token bundle for {account_id}: {e}"))
        })?)),
        None => Ok(None),
    }
}

pub fn store_tokens(vault: &dyn Vault, account_id: &str, bundle: &TokenBundle) -> Result<()> {
    vault.set(&account_entry(account_id), &serde_json::to_string(bundle)?)
}

pub fn delete_tokens(vault: &dyn Vault, account_id: &str) -> Result<()> {
    vault.delete(&account_entry(account_id))
}

pub fn load_key_secrets(vault: &dyn Vault) -> Result<HashMap<String, String>> {
    match vault.get(LOCAL_KEYS_ENTRY)? {
        Some(raw) => Ok(serde_json::from_str(&raw)
            .map_err(|e| CoreError::Vault(format!("corrupt local-keys entry: {e}")))?),
        None => Ok(HashMap::new()),
    }
}

pub fn store_key_secrets(vault: &dyn Vault, secrets: &HashMap<String, String>) -> Result<()> {
    vault.set(LOCAL_KEYS_ENTRY, &serde_json::to_string(secrets)?)
}

// ---------------------------------------------------------------------------
// OS keychain backend
// ---------------------------------------------------------------------------

#[cfg(feature = "os-keychain")]
pub struct KeyringVault;

#[cfg(feature = "os-keychain")]
impl KeyringVault {
    fn entry(name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, name)
            .map_err(|e| CoreError::Vault(format!("keychain entry error: {e}")))
    }
}

#[cfg(feature = "os-keychain")]
impl Vault for KeyringVault {
    fn get(&self, entry: &str) -> Result<Option<String>> {
        match Self::entry(entry)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CoreError::Vault(format!("keychain read error: {e}"))),
        }
    }

    fn set(&self, entry: &str, value: &str) -> Result<()> {
        Self::entry(entry)?
            .set_password(value)
            .map_err(|e| CoreError::Vault(format!("keychain write error: {e}")))
    }

    fn delete(&self, entry: &str) -> Result<()> {
        match Self::entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CoreError::Vault(format!("keychain delete error: {e}"))),
        }
    }

    fn backend(&self) -> &'static str {
        "os-keychain"
    }
}

// ---------------------------------------------------------------------------
// File fallback backend (0600, warned about in the UI)
// ---------------------------------------------------------------------------

pub struct FileVault {
    path: std::path::PathBuf,
    lock: std::sync::Mutex<()>,
}

impl FileVault {
    pub fn new() -> Result<Self> {
        let dir = crate::config::config_dir()?;
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("vault.json"),
            lock: std::sync::Mutex::new(()),
        })
    }

    #[cfg(test)]
    pub fn at(path: std::path::PathBuf) -> Self {
        Self {
            path,
            lock: std::sync::Mutex::new(()),
        }
    }

    fn read_all(&self) -> Result<HashMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn write_all(&self, map: &HashMap<String, String>) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(map)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl Vault for FileVault {
    fn get(&self, entry: &str) -> Result<Option<String>> {
        let _g = self.lock.lock().expect("vault lock");
        Ok(self.read_all()?.get(entry).cloned())
    }

    fn set(&self, entry: &str, value: &str) -> Result<()> {
        let _g = self.lock.lock().expect("vault lock");
        let mut map = self.read_all()?;
        map.insert(entry.to_string(), value.to_string());
        self.write_all(&map)
    }

    fn delete(&self, entry: &str) -> Result<()> {
        let _g = self.lock.lock().expect("vault lock");
        let mut map = self.read_all()?;
        map.remove(entry);
        self.write_all(&map)
    }

    fn backend(&self) -> &'static str {
        "file"
    }
}

/// Open the best available vault: keychain when the feature is on and the
/// backend responds; otherwise the 0600 file fallback.
pub fn open_default_vault() -> Result<Box<dyn Vault>> {
    #[cfg(feature = "os-keychain")]
    {
        let v = KeyringVault;
        // Probe the backend with a harmless read; some Linux setups have no
        // Secret Service running, in which case we fall back.
        match v.get("__starfish_probe__") {
            Ok(_) => return Ok(Box::new(v)),
            Err(e) => {
                tracing::warn!("OS keychain unavailable ({e}); falling back to file vault");
            }
        }
    }
    Ok(Box::new(FileVault::new()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_vault() -> FileVault {
        let dir = std::env::temp_dir().join(format!("starfish-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        FileVault::at(dir.join("vault.json"))
    }

    #[test]
    fn file_vault_roundtrip() {
        let v = tmp_vault();
        assert!(v.get("account:x").unwrap().is_none());
        v.set("account:x", "secret").unwrap();
        assert_eq!(v.get("account:x").unwrap().as_deref(), Some("secret"));
        v.delete("account:x").unwrap();
        assert!(v.get("account:x").unwrap().is_none());
    }

    #[test]
    fn token_bundle_helpers() {
        let v = tmp_vault();
        let bundle = TokenBundle {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(chrono::Utc::now().timestamp() + 3600),
            client_id: "cid".into(),
            token_endpoint: "https://hyperagent.com/oauth/token".into(),
            authorization_endpoint: None,
            scope: Some("threads:read".into()),
        };
        store_tokens(&v, "acct1", &bundle).unwrap();
        let back = load_tokens(&v, "acct1").unwrap().unwrap();
        assert_eq!(back.access_token, "at");
        assert!(!back.needs_refresh());

        let expired = TokenBundle {
            expires_at: Some(chrono::Utc::now().timestamp() + 10),
            ..bundle
        };
        assert!(expired.needs_refresh());
    }

    #[cfg(unix)]
    #[test]
    fn file_vault_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let v = tmp_vault();
        v.set("k", "v").unwrap();
        let mode = std::fs::metadata(&v.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
