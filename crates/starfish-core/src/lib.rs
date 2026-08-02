//! # starfish-core
//!
//! The engine behind Starfish (see `MISSION.md` at the repo root): a local
//! HTTP gateway that speaks the OpenAI and Anthropic wire formats on the
//! front and Hyperagent's MCP server on the back, plus everything that
//! supports it — OAuth 2.1 sign-in, OS-keychain credential storage, local
//! API keys, model→agent mapping, and a live request log.
//!
//! This crate is deliberately Tauri-free so it can be built, tested, and
//! reused without desktop toolchains; `src-tauri` is a thin shell over it.
//!
//! ```text
//! clients (Codex / Claude Code / SDKs)
//!    │  OpenAI + Anthropic wire formats on 127.0.0.1:8787
//!    ▼
//! gateway (axum) ── auth (local sk-starfish-… keys)
//!    │                 └─ mapping (model string → agent)
//!    ▼
//! upstream (trait) ── HyperagentUpstream ── mcp (JSON-RPC / Streamable HTTP)
//!    │                        │                └─ oauth (PKCE + refresh)
//!    └─ MockUpstream          └─ vault (OS keychain / 0600 file)
//! ```

pub mod config;
pub mod error;
pub mod estimate;
pub mod gateway;
pub mod hyperagent;
pub mod keys;
pub mod logbuf;
pub mod mapping;
pub mod mcp;
pub mod oauth;
pub mod upstream;
pub mod vault;

pub use error::{CoreError, Result};

/// Re-exported so the desktop shell names the exact same `reqwest` the core
/// was built with (no version-skew surprises).
pub use reqwest;

/// Default HTTP client used across the core: rustls, sane timeouts, no
/// system-proxy surprises for localhost work.
pub fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("starfish/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        // Generous overall timeout: MCP calls can legitimately take a while,
        // and SSE bodies stream for the duration of a poll cycle.
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("reqwest client")
}
