//! Live request log: bounded in-memory ring buffer + broadcast stream.
//!
//! Every gateway request produces one [`RequestLogEntry`]. Secrets are never
//! logged — keys are redacted before they get here, and request/response
//! bodies are size-capped, sanitized snapshots for the detail view.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub const LOG_CAPACITY: usize = 500;
pub const BODY_SNAPSHOT_LIMIT: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollEvent {
    pub at_ms: u64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogEntry {
    pub id: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// "openai" | "anthropic" | "system"
    pub surface: String,
    pub method: String,
    pub endpoint: String,
    /// Client-supplied model string.
    pub model: Option<String>,
    /// Resolved agent id/name.
    pub agent: Option<String>,
    pub account_id: Option<String>,
    pub key_hint: Option<String>,
    pub stream: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub input_tokens_est: Option<u64>,
    pub output_tokens_est: Option<u64>,
    pub thread_id: Option<String>,
    pub error: Option<String>,
    /// Sanitized, truncated request body (JSON string) for the detail view.
    pub request_snapshot: Option<String>,
    /// Sanitized, truncated response text for the detail view.
    pub response_snapshot: Option<String>,
    /// Timeline of poll ticks while the run was in flight.
    #[serde(default)]
    pub polls: Vec<PollEvent>,
}

/// Truncate a body snapshot to the configured limit.
pub fn snapshot(body: &str) -> String {
    if body.len() <= BODY_SNAPSHOT_LIMIT {
        body.to_string()
    } else {
        let mut cut = BODY_SNAPSHOT_LIMIT;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}… [truncated {} bytes]", &body[..cut], body.len() - cut)
    }
}

pub struct LogBuffer {
    entries: Mutex<VecDeque<RequestLogEntry>>,
    tx: broadcast::Sender<RequestLogEntry>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBuffer {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            entries: Mutex::new(VecDeque::with_capacity(LOG_CAPACITY)),
            tx,
        }
    }

    pub fn push(&self, entry: RequestLogEntry) {
        {
            let mut q = self.entries.lock().expect("log mutex");
            if q.len() == LOG_CAPACITY {
                q.pop_front();
            }
            q.push_back(entry.clone());
        }
        let _ = self.tx.send(entry);
    }

    pub fn recent(&self, limit: usize) -> Vec<RequestLogEntry> {
        let q = self.entries.lock().expect("log mutex");
        q.iter().rev().take(limit).cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<RequestLogEntry> {
        let q = self.entries.lock().expect("log mutex");
        q.iter().find(|e| e.id == id).cloned()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RequestLogEntry> {
        self.tx.subscribe()
    }

    pub fn clear(&self) {
        self.entries.lock().expect("log mutex").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> RequestLogEntry {
        RequestLogEntry {
            id: id.into(),
            started_at: chrono::Utc::now(),
            surface: "openai".into(),
            method: "POST".into(),
            endpoint: "/v1/chat/completions".into(),
            model: Some("hyperagent-default".into()),
            agent: None,
            account_id: None,
            key_hint: None,
            stream: false,
            status: 200,
            latency_ms: 1234,
            input_tokens_est: Some(10),
            output_tokens_est: Some(20),
            thread_id: None,
            error: None,
            request_snapshot: None,
            response_snapshot: None,
            polls: vec![],
        }
    }

    #[test]
    fn ring_buffer_caps() {
        let buf = LogBuffer::new();
        for i in 0..(LOG_CAPACITY + 10) {
            buf.push(entry(&format!("e{i}")));
        }
        let recent = buf.recent(LOG_CAPACITY * 2);
        assert_eq!(recent.len(), LOG_CAPACITY);
        assert_eq!(recent[0].id, format!("e{}", LOG_CAPACITY + 9));
    }

    #[test]
    fn snapshot_truncates() {
        let long = "y".repeat(BODY_SNAPSHOT_LIMIT + 100);
        let s = snapshot(&long);
        assert!(s.len() < long.len() + 40);
        assert!(s.contains("truncated"));
        assert_eq!(snapshot("short"), "short");
    }
}
