//! Anthropic-compatible surface — the headline feature (MISSION.md §5b).
//!
//! `POST /v1/messages` (stream + non-stream) and
//! `POST /v1/messages/count_tokens`, shaped so Claude Code and the
//! `anthropic` SDKs work unmodified. Accepts `x-api-key` *and*
//! `Authorization: Bearer`; echoes `anthropic-version`; passes through
//! `anthropic-beta` unharmed (we simply don't act on it).

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::CoreError;
use crate::estimate::{estimate_conversation_tokens, estimate_tokens};
use crate::logbuf::snapshot;
use crate::mapping::Surface;

use super::auth::authenticate;
use super::common::{base_entry, resolve_agent, run_turn, RunOptions, Transcript};
use super::sse::{channel_response, SseEvent};
use super::GatewayState;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Keep streaming connections alive with pings at this cadence.
const PING_INTERVAL_MS: u64 = 5000;

// ---------------------------------------------------------------------------
// Error shaping
// ---------------------------------------------------------------------------

/// Anthropic-style error envelope.
pub fn error_response(status: StatusCode, etype: &str, message: &str) -> Response {
    let body = json!({
        "type": "error",
        "error": { "type": etype, "message": message }
    });
    let mut resp = (status, Json(body)).into_response();
    resp.headers_mut().insert(
        "anthropic-version",
        axum::http::HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    resp
}

pub(crate) fn map_core_error(e: &CoreError) -> Response {
    match e {
        CoreError::ModelUnresolved(m) => error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            &format!(
                "model: {m} — no Hyperagent agent is mapped to this name. Claude Code uses \
                 hard-coded claude-* names; map them to agents in Starfish → Models."
            ),
        ),
        CoreError::Unauthorized | CoreError::OAuth(_) => error_response(
            StatusCode::BAD_GATEWAY,
            "api_error",
            &format!("Hyperagent rejected Starfish's credentials — re-authenticate the account in Starfish. ({e})"),
        ),
        CoreError::RunTimeout(secs) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "api_error",
            &format!("The agent run exceeded the {secs}s timeout (adjustable in Starfish → Settings)."),
        ),
        CoreError::Mcp(_) | CoreError::Upstream(_) | CoreError::Http(_) => error_response(
            StatusCode::BAD_GATEWAY,
            "api_error",
            &format!("Upstream error from Hyperagent: {e}"),
        ),
        _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, "api_error", &format!("{e}")),
    }
}

// ---------------------------------------------------------------------------
// Request flattening
// ---------------------------------------------------------------------------

/// Anthropic `system` may be a string or an array of text blocks.
fn system_text(system: &Value) -> Option<String> {
    match system {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Array(blocks) => {
            let mut out = String::new();
            for b in blocks {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
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
        _ => None,
    }
}

/// Flatten one Anthropic message's content blocks to text.
fn block_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut out = String::new();
            let mut push = |s: &str| {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(s);
            };
            for b in blocks {
                match b.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            push(t);
                        }
                    }
                    "image" => push("[image attached — not forwarded by Starfish yet]"),
                    "document" => push("[document attached — not forwarded by Starfish yet]"),
                    "tool_use" => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = b
                            .get("input")
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "{}".into());
                        push(&format!("[used tool {name} with input {input}]"));
                    }
                    "tool_result" => {
                        let id = b
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let inner = b.get("content").map(block_text).unwrap_or_default();
                        push(&format!("[tool result for {id}]\n{inner}"));
                    }
                    "thinking" | "redacted_thinking" => {} // never forward thinking blocks
                    _ => {}
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn flatten_messages_request(body: &Value) -> Result<Transcript, String> {
    let mut t = Transcript::default();
    if let Some(system) = body.get("system") {
        t.system = system_text(system);
    }
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Err("'messages' is required and must be an array.".into());
    };
    if messages.is_empty() {
        return Err("'messages' must not be empty.".into());
    }
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let text = m.get("content").map(block_text).unwrap_or_default();
        t.push(role, text);
    }
    Ok(t)
}

// ---------------------------------------------------------------------------
// POST /v1/messages
// ---------------------------------------------------------------------------

fn message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().simple())
}

fn usage_object(input: u64, output: u64) -> Value {
    // Estimates — upstream exposes no exact counts (labeled in UI/docs).
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
    })
}

pub async fn messages(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let started = Instant::now();
    let mut entry = base_entry("anthropic", "POST", "/v1/messages");
    entry.request_snapshot = Some(snapshot(&body.to_string()));

    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Anthropic) {
        Ok(i) => i,
        Err(resp) => {
            entry.status = resp.status().as_u16();
            entry.latency_ms = started.elapsed().as_millis() as u64;
            entry.error = Some("unauthorized".into());
            state.log.push(entry);
            return *resp;
        }
    };
    drop(cfg);
    entry.account_id = Some(identity.account_id.clone());
    entry.key_hint = identity.key_hint.clone();

    let Some(model) = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        let resp = error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "'model' is required.",
        );
        entry.status = resp.status().as_u16();
        entry.latency_ms = started.elapsed().as_millis() as u64;
        state.log.push(entry);
        return resp;
    };
    entry.model = Some(model.clone());
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    entry.stream = stream;

    let transcript = match flatten_messages_request(&body) {
        Ok(t) => t,
        Err(msg) => {
            let resp = error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &msg);
            entry.status = resp.status().as_u16();
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            return resp;
        }
    };
    let input_est = estimate_conversation_tokens(&transcript.parts());
    entry.input_tokens_est = Some(input_est);

    let agent = match resolve_agent(&state, &identity, &model, Surface::Anthropic).await {
        Ok(a) => a,
        Err(e) => {
            let resp = map_core_error(&e);
            entry.status = resp.status().as_u16();
            entry.error = Some(e.to_string());
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            return resp;
        }
    };
    entry.agent = Some(agent.clone());

    let prompt = transcript.render();
    let opts = RunOptions::from_state(&state).await;
    let id = message_id();

    if stream {
        let (tx, http_response) = channel_response();
        let state2 = state.clone();
        let model2 = model.clone();
        tokio::spawn(async move {
            // message_start with the message skeleton.
            let _ = tx.send(SseEvent::named(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": id, "type": "message", "role": "assistant",
                        "model": model2, "content": [],
                        "stop_reason": null, "stop_sequence": null,
                        "usage": usage_object(input_est, 0)
                    }
                })
                .to_string(),
            ));
            let _ = tx.send(SseEvent::named("ping", json!({"type": "ping"}).to_string()));
            let _ = tx.send(SseEvent::named(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                })
                .to_string(),
            ));

            // Periodic pings while the run is in flight (agent runs take
            // seconds — keep proxies and SDK read timeouts happy).
            let ping_tx = tx.clone();
            let pinger = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(PING_INTERVAL_MS)).await;
                    if ping_tx
                        .send(SseEvent::named("ping", json!({"type": "ping"}).to_string()))
                        .is_err()
                    {
                        break;
                    }
                }
            });

            let tx_delta = tx.clone();
            let result = run_turn(&state2, &identity, &agent, &prompt, &opts, |delta| {
                let _ = tx_delta.send(SseEvent::named(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "text_delta", "text": delta }
                    })
                    .to_string(),
                ));
            })
            .await;
            pinger.abort();

            match result {
                Ok(outcome) => {
                    let out_est = estimate_tokens(&outcome.text);
                    let _ = tx.send(SseEvent::named(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": 0}).to_string(),
                    ));
                    let _ = tx.send(SseEvent::named(
                        "message_delta",
                        json!({
                            "type": "message_delta",
                            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                            "usage": { "output_tokens": out_est }
                        })
                        .to_string(),
                    ));
                    let _ = tx.send(SseEvent::named(
                        "message_stop",
                        json!({"type": "message_stop"}).to_string(),
                    ));
                    entry.status = 200;
                    entry.output_tokens_est = Some(out_est);
                    entry.thread_id = Some(outcome.thread_id.clone());
                    entry.polls = outcome.polls;
                    entry.response_snapshot = Some(snapshot(&outcome.text));
                }
                Err(e) => {
                    // Anthropic streams report failures with an `error` event.
                    let _ = tx.send(SseEvent::named(
                        "error",
                        json!({
                            "type": "error",
                            "error": { "type": "api_error", "message": e.to_string() }
                        })
                        .to_string(),
                    ));
                    entry.status = 200; // headers already sent
                    entry.error = Some(e.to_string());
                }
            }
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state2.log.push(entry);
        });
        return http_response;
    }

    // Non-streaming
    match run_turn(&state, &identity, &agent, &prompt, &opts, |_| {}).await {
        Ok(outcome) => {
            let out_est = estimate_tokens(&outcome.text);
            let response = json!({
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [{ "type": "text", "text": outcome.text }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": usage_object(input_est, out_est)
            });
            entry.status = 200;
            entry.output_tokens_est = Some(out_est);
            entry.thread_id = Some(outcome.thread_id.clone());
            entry.polls = outcome.polls;
            entry.response_snapshot = Some(snapshot(&outcome.text));
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            let mut resp = Json(response).into_response();
            resp.headers_mut().insert(
                "anthropic-version",
                axum::http::HeaderValue::from_static(ANTHROPIC_VERSION),
            );
            resp
        }
        Err(e) => {
            let resp = map_core_error(&e);
            entry.status = resp.status().as_u16();
            entry.error = Some(e.to_string());
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            resp
        }
    }
}

// ---------------------------------------------------------------------------
// POST /v1/messages/count_tokens
// ---------------------------------------------------------------------------

pub async fn count_tokens(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let cfg = state.config.read().await;
    if let Err(resp) = authenticate(&cfg, &headers, Surface::Anthropic) {
        return *resp;
    }
    drop(cfg);

    match flatten_messages_request(&body) {
        Ok(t) => {
            let estimate = estimate_conversation_tokens(&t.parts());
            Json(json!({ "input_tokens": estimate })).into_response()
        }
        Err(msg) => error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_string_and_blocks() {
        assert_eq!(system_text(&json!("be kind")).unwrap(), "be kind");
        let blocks = json!([
            {"type": "text", "text": "a"},
            {"type": "text", "text": "b"}
        ]);
        assert_eq!(system_text(&blocks).unwrap(), "a\nb");
        assert!(system_text(&json!(null)).is_none());
    }

    #[test]
    fn content_blocks_flatten() {
        let content = json!([
            {"type": "text", "text": "look"},
            {"type": "image", "source": {"type": "base64", "data": "…"}},
            {"type": "tool_use", "id": "t1", "name": "grep", "input": {"q": "x"}},
            {"type": "tool_result", "tool_use_id": "t1", "content": [
                {"type": "text", "text": "3 matches"}
            ]},
            {"type": "thinking", "thinking": "secret"}
        ]);
        let out = block_text(&content);
        assert!(out.contains("look"));
        assert!(out.contains("[image attached"));
        assert!(out.contains("[used tool grep"));
        assert!(out.contains("[tool result for t1]\n3 matches"));
        assert!(!out.contains("secret"));
    }

    #[test]
    fn request_flattening_validates() {
        let bad = json!({"model": "claude-x"});
        assert!(flatten_messages_request(&bad).is_err());

        let ok = json!({
            "model": "claude-x",
            "system": "sys",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                {"role": "user", "content": "bye"}
            ]
        });
        let t = flatten_messages_request(&ok).unwrap();
        assert_eq!(t.system.as_deref(), Some("sys"));
        assert_eq!(t.turns.len(), 3);
        assert_eq!(t.turns[1], ("assistant".to_string(), "hello".to_string()));
    }
}
