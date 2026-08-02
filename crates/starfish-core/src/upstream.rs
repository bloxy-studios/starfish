//! The upstream boundary: everything the gateway needs from Hyperagent,
//! behind a trait so tests and UI development can run fully offline against
//! [`MockUpstream`] (ROADMAP: "Mock upstream").

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use crate::config::AppConfig;
use crate::error::{CoreError, Result};
use crate::hyperagent::{self, AgentInfo, RunStatus, ThreadSnapshot};
use crate::mcp::McpClient;
use crate::oauth;
use crate::vault::{self, Vault};

#[async_trait]
pub trait Upstream: Send + Sync {
    async fn list_agents(&self, account_id: &str) -> Result<Vec<AgentInfo>>;
    /// Start a run: create a thread on `agent_id` with `message`; returns the thread id.
    async fn start_run(&self, account_id: &str, agent_id: &str, message: &str) -> Result<String>;
    /// Poll a running thread.
    async fn poll(&self, account_id: &str, thread_id: &str) -> Result<ThreadSnapshot>;
    /// Health probe: initialize MCP and count agents.
    async fn doctor(&self, account_id: &str) -> Result<DoctorReport>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorReport {
    pub mcp_reachable: bool,
    pub agents_count: usize,
    pub token_state: String,
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Real upstream
// ---------------------------------------------------------------------------

pub struct HyperagentUpstream {
    http: reqwest::Client,
    config: Arc<RwLock<AppConfig>>,
    vault: Arc<dyn Vault>,
    /// MCP client per account, invalidated when the access token changes.
    clients: Mutex<HashMap<String, (String, Arc<McpClient>)>>,
}

impl HyperagentUpstream {
    pub fn new(
        http: reqwest::Client,
        config: Arc<RwLock<AppConfig>>,
        vault: Arc<dyn Vault>,
    ) -> Self {
        Self {
            http,
            config,
            vault,
            clients: Mutex::new(HashMap::new()),
        }
    }

    async fn base_url(&self, account_id: &str) -> Result<String> {
        let cfg = self.config.read().await;
        cfg.account(account_id)
            .map(|a| a.base_url.clone())
            .ok_or_else(|| CoreError::AccountNotFound(account_id.to_string()))
    }

    /// Load tokens, refresh when close to expiry, persist rotations, and
    /// return a session-caching MCP client bound to a fresh access token.
    async fn client_for(&self, account_id: &str) -> Result<Arc<McpClient>> {
        let vault = self.vault.clone();
        let id = account_id.to_string();
        let bundle = tokio::task::spawn_blocking(move || vault::load_tokens(vault.as_ref(), &id))
            .await
            .map_err(|e| CoreError::Vault(format!("vault task failed: {e}")))??
            .ok_or_else(|| {
                CoreError::OAuth(format!(
                    "no stored credentials for account {account_id} — sign in again"
                ))
            })?;

        let bundle = if bundle.needs_refresh() {
            let refreshed = oauth::refresh(&self.http, &bundle).await?;
            let vault = self.vault.clone();
            let id = account_id.to_string();
            let to_store = refreshed.clone();
            tokio::task::spawn_blocking(move || {
                vault::store_tokens(vault.as_ref(), &id, &to_store)
            })
            .await
            .map_err(|e| CoreError::Vault(format!("vault task failed: {e}")))??;
            refreshed
        } else {
            bundle
        };

        let token_fingerprint = crate::keys::hash_key(&bundle.access_token);
        let mut clients = self.clients.lock().await;
        if let Some((fp, client)) = clients.get(account_id) {
            if *fp == token_fingerprint {
                return Ok(client.clone());
            }
        }
        let base = self.base_url(account_id).await?;
        let client = Arc::new(McpClient::new(
            self.http.clone(),
            &base,
            &bundle.access_token,
        ));
        clients.insert(account_id.to_string(), (token_fingerprint, client.clone()));
        Ok(client)
    }

    /// Describe the stored token without exposing it.
    async fn token_state(&self, account_id: &str) -> String {
        let vault = self.vault.clone();
        let id = account_id.to_string();
        let loaded =
            tokio::task::spawn_blocking(move || vault::load_tokens(vault.as_ref(), &id)).await;
        match loaded {
            Ok(Ok(Some(bundle))) => match bundle.expires_in_secs() {
                Some(secs) if secs <= 0 => "expired".into(),
                Some(secs) if secs <= 300 => "expiring".into(),
                Some(_) => "valid".into(),
                None => "valid (no expiry info)".into(),
            },
            Ok(Ok(None)) => "missing — sign in".into(),
            _ => "unreadable".into(),
        }
    }
}

#[async_trait]
impl Upstream for HyperagentUpstream {
    async fn list_agents(&self, account_id: &str) -> Result<Vec<AgentInfo>> {
        let client = self.client_for(account_id).await?;
        hyperagent::list_agents(&client).await
    }

    async fn start_run(&self, account_id: &str, agent_id: &str, message: &str) -> Result<String> {
        let client = self.client_for(account_id).await?;
        hyperagent::create_thread(&client, agent_id, message).await
    }

    async fn poll(&self, account_id: &str, thread_id: &str) -> Result<ThreadSnapshot> {
        let client = self.client_for(account_id).await?;
        hyperagent::get_thread(&client, thread_id).await
    }

    async fn doctor(&self, account_id: &str) -> Result<DoctorReport> {
        let token_state = self.token_state(account_id).await;
        match self.list_agents(account_id).await {
            Ok(agents) => Ok(DoctorReport {
                mcp_reachable: true,
                agents_count: agents.len(),
                token_state,
                detail: if agents.is_empty() {
                    Some(
                        "No named agents on this account. The MCP server can only start \
                         threads on named agents — create one in Hyperagent first."
                            .into(),
                    )
                } else {
                    None
                },
            }),
            Err(e) => Ok(DoctorReport {
                mcp_reachable: false,
                agents_count: 0,
                token_state,
                detail: Some(e.to_string()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Mock upstream (offline development + tests)
// ---------------------------------------------------------------------------

/// Deterministic fake: three agents, and runs that stream a canned reply over
/// a few polls. Enable in the app with `STARFISH_MOCK_UPSTREAM=1`.
pub struct MockUpstream {
    threads: Mutex<HashMap<String, MockThread>>,
    /// Number of polls before a run completes.
    pub polls_to_complete: u32,
}

struct MockThread {
    reply: String,
    polls: u32,
}

impl Default for MockUpstream {
    fn default() -> Self {
        Self::new()
    }
}

impl MockUpstream {
    pub fn new() -> Self {
        Self {
            threads: Mutex::new(HashMap::new()),
            polls_to_complete: 3,
        }
    }

    pub fn agents() -> Vec<AgentInfo> {
        vec![
            AgentInfo {
                id: "mock-researcher".into(),
                name: "Mock Researcher".into(),
                description: Some("Offline fake agent for development".into()),
            },
            AgentInfo {
                id: "mock-coder".into(),
                name: "Mock Coder".into(),
                description: Some("Writes pretend code".into()),
            },
        ]
    }
}

#[async_trait]
impl Upstream for MockUpstream {
    async fn list_agents(&self, _account_id: &str) -> Result<Vec<AgentInfo>> {
        Ok(Self::agents())
    }

    async fn start_run(&self, _account_id: &str, agent_id: &str, message: &str) -> Result<String> {
        let thread_id = format!("mock-thread-{}", uuid::Uuid::new_v4());
        let last_line = message.lines().last().unwrap_or(message);
        let reply = format!(
            "[{agent_id}] Mock reply. You said: {}",
            last_line.chars().take(200).collect::<String>()
        );
        self.threads
            .lock()
            .await
            .insert(thread_id.clone(), MockThread { reply, polls: 0 });
        Ok(thread_id)
    }

    async fn poll(&self, _account_id: &str, thread_id: &str) -> Result<ThreadSnapshot> {
        let mut threads = self.threads.lock().await;
        let t = threads
            .get_mut(thread_id)
            .ok_or_else(|| CoreError::Upstream("unknown mock thread".into()))?;
        t.polls += 1;
        let total = self.polls_to_complete.max(1);
        let frac = (t.polls.min(total) as usize * t.reply.len()) / total as usize;
        let mut cut = frac.min(t.reply.len());
        while !t.reply.is_char_boundary(cut) {
            cut -= 1;
        }
        Ok(ThreadSnapshot {
            thread_id: thread_id.to_string(),
            status: if t.polls >= total {
                RunStatus::Completed
            } else {
                RunStatus::Running
            },
            assistant_text: t.reply[..cut].to_string(),
            raw_status: Some(if t.polls >= total {
                "completed".into()
            } else {
                "running".into()
            }),
        })
    }

    async fn doctor(&self, _account_id: &str) -> Result<DoctorReport> {
        Ok(DoctorReport {
            mcp_reachable: true,
            agents_count: Self::agents().len(),
            token_state: "mock".into(),
            detail: Some("Mock upstream active (STARFISH_MOCK_UPSTREAM)".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_run_streams_and_completes() {
        let mock = MockUpstream::new();
        let tid = mock
            .start_run("acct", "mock-coder", "hello world")
            .await
            .unwrap();
        let mut last = String::new();
        let mut status = RunStatus::Running;
        for _ in 0..5 {
            let snap = mock.poll("acct", &tid).await.unwrap();
            assert!(snap.assistant_text.len() >= last.len());
            last = snap.assistant_text.clone();
            status = snap.status;
            if status == RunStatus::Completed {
                break;
            }
        }
        assert_eq!(status, RunStatus::Completed);
        assert!(last.contains("hello world"));
    }
}
