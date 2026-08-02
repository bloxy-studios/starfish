//! App configuration — everything that is *not* secret.
//!
//! Stored as JSON in the platform config dir (e.g. `~/.config/starfish` on
//! Linux, `~/Library/Application Support/starfish` on macOS). Token bundles and
//! key material never live here — they live in the vault (OS keychain).
//! Exporting this file is therefore always safe.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::keys::KeyRecord;
use crate::mapping::MappingRule;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8787;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1000;
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_UPSTREAM: &str = "https://hyperagent.com";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Bind host. Localhost by default; anything else requires the explicit
    /// opt-in flag below (MISSION.md §7).
    pub host: String,
    pub port: u16,
    /// How often to poll `get_thread` while a run is in flight.
    pub poll_interval_ms: u64,
    /// Hard cap on a single run.
    pub run_timeout_secs: u64,
    /// Dev-only escape hatch: accept requests without a key. Off by default;
    /// the UI shows a warning banner while this is on.
    pub allow_anonymous: bool,
    /// Must be set to serve on a non-loopback host.
    pub i_understand_lan_exposure_risks: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            run_timeout_secs: DEFAULT_RUN_TIMEOUT_SECS,
            allow_anonymous: false,
            i_understand_lan_exposure_risks: false,
        }
    }
}

/// A signed-in Hyperagent account (metadata only — tokens are in the vault).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub id: String,
    pub nickname: String,
    /// Upstream base URL (default `https://hyperagent.com`).
    #[serde(default = "default_upstream")]
    pub base_url: String,
    /// Best-effort identity (email/name) if the upstream ever exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Default agent for this account (used when a key has no override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

fn default_upstream() -> String {
    DEFAULT_UPSTREAM.into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// "auto" | "dark" | "light"
    pub theme: String,
    /// "error" | "warn" | "info" | "debug" | "trace"
    pub log_level: String,
    /// Launch the gateway server automatically when the app starts.
    pub autostart_server: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub accounts: Vec<AccountRecord>,
    pub keys: Vec<KeyRecord>,
    pub mappings: Vec<MappingRule>,
    pub settings: Settings,
    /// Set when the onboarding wizard has completed once.
    pub onboarded: bool,
}

impl AppConfig {
    pub fn account(&self, id: &str) -> Option<&AccountRecord> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn account_mut(&mut self, id: &str) -> Option<&mut AccountRecord> {
        self.accounts.iter_mut().find(|a| a.id == id)
    }

    /// Active (non-revoked) keys.
    pub fn active_keys(&self) -> impl Iterator<Item = &KeyRecord> {
        self.keys.iter().filter(|k| !k.revoked)
    }
}

/// Platform config directory for Starfish.
pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| CoreError::Config("no platform config directory".into()))?;
    Ok(base.join("starfish"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

/// Load config from disk (defaults when missing or unreadable-as-new).
pub fn load() -> Result<AppConfig> {
    let path = config_path()?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let cfg: AppConfig = serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::Config(format!("failed to parse {path:?}: {e}")))?;
            Ok(cfg)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(e) => Err(e.into()),
    }
}

/// Persist config atomically (write temp file, then rename).
pub fn save(cfg: &AppConfig) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = config_path()?;
    let tmp = dir.join(format!(".config.json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(cfg)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Validate a server config before applying / binding.
pub fn validate_server(cfg: &ServerConfig) -> Result<()> {
    let loopback = matches!(cfg.host.as_str(), "127.0.0.1" | "::1" | "localhost");
    if !loopback && !cfg.i_understand_lan_exposure_risks {
        return Err(CoreError::Config(
            "refusing to bind beyond localhost without the explicit LAN opt-in".into(),
        ));
    }
    if cfg.poll_interval_ms < 100 {
        return Err(CoreError::Config("poll interval must be ≥ 100ms".into()));
    }
    if cfg.run_timeout_secs < 5 {
        return Err(CoreError::Config("run timeout must be ≥ 5s".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 8787);
        assert!(!cfg.server.allow_anonymous);
        assert!(validate_server(&cfg.server).is_ok());
    }

    #[test]
    fn lan_bind_requires_opt_in() {
        let mut server = ServerConfig {
            host: "0.0.0.0".into(),
            ..Default::default()
        };
        assert!(validate_server(&server).is_err());
        server.i_understand_lan_exposure_risks = true;
        assert!(validate_server(&server).is_ok());
    }

    #[test]
    fn roundtrips_json() {
        let cfg = AppConfig::default();
        let s = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.server.port, cfg.server.port);
    }
}
