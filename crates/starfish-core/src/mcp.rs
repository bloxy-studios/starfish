//! Minimal MCP client for the Hyperagent hosted server.
//!
//! Transport: JSON-RPC 2.0 over Streamable HTTP — a POST whose response body
//! may be `application/json` *or* an SSE stream (`text/event-stream`); both
//! are handled. Implements the `initialize` handshake, honors
//! `Mcp-Session-Id`, and sends `MCP-Protocol-Version` on every request after
//! initialization (per the 2025-06-18 Streamable HTTP spec).

use std::sync::atomic::{AtomicI64, Ordering};

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::error::{CoreError, Result};

/// Protocol version we offer during `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const MCP_PATH: &str = "/api/mcp";

pub struct McpClient {
    http: reqwest::Client,
    endpoint: String,
    access_token: String,
    session_id: Mutex<Option<String>>,
    negotiated_version: Mutex<Option<String>>,
    next_id: AtomicI64,
}

impl McpClient {
    /// `base_url` is the upstream origin, e.g. `https://hyperagent.com`.
    pub fn new(http: reqwest::Client, base_url: &str, access_token: &str) -> Self {
        let endpoint = format!("{}{}", base_url.trim_end_matches('/'), MCP_PATH);
        Self {
            http,
            endpoint,
            access_token: access_token.to_string(),
            session_id: Mutex::new(None),
            negotiated_version: Mutex::new(None),
            next_id: AtomicI64::new(1),
        }
    }

    /// Update the bearer token after a refresh without losing the session.
    pub fn with_token(&self, access_token: &str) -> Self {
        Self {
            http: self.http.clone(),
            endpoint: self.endpoint.clone(),
            access_token: access_token.to_string(),
            session_id: Mutex::new(None),
            negotiated_version: Mutex::new(None),
            next_id: AtomicI64::new(1),
        }
    }

    fn rpc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// `initialize` handshake + `notifications/initialized`.
    pub async fn initialize(&self) -> Result<Value> {
        let id = self.rpc_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "starfish", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        let (result, session) = self.post_rpc(&body, id, false).await?;
        if let Some(sid) = session {
            *self.session_id.lock().await = Some(sid);
        }
        if let Some(v) = result.get("protocolVersion").and_then(Value::as_str) {
            *self.negotiated_version.lock().await = Some(v.to_string());
        }
        // Fire-and-forget initialized notification (no id → no response body
        // expected; 202 Accepted is normal).
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let _ = self.raw_post(&note).await;
        Ok(result)
    }

    /// Call an MCP tool. Initializes lazily on first use and retries once on
    /// an expired session.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        if self.session_id.lock().await.is_none() {
            self.initialize().await?;
        }
        match self.try_call_tool(name, arguments.clone()).await {
            Err(CoreError::Mcp(msg)) if msg.contains("session") || msg.contains("404") => {
                // Session likely expired — re-initialize once and retry.
                *self.session_id.lock().await = None;
                self.initialize().await?;
                self.try_call_tool(name, arguments).await
            }
            other => other,
        }
    }

    async fn try_call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let id = self.rpc_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let (result, _) = self.post_rpc(&body, id, true).await?;
        // `tools/call` results carry `content` blocks and optional
        // `structuredContent` / `isError`.
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let text = extract_text_content(&result).unwrap_or_else(|| result.to_string());
            return Err(CoreError::Upstream(format!("tool {name} failed: {text}")));
        }
        Ok(result)
    }

    /// List available tools (used by the doctor panel).
    pub async fn list_tools(&self) -> Result<Value> {
        if self.session_id.lock().await.is_none() {
            self.initialize().await?;
        }
        let id = self.rpc_id();
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} });
        let (result, _) = self.post_rpc(&body, id, true).await?;
        Ok(result)
    }

    async fn raw_post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut req = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.access_token)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(sid) = self.session_id.lock().await.as_deref() {
            req = req.header("mcp-session-id", sid);
        }
        if let Some(v) = self.negotiated_version.lock().await.as_deref() {
            req = req.header("mcp-protocol-version", v);
        }
        Ok(req.json(body).send().await?)
    }

    /// POST a JSON-RPC request; parse a JSON or SSE response; return the
    /// `result` for the matching id (plus any session id header).
    async fn post_rpc(
        &self,
        body: &Value,
        id: i64,
        _with_session: bool,
    ) -> Result<(Value, Option<String>)> {
        let resp = self.raw_post(body).await?;
        let status = resp.status();
        let session = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        if status.as_u16() == 401 {
            return Err(CoreError::Unauthorized);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(CoreError::Mcp(format!(
                "MCP HTTP {status}: {}",
                crate::logbuf::snapshot(&text)
            )));
        }

        let content_type = resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let message: Value = if content_type.starts_with("text/event-stream") {
            read_sse_until_response(resp, id).await?
        } else {
            let text = resp.text().await?;
            if text.trim().is_empty() {
                // Notifications get empty 202s.
                return Ok((Value::Null, session));
            }
            serde_json::from_str(&text)
                .map_err(|e| CoreError::Mcp(format!("bad JSON-RPC body: {e}")))?
        };

        let message = match &message {
            Value::Array(items) => items
                .iter()
                .find(|m| m.get("id").and_then(Value::as_i64) == Some(id))
                .cloned()
                .unwrap_or(Value::Null),
            _ => message,
        };

        if let Some(err) = message.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(CoreError::Mcp(format!("JSON-RPC error {code}: {msg}")));
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        Ok((result, session))
    }
}

/// Read an SSE body until we see the JSON-RPC *response* for `id` (servers may
/// interleave notifications/progress events first).
async fn read_sse_until_response(resp: reqwest::Response, id: i64) -> Result<Value> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.extend_from_slice(&chunk);
        // Process complete events (separated by a blank line).
        while let Some(pos) = find_event_boundary(&buf) {
            let event_bytes: Vec<u8> = buf.drain(..pos.end).collect();
            let event_text = String::from_utf8_lossy(&event_bytes[..pos.start]).to_string();
            if let Some(data) = sse_data(&event_text) {
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    let matches = |m: &Value| m.get("id").and_then(Value::as_i64) == Some(id);
                    match &v {
                        Value::Array(items) => {
                            if let Some(found) = items.iter().find(|m| matches(m)) {
                                return Ok(found.clone());
                            }
                        }
                        _ if matches(&v) => return Ok(v),
                        _ => {} // notification / other id — keep reading
                    }
                }
            }
        }
    }
    Err(CoreError::Mcp(
        "SSE stream ended without a response for the request".into(),
    ))
}

struct EventBoundary {
    start: usize,
    end: usize,
}

/// Find the first complete SSE event; returns (payload_end, boundary_end).
fn find_event_boundary(buf: &[u8]) -> Option<EventBoundary> {
    // Events end with \n\n (also tolerate \r\n\r\n).
    for (i, w) in buf.windows(2).enumerate() {
        if w == b"\n\n" {
            return Some(EventBoundary {
                start: i,
                end: i + 2,
            });
        }
    }
    for (i, w) in buf.windows(4).enumerate() {
        if w == b"\r\n\r\n" {
            return Some(EventBoundary {
                start: i,
                end: i + 4,
            });
        }
    }
    None
}

/// Join the `data:` lines of one SSE event.
fn sse_data(event_text: &str) -> Option<String> {
    let mut data_lines: Vec<&str> = Vec::new();
    for line in event_text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

/// Pull the concatenated text out of a `tools/call` result's `content` blocks.
pub fn extract_text_content(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    let mut out = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(t) = block.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Prefer `structuredContent`; otherwise try to parse the text content as
/// JSON; otherwise return the raw text as a JSON string.
pub fn tool_result_value(result: &Value) -> Value {
    if let Some(sc) = result.get("structuredContent") {
        if !sc.is_null() {
            return sc.clone();
        }
    }
    if let Some(text) = extract_text_content(result) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            return v;
        }
        return Value::String(text);
    }
    result.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_event_parsing() {
        let event = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}";
        let data = sse_data(event).unwrap();
        let v: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["id"], 7);
    }

    #[test]
    fn sse_multiline_data_joins() {
        let event = "data: {\"a\":\ndata: 1}";
        assert_eq!(sse_data(event).unwrap(), "{\"a\":\n1}");
    }

    #[test]
    fn boundary_detection() {
        let buf = b"data: x\n\nrest".to_vec();
        let b = find_event_boundary(&buf).unwrap();
        assert_eq!(&buf[..b.start], b"data: x");
        assert_eq!(&buf[b.end..], b"rest");
    }

    #[test]
    fn text_content_extraction() {
        let result = json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "world"}
            ]
        });
        assert_eq!(extract_text_content(&result).unwrap(), "hello\nworld");
    }

    #[test]
    fn tool_result_prefers_structured() {
        let result = json!({
            "content": [{"type":"text","text":"{\"x\":1}"}],
            "structuredContent": {"x": 2}
        });
        assert_eq!(tool_result_value(&result)["x"], 2);

        let no_structured = json!({
            "content": [{"type":"text","text":"{\"x\":1}"}]
        });
        assert_eq!(tool_result_value(&no_structured)["x"], 1);

        let plain = json!({
            "content": [{"type":"text","text":"not json"}]
        });
        assert_eq!(tool_result_value(&plain), Value::String("not json".into()));
    }
}
