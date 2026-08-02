//! Shared request machinery: transcript flattening and the poll-diff run loop.
//!
//! Stateless by design (MISSION.md §4): each API call carries its own context,
//! which we flatten into one self-contained Hyperagent message; the reply is
//! produced by polling `get_thread` and diffing the assistant text.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{CoreError, Result};
use crate::hyperagent::RunStatus;
use crate::logbuf::PollEvent;

use super::auth::RouteIdentity;
use super::GatewayState;

/// A flattened conversation ready to become one Hyperagent message.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    pub system: Option<String>,
    /// (role, text) in order. Roles: "user" | "assistant" | anything else verbatim.
    pub turns: Vec<(String, String)>,
}

impl Transcript {
    pub fn push(&mut self, role: impl Into<String>, text: impl Into<String>) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.turns.push((role.into(), text));
    }

    /// All text parts (for token estimation).
    pub fn parts(&self) -> Vec<&str> {
        let mut v: Vec<&str> = Vec::new();
        if let Some(s) = &self.system {
            v.push(s);
        }
        v.extend(self.turns.iter().map(|(_, t)| t.as_str()));
        v
    }

    /// Render the single message we send upstream.
    ///
    /// Fast path: a lone user turn with no system prompt is sent verbatim so
    /// simple calls hit the agent exactly as typed. Anything richer is wrapped
    /// in a compact transcript the agent is asked to continue.
    pub fn render(&self) -> String {
        if self.system.is_none() && self.turns.len() == 1 && self.turns[0].0 == "user" {
            return self.turns[0].1.clone();
        }
        let mut out = String::new();
        out.push_str(
            "You are answering through an API adapter. Below is the client conversation so \
             far. Reply as the assistant to the final user turn. Output ONLY the assistant \
             reply — no transcript markers, no commentary about this format.\n",
        );
        if let Some(system) = &self.system {
            out.push_str("\n[system]\n");
            out.push_str(system);
            out.push('\n');
        }
        for (role, text) in &self.turns {
            out.push_str(&format!("\n[{role}]\n{text}\n"));
        }
        out
    }
}

/// Options for one run.
pub struct RunOptions {
    /// Overrides config when set (already resolved by the handler).
    pub poll_interval: Duration,
    pub timeout: Duration,
    /// Cooperative cancellation (used by `POST /v1/responses/{id}/cancel`).
    pub cancel: Option<Arc<AtomicBool>>,
}

impl RunOptions {
    pub async fn from_state(state: &GatewayState) -> Self {
        let cfg = state.config.read().await;
        Self {
            poll_interval: Duration::from_millis(cfg.server.poll_interval_ms),
            timeout: Duration::from_secs(cfg.server.run_timeout_secs),
            cancel: None,
        }
    }
}

/// How a run ended (errors are returned as `Err` instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    Cancelled,
}

pub struct TurnOutcome {
    pub end: TurnEnd,
    pub text: String,
    pub thread_id: String,
    pub polls: Vec<PollEvent>,
}

/// If the upstream never reports a status, declare the run finished after the
/// assistant text has been non-empty and unchanged for this many polls.
const STABLE_POLLS_FALLBACK: u32 = 5;

/// Start a thread and poll it to completion, invoking `on_delta` with each new
/// suffix of assistant text (the emulated-streaming primitive).
pub async fn run_turn(
    state: &GatewayState,
    identity: &RouteIdentity,
    agent_id: &str,
    prompt: &str,
    opts: &RunOptions,
    mut on_delta: impl FnMut(&str),
) -> Result<TurnOutcome> {
    let started = Instant::now();
    let mut polls: Vec<PollEvent> = Vec::new();

    let thread_id = state
        .upstream
        .start_run(&identity.account_id, agent_id, prompt)
        .await?;
    polls.push(PollEvent {
        at_ms: started.elapsed().as_millis() as u64,
        note: format!("thread created: {thread_id}"),
    });

    let mut previous = String::new();
    let mut stable = 0u32;

    loop {
        if let Some(cancel) = &opts.cancel {
            if cancel.load(Ordering::Relaxed) {
                polls.push(PollEvent {
                    at_ms: started.elapsed().as_millis() as u64,
                    note: "cancelled by client".into(),
                });
                return Ok(TurnOutcome {
                    end: TurnEnd::Cancelled,
                    text: previous,
                    thread_id,
                    polls,
                });
            }
        }
        if started.elapsed() > opts.timeout {
            return Err(CoreError::RunTimeout(opts.timeout.as_secs()));
        }

        tokio::time::sleep(opts.poll_interval).await;

        let snap = state
            .upstream
            .poll(&identity.account_id, &thread_id)
            .await?;

        // Emit the new suffix, if any.
        if snap.assistant_text.len() > previous.len() && snap.assistant_text.starts_with(&previous)
        {
            let delta = snap.assistant_text[previous.len()..].to_string();
            on_delta(&delta);
            previous = snap.assistant_text.clone();
            stable = 0;
        } else if snap.assistant_text != previous && !snap.assistant_text.is_empty() {
            // Upstream rewrote the message (rare). Restart the diff from the
            // new text; emit it whole so the client is not left behind.
            on_delta(&snap.assistant_text);
            previous = snap.assistant_text.clone();
            stable = 0;
        } else {
            stable += 1;
        }

        polls.push(PollEvent {
            at_ms: started.elapsed().as_millis() as u64,
            note: format!(
                "poll: status={} chars={}",
                snap.raw_status.as_deref().unwrap_or("?"),
                previous.len()
            ),
        });

        match snap.status {
            RunStatus::Completed => {
                return Ok(TurnOutcome {
                    end: TurnEnd::Completed,
                    text: previous,
                    thread_id,
                    polls,
                });
            }
            RunStatus::Failed => {
                return Err(CoreError::Upstream(format!(
                    "agent run failed (status: {})",
                    snap.raw_status.unwrap_or_else(|| "failed".into())
                )));
            }
            RunStatus::Unknown if !previous.is_empty() && stable >= STABLE_POLLS_FALLBACK => {
                // No status signal — fall back to text stability.
                polls.push(PollEvent {
                    at_ms: started.elapsed().as_millis() as u64,
                    note: "no status from upstream; finishing on stable text".into(),
                });
                return Ok(TurnOutcome {
                    end: TurnEnd::Completed,
                    text: previous,
                    thread_id,
                    polls,
                });
            }
            _ => {}
        }
    }
}

/// Resolve the model string for a request, with surface-shaped errors left to
/// the caller.
pub async fn resolve_agent(
    state: &GatewayState,
    identity: &RouteIdentity,
    model: &str,
    surface: crate::mapping::Surface,
) -> Result<String> {
    let agents = state.agents(&identity.account_id, false).await?;
    let (rules, account_default) = {
        let cfg = state.config.read().await;
        (
            cfg.mappings.clone(),
            cfg.account(&identity.account_id)
                .and_then(|a| a.default_agent_id.clone()),
        )
    };
    let default_agent = identity.default_agent_id.clone().or(account_default);
    crate::mapping::resolve_model(model, surface, &agents, &rules, default_agent.as_deref())
        .ok_or_else(|| CoreError::ModelUnresolved(model.to_string()))
}

/// Fresh request-log entry with the invariant fields filled in.
pub(crate) fn base_entry(
    surface: &str,
    method: &str,
    endpoint: &str,
) -> crate::logbuf::RequestLogEntry {
    crate::logbuf::RequestLogEntry {
        id: uuid::Uuid::new_v4().to_string(),
        started_at: chrono::Utc::now(),
        surface: surface.to_string(),
        method: method.to_string(),
        endpoint: endpoint.to_string(),
        model: None,
        agent: None,
        account_id: None,
        key_hint: None,
        stream: false,
        status: 0,
        latency_ms: 0,
        input_tokens_est: None,
        output_tokens_est: None,
        thread_id: None,
        error: None,
        request_snapshot: None,
        response_snapshot: None,
        polls: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::testutil::mock_state;
    use crate::mapping::Surface;

    #[test]
    fn lone_user_message_passes_verbatim() {
        let mut t = Transcript::default();
        t.push("user", "just do the thing");
        assert_eq!(t.render(), "just do the thing");
    }

    #[test]
    fn rich_conversations_are_wrapped() {
        let mut t = Transcript {
            system: Some("be terse".into()),
            ..Default::default()
        };
        t.push("user", "hi");
        t.push("assistant", "hello");
        t.push("user", "and now?");
        let r = t.render();
        assert!(r.contains("[system]\nbe terse"));
        assert!(r.contains("[user]\nhi"));
        assert!(r.contains("[assistant]\nhello"));
        assert!(r.ends_with("[user]\nand now?\n"));
        assert!(r.starts_with("You are answering through an API adapter"));
    }

    #[tokio::test]
    async fn run_turn_streams_deltas_and_completes() {
        let (state, _key) = mock_state();
        let identity = RouteIdentity {
            account_id: "acct1".into(),
            key_id: None,
            key_hint: None,
            default_agent_id: Some("mock-researcher".into()),
            disabled_tools: vec![],
        };
        let opts = RunOptions {
            poll_interval: Duration::from_millis(10),
            timeout: Duration::from_secs(5),
            cancel: None,
        };
        let mut chunks: Vec<String> = Vec::new();
        let outcome = run_turn(&state, &identity, "mock-researcher", "ping", &opts, |d| {
            chunks.push(d.to_string())
        })
        .await
        .unwrap();
        assert_eq!(outcome.end, TurnEnd::Completed);
        assert!(!chunks.is_empty());
        assert_eq!(chunks.concat(), outcome.text);
        assert!(outcome.text.contains("ping"));
        assert!(outcome.polls.len() >= 2);
    }

    #[tokio::test]
    async fn cancellation_stops_the_run() {
        let (state, _key) = mock_state();
        let identity = RouteIdentity {
            account_id: "acct1".into(),
            key_id: None,
            key_hint: None,
            default_agent_id: None,
            disabled_tools: vec![],
        };
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let opts = RunOptions {
            poll_interval: Duration::from_millis(10),
            timeout: Duration::from_secs(5),
            cancel: Some(cancel),
        };
        let outcome = run_turn(&state, &identity, "mock-coder", "x", &opts, |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.end, TurnEnd::Cancelled);
    }

    #[tokio::test]
    async fn resolve_agent_uses_identity_default() {
        let (state, _key) = mock_state();
        let identity = RouteIdentity {
            account_id: "acct1".into(),
            key_id: None,
            key_hint: None,
            default_agent_id: Some("mock-coder".into()),
            disabled_tools: vec![],
        };
        let agent = resolve_agent(&state, &identity, "hyperagent-default", Surface::Openai)
            .await
            .unwrap();
        assert_eq!(agent, "mock-coder");
        // Exact agent id still wins.
        let agent = resolve_agent(&state, &identity, "mock-researcher", Surface::Openai)
            .await
            .unwrap();
        assert_eq!(agent, "mock-researcher");
    }
}
