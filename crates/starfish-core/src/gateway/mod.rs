//! The local gateway server: one axum listener, two API surfaces.
//!
//! - OpenAI surface:    `/v1/models`, `/v1/chat/completions`, `/v1/responses…`
//! - Anthropic surface: `/v1/messages`, `/v1/messages/count_tokens`
//! - `/healthz` for quick liveness checks.

pub mod anthropic;
pub mod auth;
pub mod common;
pub mod openai;
pub mod sse;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::routing::{get, post};
use axum::Router;
use tokio::sync::{oneshot, RwLock};

use crate::config::AppConfig;
use crate::error::{CoreError, Result};
use crate::hyperagent::AgentInfo;
use crate::logbuf::LogBuffer;
use crate::upstream::Upstream;

pub const AGENTS_CACHE_TTL: Duration = Duration::from_secs(60);

/// Shared state for all gateway handlers (and reused by Tauri commands).
pub struct GatewayState {
    pub config: Arc<RwLock<AppConfig>>,
    pub upstream: Arc<dyn Upstream>,
    pub log: Arc<LogBuffer>,
    agents_cache: RwLock<HashMap<String, (Instant, Vec<AgentInfo>)>>,
    pub responses: openai::ResponsesRegistry,
}

impl GatewayState {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        upstream: Arc<dyn Upstream>,
        log: Arc<LogBuffer>,
    ) -> Self {
        Self {
            config,
            upstream,
            log,
            agents_cache: RwLock::new(HashMap::new()),
            responses: openai::ResponsesRegistry::default(),
        }
    }

    /// Agents for an account, with a small TTL cache.
    pub async fn agents(&self, account_id: &str, force_refresh: bool) -> Result<Vec<AgentInfo>> {
        if !force_refresh {
            let cache = self.agents_cache.read().await;
            if let Some((at, agents)) = cache.get(account_id) {
                if at.elapsed() < AGENTS_CACHE_TTL {
                    return Ok(agents.clone());
                }
            }
        }
        let agents = self.upstream.list_agents(account_id).await?;
        self.agents_cache
            .write()
            .await
            .insert(account_id.to_string(), (Instant::now(), agents.clone()));
        Ok(agents)
    }

    pub async fn invalidate_agents(&self, account_id: &str) {
        self.agents_cache.write().await.remove(account_id);
    }
}

pub fn build_router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        // OpenAI surface
        .route("/v1/models", get(openai::list_models))
        .route("/v1/models/{id}", get(openai::get_model))
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/responses", post(openai::create_response))
        .route("/v1/responses/{id}", get(openai::get_response))
        .route("/v1/responses/{id}/cancel", post(openai::cancel_response))
        .route(
            "/v1/responses/{id}/input_items",
            get(openai::list_response_input_items),
        )
        // Anthropic surface
        .route("/v1/messages", post(anthropic::messages))
        .route("/v1/messages/count_tokens", post(anthropic::count_tokens))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// A running server: keep the handle to stop it.
pub struct ServerHandle {
    pub addr: SocketAddr,
    pub started_at: chrono::DateTime<chrono::Utc>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

/// Bind and serve. Validates the configured host/port first.
pub async fn start(state: Arc<GatewayState>) -> Result<ServerHandle> {
    let (host, port) = {
        let cfg = state.config.read().await;
        crate::config::validate_server(&cfg.server)?;
        (cfg.server.host.clone(), cfg.server.port)
    };
    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|e| CoreError::Server(format!("could not bind {host}:{port}: {e}")))?;
    let addr = listener.local_addr()?;
    let router = build_router(state);
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async {
            let _ = rx.await;
        });
        if let Err(e) = serve.await {
            tracing::error!("gateway server error: {e}");
        }
    });
    tracing::info!("gateway listening on http://{addr}");
    Ok(ServerHandle {
        addr,
        started_at: chrono::Utc::now(),
        shutdown: Some(tx),
        task,
    })
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::config::{AccountRecord, AppConfig};
    use crate::keys;
    use crate::upstream::MockUpstream;

    /// A GatewayState wired to the mock upstream with one account + one key.
    /// Returns (state, plaintext key).
    pub fn mock_state() -> (Arc<GatewayState>, String) {
        let mut cfg = AppConfig::default();
        cfg.accounts.push(AccountRecord {
            id: "acct1".into(),
            nickname: "Test".into(),
            base_url: "https://hyperagent.com".into(),
            identity: None,
            default_agent_id: Some("mock-researcher".into()),
            created_at: chrono::Utc::now(),
        });
        let (secret, hash, hint) = keys::generate_key();
        cfg.keys.push(keys::KeyRecord {
            id: "key1".into(),
            name: "test key".into(),
            hash,
            hint,
            account_id: "acct1".into(),
            default_agent_id: None,
            disabled_tools: vec![],
            created_at: chrono::Utc::now(),
            last_used_at: None,
            revoked: false,
        });
        // Fast polls for tests.
        cfg.server.poll_interval_ms = 100;
        let state = Arc::new(GatewayState::new(
            Arc::new(RwLock::new(cfg)),
            Arc::new(MockUpstream::new()),
            Arc::new(LogBuffer::new()),
        ));
        (state, secret)
    }
}
