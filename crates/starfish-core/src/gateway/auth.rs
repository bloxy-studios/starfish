//! Local key authentication for gateway requests.
//!
//! Accepts `Authorization: Bearer sk-…` (OpenAI clients) and `x-api-key: sk-…`
//! (Anthropic clients / Claude Code) on both surfaces. Keys are verified
//! against SHA-256 hashes in config; secrets never touch the logs.

use axum::http::HeaderMap;
use axum::response::Response;

use crate::config::AppConfig;
use crate::keys;
use crate::mapping::Surface;

use super::{anthropic, openai};

/// Who a request runs as, after key verification.
#[derive(Debug, Clone)]
pub struct RouteIdentity {
    pub account_id: String,
    pub key_id: Option<String>,
    pub key_hint: Option<String>,
    /// Key override, else the account default.
    pub default_agent_id: Option<String>,
    pub disabled_tools: Vec<String>,
}

/// Pull the presented key out of the headers.
pub fn presented_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = v.to_str() {
            let s = s.trim();
            if let Some(rest) = s
                .strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
            {
                return Some(rest.trim().to_string());
            }
        }
    }
    if let Some(v) = headers.get("x-api-key") {
        if let Ok(s) = v.to_str() {
            return Some(s.trim().to_string());
        }
    }
    None
}

/// Verify the request and resolve its identity. Errors are already shaped for
/// the given surface.
pub fn authenticate(
    cfg: &AppConfig,
    headers: &HeaderMap,
    surface: Surface,
) -> Result<RouteIdentity, Box<Response>> {
    let presented = presented_key(headers);

    let unauthorized = |msg: &str| -> Box<Response> {
        Box::new(match surface {
            Surface::Openai => {
                openai::error_response(axum::http::StatusCode::UNAUTHORIZED, "invalid_api_key", msg)
            }
            Surface::Anthropic => anthropic::error_response(
                axum::http::StatusCode::UNAUTHORIZED,
                "authentication_error",
                msg,
            ),
        })
    };

    match presented {
        Some(secret) => {
            for key in cfg.active_keys() {
                if keys::verify_key(&secret, &key.hash) {
                    let account = cfg.account(&key.account_id);
                    let default_agent_id = key
                        .default_agent_id
                        .clone()
                        .or_else(|| account.and_then(|a| a.default_agent_id.clone()));
                    return Ok(RouteIdentity {
                        account_id: key.account_id.clone(),
                        key_id: Some(key.id.clone()),
                        key_hint: Some(key.hint.clone()),
                        default_agent_id,
                        disabled_tools: key.disabled_tools.clone(),
                    });
                }
            }
            Err(unauthorized(
                "Invalid API key. Create or reveal keys in Starfish → Keys.",
            ))
        }
        None => {
            if cfg.server.allow_anonymous {
                if let Some(account) = cfg.accounts.first() {
                    return Ok(RouteIdentity {
                        account_id: account.id.clone(),
                        key_id: None,
                        key_hint: None,
                        default_agent_id: account.default_agent_id.clone(),
                        disabled_tools: vec![],
                    });
                }
                return Err(unauthorized(
                    "Anonymous mode is on but no Hyperagent account is signed in.",
                ));
            }
            Err(unauthorized(
                "Missing API key. Send it as 'Authorization: Bearer <key>' or 'x-api-key: <key>'.",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::testutil::mock_state;

    fn headers_with(name: &str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        h
    }

    #[tokio::test]
    async fn bearer_and_x_api_key_both_work() {
        let (state, secret) = mock_state();
        let cfg = state.config.read().await;

        let h = headers_with("authorization", &format!("Bearer {secret}"));
        let id = authenticate(&cfg, &h, Surface::Openai).unwrap();
        assert_eq!(id.account_id, "acct1");

        let h = headers_with("x-api-key", &secret);
        let id = authenticate(&cfg, &h, Surface::Anthropic).unwrap();
        assert_eq!(id.account_id, "acct1");
        assert_eq!(id.default_agent_id.as_deref(), Some("mock-researcher"));
    }

    #[tokio::test]
    async fn missing_or_wrong_key_is_rejected() {
        let (state, _secret) = mock_state();
        let cfg = state.config.read().await;

        assert!(authenticate(&cfg, &HeaderMap::new(), Surface::Openai).is_err());
        let h = headers_with("authorization", "Bearer sk-starfish-nope");
        assert!(authenticate(&cfg, &h, Surface::Openai).is_err());
    }

    #[tokio::test]
    async fn revoked_key_is_rejected_immediately() {
        let (state, secret) = mock_state();
        {
            let mut cfg = state.config.write().await;
            cfg.keys[0].revoked = true;
        }
        let cfg = state.config.read().await;
        let h = headers_with("authorization", &format!("Bearer {secret}"));
        assert!(authenticate(&cfg, &h, Surface::Openai).is_err());
    }

    #[tokio::test]
    async fn anonymous_requires_dev_toggle() {
        let (state, _) = mock_state();
        {
            let mut cfg = state.config.write().await;
            cfg.server.allow_anonymous = true;
        }
        let cfg = state.config.read().await;
        let id = authenticate(&cfg, &HeaderMap::new(), Surface::Openai).unwrap();
        assert_eq!(id.account_id, "acct1");
        assert!(id.key_id.is_none());
    }
}
