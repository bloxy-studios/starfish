//! Typed wrappers over the Hyperagent MCP tools, with defensive parsers.
//!
//! The hosted server's exact payload shapes are not a published contract, so
//! every parser here is tolerant: it prefers `structuredContent`, falls back
//! to JSON-in-text, and navigates several plausible field spellings before
//! giving up with a useful error. Shapes covered are exercised by fixture
//! tests below (ROADMAP: "parser unit tests from captured payloads").

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{CoreError, Result};
use crate::mcp::{tool_result_value, McpClient};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub thread_id: String,
    pub status: RunStatus,
    /// Concatenated text of the latest assistant message (empty when none).
    pub assistant_text: String,
    /// Raw status string as reported upstream (for logs/doctor).
    pub raw_status: Option<String>,
}

/// `list_agents`
pub async fn list_agents(client: &McpClient) -> Result<Vec<AgentInfo>> {
    let result = client.call_tool("list_agents", json!({})).await?;
    let value = tool_result_value(&result);
    parse_agents(&value)
        .ok_or_else(|| CoreError::Upstream("could not parse list_agents result".into()))
}

/// `create_thread(agentId, message)` → thread id
pub async fn create_thread(client: &McpClient, agent_id: &str, message: &str) -> Result<String> {
    let result = client
        .call_tool(
            "create_thread",
            json!({ "agentId": agent_id, "message": message }),
        )
        .await?;
    let value = tool_result_value(&result);
    parse_thread_id(&value).ok_or_else(|| {
        CoreError::Upstream(format!(
            "create_thread returned no thread id: {}",
            crate::logbuf::snapshot(&value.to_string())
        ))
    })
}

/// `send_message(threadId, message)` — used for thread continuity (later
/// phase) and by the doctor.
pub async fn send_message(client: &McpClient, thread_id: &str, message: &str) -> Result<()> {
    client
        .call_tool(
            "send_message",
            json!({ "threadId": thread_id, "message": message }),
        )
        .await?;
    Ok(())
}

/// `get_thread(threadId)` → snapshot (status + latest assistant text)
pub async fn get_thread(client: &McpClient, thread_id: &str) -> Result<ThreadSnapshot> {
    let result = client
        .call_tool("get_thread", json!({ "threadId": thread_id }))
        .await?;
    let value = tool_result_value(&result);
    Ok(parse_thread_snapshot(thread_id, &value))
}

// ---------------------------------------------------------------------------
// Defensive parsers
// ---------------------------------------------------------------------------

/// Accepts `[…]`, `{agents:[…]}`, `{data:[…]}`, `{items:[…]}`.
pub fn parse_agents(value: &Value) -> Option<Vec<AgentInfo>> {
    let arr = value
        .as_array()
        .or_else(|| value.get("agents").and_then(Value::as_array))
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.get("items").and_then(Value::as_array))?;
    let mut out = Vec::new();
    for item in arr {
        let id = str_at(item, &["id", "agentId", "agent_id"])?;
        let name = str_at(item, &["name", "title", "displayName"]).unwrap_or_else(|| id.clone());
        let description = str_at(item, &["description", "summary", "about"]);
        out.push(AgentInfo {
            id,
            name,
            description,
        });
    }
    Some(out)
}

/// Accepts a bare string, `{threadId}`, `{thread_id}`, `{id}`,
/// `{thread:{id}}`, `{data:{threadId}}`.
pub fn parse_thread_id(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        let s = s.trim();
        if !s.is_empty() && !s.contains(' ') {
            return Some(s.to_string());
        }
        // Prose like "Created thread cmxyz…" — grab a plausible id token.
        return s
            .split_whitespace()
            .find(|w| w.len() >= 10 && w.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(str::to_string);
    }
    str_at(value, &["threadId", "thread_id", "id"])
        .or_else(|| value.get("thread").and_then(parse_thread_id_ref))
        .or_else(|| value.get("data").and_then(parse_thread_id_ref))
}

fn parse_thread_id_ref(value: &Value) -> Option<String> {
    parse_thread_id(value)
}

/// Build a snapshot from whatever `get_thread` returned.
pub fn parse_thread_snapshot(thread_id: &str, value: &Value) -> ThreadSnapshot {
    // The thread object may be at the root or nested.
    let root = ["thread", "data"]
        .iter()
        .find_map(|k| value.get(*k))
        .filter(|v| v.is_object())
        .unwrap_or(value);

    let raw_status = str_at(root, &["status", "state", "runStatus", "run_state"])
        .or_else(|| str_at(value, &["status", "state"]));

    let status = match raw_status
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("running") | Some("in_progress") | Some("processing") | Some("active")
        | Some("pending") | Some("queued") | Some("streaming") | Some("working") => {
            RunStatus::Running
        }
        Some("completed")
        | Some("complete")
        | Some("done")
        | Some("finished")
        | Some("idle")
        | Some("awaiting_user")
        | Some("awaiting_user_input")
        | Some("awaiting_input")
        | Some("stopped")
        | Some("ready") => RunStatus::Completed,
        Some("failed") | Some("error") | Some("errored") | Some("cancelled") | Some("canceled") => {
            RunStatus::Failed
        }
        Some(_) => RunStatus::Unknown,
        None => {
            // Some shapes use a boolean instead of a status string.
            match bool_at(root, &["running", "isRunning", "is_running", "busy"]) {
                Some(true) => RunStatus::Running,
                Some(false) => RunStatus::Completed,
                None => RunStatus::Unknown,
            }
        }
    };

    let assistant_text = extract_last_assistant_text(root).unwrap_or_default();

    ThreadSnapshot {
        thread_id: thread_id.to_string(),
        status,
        assistant_text,
        raw_status,
    }
}

/// Find the latest assistant message's text in a messages array at
/// `messages` / `history` / `turns`, tolerating several message shapes:
/// content as string, content as blocks, `text` field.
fn extract_last_assistant_text(root: &Value) -> Option<String> {
    let messages = ["messages", "history", "turns"]
        .iter()
        .find_map(|k| root.get(*k))
        .and_then(Value::as_array)?;

    for msg in messages.iter().rev() {
        let role = str_at(msg, &["role", "author", "sender"]).unwrap_or_default();
        if role != "assistant" && role != "agent" && role != "ai" {
            continue;
        }
        if let Some(text) = message_text(msg) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Extract readable text from a message value.
fn message_text(msg: &Value) -> Option<String> {
    // 1. content: "..."
    if let Some(s) = msg.get("content").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    // 2. content: [{type:text, text}, …] (also `parts`)
    for key in ["content", "parts", "blocks"] {
        if let Some(blocks) = msg.get(key).and_then(Value::as_array) {
            let mut out = String::new();
            for b in blocks {
                if let Some(s) = b.as_str() {
                    push_line(&mut out, s);
                    continue;
                }
                let btype = str_at(b, &["type"]).unwrap_or_default();
                if btype.is_empty() || btype == "text" || btype == "output_text" {
                    if let Some(t) = str_at(b, &["text", "value", "content"]) {
                        push_line(&mut out, &t);
                    }
                }
            }
            if !out.is_empty() {
                return Some(out);
            }
        }
    }
    // 3. text: "..."
    if let Some(s) = msg.get("text").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    None
}

fn push_line(out: &mut String, s: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(s);
}

fn str_at(value: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = value.get(*k).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn bool_at(value: &Value, keys: &[&str]) -> Option<bool> {
    for k in keys {
        if let Some(b) = value.get(*k).and_then(Value::as_bool) {
            return Some(b);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_from_plain_array() {
        let v = json!([
            {"id": "a1", "name": "Researcher", "description": "digs deep"},
            {"id": "a2", "name": "Coder"}
        ]);
        let agents = parse_agents(&v).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].id, "a1");
        assert_eq!(agents[1].description, None);
    }

    #[test]
    fn agents_from_wrapped_shapes() {
        for key in ["agents", "data", "items"] {
            let v = json!({ key: [{"agentId": "x9", "displayName": "X"}] });
            let agents = parse_agents(&v).unwrap();
            assert_eq!(agents[0].id, "x9");
            assert_eq!(agents[0].name, "X");
        }
    }

    #[test]
    fn thread_id_shapes() {
        assert_eq!(
            parse_thread_id(&json!("cmthread123")).unwrap(),
            "cmthread123"
        );
        assert_eq!(parse_thread_id(&json!({"threadId": "t1"})).unwrap(), "t1");
        assert_eq!(parse_thread_id(&json!({"thread_id": "t2"})).unwrap(), "t2");
        assert_eq!(parse_thread_id(&json!({"id": "t3"})).unwrap(), "t3");
        assert_eq!(
            parse_thread_id(&json!({"thread": {"id": "t4"}})).unwrap(),
            "t4"
        );
        assert_eq!(
            parse_thread_id(&json!({"data": {"threadId": "t5"}})).unwrap(),
            "t5"
        );
        assert_eq!(
            parse_thread_id(&json!("Created thread cmabc123xyz for you")).unwrap(),
            "cmabc123xyz"
        );
    }

    #[test]
    fn snapshot_running_then_completed() {
        let running = json!({
            "status": "running",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "Working on"}
            ]
        });
        let snap = parse_thread_snapshot("t", &running);
        assert_eq!(snap.status, RunStatus::Running);
        assert_eq!(snap.assistant_text, "Working on");

        let done = json!({
            "thread": {
                "state": "completed",
                "messages": [
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": [
                        {"type": "text", "text": "Working on it…"},
                        {"type": "text", "text": "done!"}
                    ]}
                ]
            }
        });
        let snap = parse_thread_snapshot("t", &done);
        assert_eq!(snap.status, RunStatus::Completed);
        assert_eq!(snap.assistant_text, "Working on it…\ndone!");
    }

    #[test]
    fn snapshot_boolean_running_flag() {
        let v = json!({ "isRunning": false, "messages": [
            {"role": "assistant", "text": "answer"}
        ]});
        let snap = parse_thread_snapshot("t", &v);
        assert_eq!(snap.status, RunStatus::Completed);
        assert_eq!(snap.assistant_text, "answer");
    }

    #[test]
    fn snapshot_unknown_status() {
        let v = json!({ "messages": [] });
        let snap = parse_thread_snapshot("t", &v);
        assert_eq!(snap.status, RunStatus::Unknown);
        assert_eq!(snap.assistant_text, "");
    }

    #[test]
    fn failed_states() {
        for s in ["failed", "error", "cancelled"] {
            let v = json!({ "status": s });
            assert_eq!(parse_thread_snapshot("t", &v).status, RunStatus::Failed);
        }
    }
}
