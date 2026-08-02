//! Local gateway API keys (`sk-starfish-…`).
//!
//! Key material is generated here and stored in the vault (OS keychain); the
//! config file only ever holds metadata plus a SHA-256 hash for verification,
//! so a config export can never leak a usable key.

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const KEY_PREFIX: &str = "sk-starfish-";

/// Metadata stored in the config file (no secret material).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRecord {
    pub id: String,
    pub name: String,
    /// SHA-256 hex digest of the full key string.
    pub hash: String,
    /// First 8 visible chars after the prefix, for display ("sk-starfish-ab12cd34…").
    pub hint: String,
    /// Account this key routes to.
    pub account_id: String,
    /// Optional default agent override for this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent_id: Option<String>,
    /// Tools the routed agent should not use for requests made with this key.
    /// (Forwarded as guidance once the tool bridge lands; recorded now.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub revoked: bool,
}

/// Generate a new key: returns (secret, hash, hint).
pub fn generate_key() -> (String, String, String) {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(bytes)
        .replace(['-', '_'], "0");
    let secret = format!("{KEY_PREFIX}{body}");
    let hash = hash_key(&secret);
    let hint = body.chars().take(8).collect::<String>();
    (secret, hash, hint)
}

/// Hash a full key string (SHA-256 hex).
pub fn hash_key(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex_encode(&hasher.finalize())
}

/// Constant-time-ish verification of a presented key against a stored hash.
pub fn verify_key(presented: &str, stored_hash: &str) -> bool {
    let presented_hash = hash_key(presented);
    // Compare hashes byte-wise without early exit.
    if presented_hash.len() != stored_hash.len() {
        return false;
    }
    presented_hash
        .bytes()
        .zip(stored_hash.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Redact a key for logs: keep prefix + first 4 chars.
pub fn redact_key(secret: &str) -> String {
    if let Some(rest) = secret.strip_prefix(KEY_PREFIX) {
        format!("{KEY_PREFIX}{}…", rest.chars().take(4).collect::<String>())
    } else {
        let head = secret.chars().take(6).collect::<String>();
        format!("{head}…")
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_verifies() {
        let (secret, hash, hint) = generate_key();
        assert!(secret.starts_with(KEY_PREFIX));
        assert!(verify_key(&secret, &hash));
        assert!(!verify_key("sk-starfish-wrong", &hash));
        assert_eq!(hint.len(), 8);
    }

    #[test]
    fn keys_are_unique() {
        let (a, _, _) = generate_key();
        let (b, _, _) = generate_key();
        assert_ne!(a, b);
    }

    #[test]
    fn redaction_hides_material() {
        let (secret, _, _) = generate_key();
        let red = redact_key(&secret);
        assert!(red.len() < secret.len());
        assert!(!secret.contains(&red)); // ellipsis makes it a non-substring
    }
}
