//! Core error type shared across Starfish modules.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("config error: {0}")]
    Config(String),

    #[error("vault error: {0}")]
    Vault(String),

    #[error("oauth error: {0}")]
    OAuth(String),

    #[error("mcp error: {0}")]
    Mcp(String),

    #[error("upstream error: {0}")]
    Upstream(String),

    #[error("account not found: {0}")]
    AccountNotFound(String),

    #[error("no agent could be resolved for model '{0}'")]
    ModelUnresolved(String),

    #[error("authentication required")]
    Unauthorized,

    #[error("run timed out after {0}s")]
    RunTimeout(u64),

    #[error("server error: {0}")]
    Server(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;
