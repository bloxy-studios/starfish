//! OpenAI-compatible surface: `/v1/models`, `/v1/chat/completions`, and the
//! Responses API (`/v1/responses…`) that Codex speaks by default.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::CoreError;
use crate::estimate::{estimate_conversation_tokens, estimate_tokens};
use crate::logbuf::snapshot;
use crate::mapping::{Surface, DEFAULT_MODEL_ALIAS};

use super::auth::authenticate;
use super::common::{base_entry, resolve_agent, run_turn, RunOptions, Transcript, TurnEnd};
use super::sse::{channel_response, SseEvent};
use super::GatewayState;

// ---------------------------------------------------------------------------
// Error shaping
// ---------------------------------------------------------------------------

/// OpenAI-style error envelope.
pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let etype = if status.is_server_error() {
        "api_error"
    } else {
        "invalid_request_error"
    };
    let body = json!({
        "error": { "message": message, "type": etype, "param": null, "code": code }
    });
    (status, Json(body)).into_response()
}

pub(crate) fn map_core_error(e: &CoreError) -> Response {
    match e {
        CoreError::ModelUnresolved(m) => error_response(
            StatusCode::NOT_FOUND,
            "model_not_found",
            &format!(
                "The model '{m}' does not exist for this key. Use an agent id/name from \
                 GET /v1/models, '{DEFAULT_MODEL_ALIAS}', or add a mapping in Starfish → Models."
            ),
        ),
        CoreError::Unauthorized | CoreError::OAuth(_) => error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_auth",
            &format!("Hyperagent rejected Starfish's credentials — re-authenticate the account in Starfish. ({e})"),
        ),
        CoreError::RunTimeout(secs) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            &format!("The agent run exceeded the {secs}s timeout (adjustable in Starfish → Settings)."),
        ),
        CoreError::AccountNotFound(id) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "account_missing",
            &format!("This key routes to account '{id}', which no longer exists in Starfish."),
        ),
        CoreError::Mcp(_) | CoreError::Upstream(_) | CoreError::Http(_) => error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            &format!("Upstream error from Hyperagent: {e}"),
        ),
        _ => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            &format!("{e}"),
        ),
    }
}

fn status_of(resp: &Response) -> u16 {
    resp.status().as_u16()
}

// ---------------------------------------------------------------------------
// GET /v1/models
// ---------------------------------------------------------------------------

fn model_object(id: &str, description: Option<&str>) -> Value {
    let mut obj = json!({
        "id": id,
        "object": "model",
        "created": 0,
        "owned_by": "hyperagent"
    });
    if let Some(d) = description {
        obj["description"] = json!(d);
    }
    obj
}

pub async fn list_models(State(state): State<Arc<GatewayState>>, headers: HeaderMap) -> Response {
    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    drop(cfg);

    match state.agents(&identity.account_id, false).await {
        Ok(agents) => {
            let mut data: Vec<Value> = vec![model_object(
                DEFAULT_MODEL_ALIAS,
                Some("Alias for this key's default Hyperagent agent"),
            )];
            data.extend(
                agents
                    .iter()
                    .map(|a| model_object(&a.id, a.description.as_deref())),
            );
            Json(json!({ "object": "list", "data": data })).into_response()
        }
        Err(e) => map_core_error(&e),
    }
}

pub async fn get_model(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    drop(cfg);

    if id == DEFAULT_MODEL_ALIAS {
        return Json(model_object(&id, Some("Alias for the default agent"))).into_response();
    }
    match state.agents(&identity.account_id, false).await {
        Ok(agents) => match agents
            .iter()
            .find(|a| a.id == id || a.name.eq_ignore_ascii_case(&id))
        {
            Some(a) => Json(model_object(&a.id, a.description.as_deref())).into_response(),
            None => error_response(
                StatusCode::NOT_FOUND,
                "model_not_found",
                &format!("The model '{id}' does not exist."),
            ),
        },
        Err(e) => map_core_error(&e),
    }
}

// ---------------------------------------------------------------------------
// POST /v1/chat/completions
// ---------------------------------------------------------------------------

/// Pull text out of an OpenAI message `content` (string or parts array).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
                match ptype {
                    "text" | "input_text" | "output_text" => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(t);
                        }
                    }
                    "image_url" | "input_image" => {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str("[image attached — not forwarded by Starfish yet]");
                    }
                    _ => {}
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Flatten OpenAI chat `messages` into a transcript.
fn flatten_chat_messages(messages: &[Value]) -> Transcript {
    let mut t = Transcript::default();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let text = m.get("content").map(content_text).unwrap_or_default();
        match role {
            "system" | "developer" => {
                let sys = t.system.get_or_insert_with(String::new);
                if !sys.is_empty() {
                    sys.push('\n');
                }
                sys.push_str(&text);
            }
            "tool" => {
                let call_id = m
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                t.push("user", format!("[tool result for {call_id}]\n{text}"));
            }
            "assistant" => {
                let mut text = text;
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for c in calls {
                        let name = c
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        let args = c
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}");
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&format!("[called tool {name} with {args}]"));
                    }
                }
                t.push("assistant", text);
            }
            other => t.push(other, text),
        }
    }
    t
}

pub async fn chat_completions(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let started = Instant::now();
    let mut entry = base_entry("openai", "POST", "/v1/chat/completions");
    entry.request_snapshot = Some(snapshot(&body.to_string()));

    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => {
            entry.status = status_of(&resp);
            entry.latency_ms = started.elapsed().as_millis() as u64;
            entry.error = Some("unauthorized".into());
            state.log.push(entry);
            return *resp;
        }
    };
    drop(cfg);
    entry.account_id = Some(identity.account_id.clone());
    entry.key_hint = identity.key_hint.clone();

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL_ALIAS)
        .to_string();
    entry.model = Some(model.clone());

    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        let resp = error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "'messages' is required and must be an array.",
        );
        entry.status = status_of(&resp);
        entry.latency_ms = started.elapsed().as_millis() as u64;
        state.log.push(entry);
        return resp;
    };
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let include_usage = body
        .pointer("/stream_options/include_usage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    entry.stream = stream;

    let transcript = flatten_chat_messages(messages);
    let input_est = estimate_conversation_tokens(&transcript.parts());
    entry.input_tokens_est = Some(input_est);

    let agent = match resolve_agent(&state, &identity, &model, Surface::Openai).await {
        Ok(a) => a,
        Err(e) => {
            let resp = map_core_error(&e);
            entry.status = status_of(&resp);
            entry.latency_ms = started.elapsed().as_millis() as u64;
            entry.error = Some(e.to_string());
            state.log.push(entry);
            return resp;
        }
    };
    entry.agent = Some(agent.clone());

    let prompt = transcript.render();
    let opts = RunOptions::from_state(&state).await;
    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = chrono::Utc::now().timestamp();

    if stream {
        let (tx, response) = channel_response();
        let state2 = state.clone();
        let model2 = model.clone();
        tokio::spawn(async move {
            let chunk = |delta: Value, finish: Option<&str>| {
                json!({
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model2,
                    "system_fingerprint": "starfish",
                    "choices": [{
                        "index": 0,
                        "delta": delta,
                        "logprobs": null,
                        "finish_reason": finish
                    }]
                })
            };
            // Role-first delta.
            let _ = tx.send(SseEvent::data(
                chunk(json!({"role": "assistant", "content": ""}), None).to_string(),
            ));

            let tx_delta = tx.clone();
            let result = run_turn(&state2, &identity, &agent, &prompt, &opts, |delta| {
                let _ = tx_delta.send(SseEvent::data(
                    chunk(json!({"content": delta}), None).to_string(),
                ));
            })
            .await;

            match result {
                Ok(outcome) => {
                    let _ = tx.send(SseEvent::data(chunk(json!({}), Some("stop")).to_string()));
                    if include_usage {
                        let out_est = estimate_tokens(&outcome.text);
                        let _ = tx.send(SseEvent::data(
                            json!({
                                "id": completion_id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model2,
                                "choices": [],
                                "usage": usage_object(input_est, out_est)
                            })
                            .to_string(),
                        ));
                    }
                    let _ = tx.send(SseEvent::data("[DONE]"));
                    entry.status = 200;
                    entry.output_tokens_est = Some(estimate_tokens(&outcome.text));
                    entry.thread_id = Some(outcome.thread_id.clone());
                    entry.polls = outcome.polls;
                    entry.response_snapshot = Some(snapshot(&outcome.text));
                }
                Err(e) => {
                    let _ = tx.send(SseEvent::data(
                        json!({"error": {"message": e.to_string(), "type": "api_error", "param": null, "code": "upstream_error"}})
                            .to_string(),
                    ));
                    let _ = tx.send(SseEvent::data("[DONE]"));
                    entry.status = 200; // headers were already sent
                    entry.error = Some(e.to_string());
                }
            }
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state2.log.push(entry);
        });
        return response;
    }

    // Non-streaming
    match run_turn(&state, &identity, &agent, &prompt, &opts, |_| {}).await {
        Ok(outcome) => {
            let out_est = estimate_tokens(&outcome.text);
            let body = json!({
                "id": completion_id,
                "object": "chat.completion",
                "created": created,
                "model": model,
                "system_fingerprint": "starfish",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": outcome.text,
                        "refusal": null,
                        "annotations": []
                    },
                    "logprobs": null,
                    "finish_reason": "stop"
                }],
                "usage": usage_object(input_est, out_est),
                "service_tier": null
            });
            entry.status = 200;
            entry.output_tokens_est = Some(out_est);
            entry.thread_id = Some(outcome.thread_id.clone());
            entry.polls = outcome.polls;
            entry.response_snapshot = Some(snapshot(&outcome.text));
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            Json(body).into_response()
        }
        Err(e) => {
            let resp = map_core_error(&e);
            entry.status = status_of(&resp);
            entry.error = Some(e.to_string());
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            resp
        }
    }
}

fn usage_object(input: u64, output: u64) -> Value {
    // Estimates — the upstream exposes no exact counts (labeled in UI/docs).
    json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": input + output,
        "prompt_tokens_details": { "cached_tokens": 0 },
        "completion_tokens_details": { "reasoning_tokens": 0 }
    })
}

// ---------------------------------------------------------------------------
// Responses API (Codex's default wire API)
// ---------------------------------------------------------------------------

/// The identity a stored response belongs to — the key that created it.
///
/// Responses are readable/cancellable only by the identity that created them:
/// keys route to accounts and can belong to different people sharing one
/// Starfish (MISSION §3/§6B), so a second key must not be able to fetch or
/// interrupt another key's run just by learning the `resp_…` id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseOwner {
    pub account_id: String,
    /// `None` only in dev anonymous mode.
    pub key_id: Option<String>,
}

impl ResponseOwner {
    pub fn of(identity: &super::auth::RouteIdentity) -> Self {
        Self {
            account_id: identity.account_id.clone(),
            key_id: identity.key_id.clone(),
        }
    }
}

pub struct StoredResponse {
    pub owner: ResponseOwner,
    pub response: Value,
    pub input_items: Vec<Value>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct ResponsesRegistry {
    inner: Mutex<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    order: VecDeque<String>,
    items: HashMap<String, StoredResponse>,
}

const RESPONSES_CAPACITY: usize = 200;

impl ResponsesRegistry {
    pub fn insert(&self, id: String, stored: StoredResponse) {
        let mut inner = self.inner.lock().expect("registry lock");
        if inner.order.len() == RESPONSES_CAPACITY {
            if let Some(old) = inner.order.pop_front() {
                inner.items.remove(&old);
            }
        }
        inner.order.push_back(id.clone());
        inner.items.insert(id, stored);
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut StoredResponse)) {
        let mut inner = self.inner.lock().expect("registry lock");
        if let Some(item) = inner.items.get_mut(id) {
            f(item);
        }
    }

    /// Owner-scoped read: `None` both when the id is unknown and when it
    /// belongs to a different identity, so callers can't distinguish the two.
    pub fn response_for(&self, id: &str, owner: &ResponseOwner) -> Option<Value> {
        let inner = self.inner.lock().expect("registry lock");
        inner
            .items
            .get(id)
            .filter(|s| s.owner == *owner)
            .map(|s| s.response.clone())
    }

    pub fn input_items_for(&self, id: &str, owner: &ResponseOwner) -> Option<Vec<Value>> {
        let inner = self.inner.lock().expect("registry lock");
        inner
            .items
            .get(id)
            .filter(|s| s.owner == *owner)
            .map(|s| s.input_items.clone())
    }

    pub fn cancel_flag_for(&self, id: &str, owner: &ResponseOwner) -> Option<Arc<AtomicBool>> {
        let inner = self.inner.lock().expect("registry lock");
        inner
            .items
            .get(id)
            .filter(|s| s.owner == *owner)
            .map(|s| s.cancel.clone())
    }

    /// Unscoped read for the request path that created the entry (it already
    /// holds the freshly minted id). HTTP handlers must use `response_for`.
    fn response_unchecked(&self, id: &str) -> Option<Value> {
        let inner = self.inner.lock().expect("registry lock");
        inner.items.get(id).map(|s| s.response.clone())
    }
}

/// Normalize the `input` field into (transcript turns, normalized items).
fn flatten_responses_input(input: &Value, instructions: Option<&str>) -> (Transcript, Vec<Value>) {
    let mut t = Transcript::default();
    if let Some(instr) = instructions {
        if !instr.is_empty() {
            t.system = Some(instr.to_string());
        }
    }
    let mut items: Vec<Value> = Vec::new();

    match input {
        Value::String(s) => {
            t.push("user", s.clone());
            items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": s }]
            }));
        }
        Value::Array(arr) => {
            for item in arr {
                let itype = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                match itype {
                    "message" => {
                        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                        let text = item.get("content").map(content_text).unwrap_or_default();
                        if role == "system" || role == "developer" {
                            let sys = t.system.get_or_insert_with(String::new);
                            if !sys.is_empty() {
                                sys.push('\n');
                            }
                            sys.push_str(&text);
                        } else {
                            t.push(role, text);
                        }
                        items.push(item.clone());
                    }
                    "function_call" => {
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let args = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}");
                        t.push("assistant", format!("[called tool {name} with {args}]"));
                        items.push(item.clone());
                    }
                    "function_call_output" => {
                        let output = item.get("output").map(content_text).unwrap_or_else(|| {
                            item.get("output")
                                .map(|v| v.to_string())
                                .unwrap_or_default()
                        });
                        t.push("user", format!("[tool result]\n{output}"));
                        items.push(item.clone());
                    }
                    // Reasoning items / item references from stored contexts are
                    // not replayable here — skip them but keep them listable.
                    _ => items.push(item.clone()),
                }
            }
        }
        _ => {}
    }
    (t, items)
}

/// Build a Responses API `response` object.
#[allow(clippy::too_many_arguments)]
fn response_object(
    id: &str,
    created: i64,
    model: &str,
    status: &str,
    message_id: &str,
    text: Option<&str>,
    usage: Option<(u64, u64)>,
    instructions: Option<&str>,
    store: bool,
    background: bool,
) -> Value {
    let output = match text {
        Some(text) if !text.is_empty() || status == "completed" => json!([{
            "type": "message",
            "id": message_id,
            "status": if status == "completed" { "completed" } else { "in_progress" },
            "role": "assistant",
            "content": [{ "type": "output_text", "annotations": [], "text": text }]
        }]),
        _ => json!([]),
    };
    let usage = match usage {
        Some((input, output_toks)) => json!({
            "input_tokens": input,
            "input_tokens_details": { "cached_tokens": 0 },
            "output_tokens": output_toks,
            "output_tokens_details": { "reasoning_tokens": 0 },
            "total_tokens": input + output_toks
        }),
        None => Value::Null,
    };
    json!({
        "id": id,
        "object": "response",
        "created_at": created,
        "status": status,
        "background": background,
        "error": null,
        "incomplete_details": null,
        "instructions": instructions,
        "max_output_tokens": null,
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": { "effort": null, "summary": null },
        "store": store,
        "temperature": 1.0,
        "text": { "format": { "type": "text" } },
        "tool_choice": "auto",
        "tools": [],
        "top_p": 1.0,
        "truncation": "disabled",
        "usage": usage,
        "user": null,
        "metadata": {}
    })
}

pub async fn create_response(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let started = Instant::now();
    let mut entry = base_entry("openai", "POST", "/v1/responses");
    entry.request_snapshot = Some(snapshot(&body.to_string()));

    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => {
            entry.status = status_of(&resp);
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            return *resp;
        }
    };
    drop(cfg);
    entry.account_id = Some(identity.account_id.clone());
    entry.key_hint = identity.key_hint.clone();

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL_ALIAS)
        .to_string();
    entry.model = Some(model.clone());
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let background = body
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let store = body.get("store").and_then(Value::as_bool).unwrap_or(true);
    entry.stream = stream;

    let instructions = body
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::to_string);
    let input = body.get("input").cloned().unwrap_or(Value::Null);
    if input.is_null() {
        let resp = error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "'input' is required (string or array of input items).",
        );
        entry.status = status_of(&resp);
        entry.latency_ms = started.elapsed().as_millis() as u64;
        state.log.push(entry);
        return resp;
    }

    let (transcript, items) = flatten_responses_input(&input, instructions.as_deref());
    let input_est = estimate_conversation_tokens(&transcript.parts());
    entry.input_tokens_est = Some(input_est);

    let agent = match resolve_agent(&state, &identity, &model, Surface::Openai).await {
        Ok(a) => a,
        Err(e) => {
            let resp = map_core_error(&e);
            entry.status = status_of(&resp);
            entry.error = Some(e.to_string());
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            return resp;
        }
    };
    entry.agent = Some(agent.clone());

    let prompt = transcript.render();
    let response_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let created = chrono::Utc::now().timestamp();
    let cancel = Arc::new(AtomicBool::new(false));

    let mut opts = RunOptions::from_state(&state).await;
    opts.cancel = Some(cancel.clone());

    // Register up-front so GET /v1/responses/{id} works during the run,
    // scoped to the identity that created it.
    state.responses.insert(
        response_id.clone(),
        StoredResponse {
            owner: ResponseOwner::of(&identity),
            response: response_object(
                &response_id,
                created,
                &model,
                if background { "queued" } else { "in_progress" },
                &message_id,
                None,
                None,
                instructions.as_deref(),
                store,
                background,
            ),
            input_items: items,
            cancel: cancel.clone(),
        },
    );

    // --- background mode: reply immediately, run in a task -----------------
    if background {
        let state2 = state.clone();
        let response_id2 = response_id.clone();
        let model2 = model.clone();
        let instructions2 = instructions.clone();
        let queued = state
            .responses
            .response_unchecked(&response_id)
            .expect("just inserted");
        tokio::spawn(async move {
            state2.responses.update(&response_id2, |s| {
                s.response["status"] = json!("in_progress");
            });
            let result = run_turn(&state2, &identity, &agent, &prompt, &opts, |_| {}).await;
            let final_response = match &result {
                Ok(outcome) => {
                    let status = match outcome.end {
                        TurnEnd::Completed => "completed",
                        TurnEnd::Cancelled => "cancelled",
                    };
                    entry.thread_id = Some(outcome.thread_id.clone());
                    entry.polls = outcome.polls.clone();
                    entry.output_tokens_est = Some(estimate_tokens(&outcome.text));
                    entry.status = 200;
                    response_object(
                        &response_id2,
                        created,
                        &model2,
                        status,
                        &format!("msg_{}", uuid::Uuid::new_v4().simple()),
                        Some(&outcome.text),
                        Some((input_est, estimate_tokens(&outcome.text))),
                        instructions2.as_deref(),
                        store,
                        true,
                    )
                }
                Err(e) => {
                    entry.status = 502;
                    entry.error = Some(e.to_string());
                    let mut r = response_object(
                        &response_id2,
                        created,
                        &model2,
                        "failed",
                        "msg_failed",
                        None,
                        None,
                        instructions2.as_deref(),
                        store,
                        true,
                    );
                    r["error"] = json!({ "code": "upstream_error", "message": e.to_string() });
                    r
                }
            };
            state2
                .responses
                .update(&response_id2, |s| s.response = final_response);
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state2.log.push(entry);
        });
        return (StatusCode::OK, Json(queued)).into_response();
    }

    // --- streaming ----------------------------------------------------------
    if stream {
        let (tx, http_response) = channel_response();
        let state2 = state.clone();
        let instructions2 = instructions.clone();
        tokio::spawn(async move {
            let seq = std::sync::atomic::AtomicU64::new(0);
            let next_seq = || seq.fetch_add(1, Ordering::Relaxed);
            let base = response_object(
                &response_id,
                created,
                &model,
                "in_progress",
                &message_id,
                None,
                None,
                instructions2.as_deref(),
                store,
                false,
            );
            let _ = tx.send(SseEvent::named(
                "response.created",
                json!({"type": "response.created", "response": base, "sequence_number": next_seq()})
                    .to_string(),
            ));
            let base2 = state2
                .responses
                .response_unchecked(&response_id)
                .unwrap_or(Value::Null);
            let _ = tx.send(SseEvent::named(
                "response.in_progress",
                json!({"type": "response.in_progress", "response": base2, "sequence_number": next_seq()})
                    .to_string(),
            ));
            let _ = tx.send(SseEvent::named(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "id": message_id, "type": "message", "status": "in_progress",
                        "role": "assistant", "content": []
                    },
                    "sequence_number": next_seq()
                })
                .to_string(),
            ));
            let _ = tx.send(SseEvent::named(
                "response.content_part.added",
                json!({
                    "type": "response.content_part.added",
                    "item_id": message_id, "output_index": 0, "content_index": 0,
                    "part": { "type": "output_text", "annotations": [], "text": "" },
                    "sequence_number": next_seq()
                })
                .to_string(),
            ));

            let tx_delta = tx.clone();
            let message_id2 = message_id.clone();
            let result = run_turn(&state2, &identity, &agent, &prompt, &opts, |delta| {
                let _ = tx_delta.send(SseEvent::named(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": message_id2, "output_index": 0, "content_index": 0,
                        "delta": delta,
                        "sequence_number": next_seq()
                    })
                    .to_string(),
                ));
            })
            .await;

            match result {
                Ok(outcome) => {
                    let out_est = estimate_tokens(&outcome.text);
                    let status = match outcome.end {
                        TurnEnd::Completed => "completed",
                        TurnEnd::Cancelled => "cancelled",
                    };
                    let _ = tx.send(SseEvent::named(
                        "response.output_text.done",
                        json!({
                            "type": "response.output_text.done",
                            "item_id": message_id, "output_index": 0, "content_index": 0,
                            "text": outcome.text,
                            "sequence_number": next_seq()
                        })
                        .to_string(),
                    ));
                    let _ = tx.send(SseEvent::named(
                        "response.content_part.done",
                        json!({
                            "type": "response.content_part.done",
                            "item_id": message_id, "output_index": 0, "content_index": 0,
                            "part": { "type": "output_text", "annotations": [], "text": outcome.text },
                            "sequence_number": next_seq()
                        })
                        .to_string(),
                    ));
                    let _ = tx.send(SseEvent::named(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": 0,
                            "item": {
                                "id": message_id, "type": "message", "status": "completed",
                                "role": "assistant",
                                "content": [{ "type": "output_text", "annotations": [], "text": outcome.text }]
                            },
                            "sequence_number": next_seq()
                        })
                        .to_string(),
                    ));
                    let final_response = response_object(
                        &response_id,
                        created,
                        &model,
                        status,
                        &message_id,
                        Some(&outcome.text),
                        Some((input_est, out_est)),
                        instructions2.as_deref(),
                        store,
                        false,
                    );
                    state2
                        .responses
                        .update(&response_id, |s| s.response = final_response.clone());
                    let event_name = if status == "completed" {
                        "response.completed"
                    } else {
                        "response.cancelled"
                    };
                    let _ = tx.send(SseEvent::named(
                        event_name,
                        json!({"type": event_name, "response": final_response, "sequence_number": next_seq()})
                            .to_string(),
                    ));
                    entry.status = 200;
                    entry.thread_id = Some(outcome.thread_id.clone());
                    entry.polls = outcome.polls;
                    entry.output_tokens_est = Some(out_est);
                    entry.response_snapshot = Some(snapshot(&outcome.text));
                }
                Err(e) => {
                    let mut failed = response_object(
                        &response_id,
                        created,
                        &model,
                        "failed",
                        &message_id,
                        None,
                        None,
                        instructions2.as_deref(),
                        store,
                        false,
                    );
                    failed["error"] = json!({ "code": "upstream_error", "message": e.to_string() });
                    state2
                        .responses
                        .update(&response_id, |s| s.response = failed.clone());
                    let _ = tx.send(SseEvent::named(
                        "response.failed",
                        json!({"type": "response.failed", "response": failed, "sequence_number": next_seq()})
                            .to_string(),
                    ));
                    entry.status = 200;
                    entry.error = Some(e.to_string());
                }
            }
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state2.log.push(entry);
        });
        return http_response;
    }

    // --- non-streaming, foreground ------------------------------------------
    match run_turn(&state, &identity, &agent, &prompt, &opts, |_| {}).await {
        Ok(outcome) => {
            let out_est = estimate_tokens(&outcome.text);
            let status = match outcome.end {
                TurnEnd::Completed => "completed",
                TurnEnd::Cancelled => "cancelled",
            };
            let final_response = response_object(
                &response_id,
                created,
                &model,
                status,
                &message_id,
                Some(&outcome.text),
                Some((input_est, out_est)),
                instructions.as_deref(),
                store,
                false,
            );
            state
                .responses
                .update(&response_id, |s| s.response = final_response.clone());
            entry.status = 200;
            entry.thread_id = Some(outcome.thread_id.clone());
            entry.polls = outcome.polls;
            entry.output_tokens_est = Some(out_est);
            entry.response_snapshot = Some(snapshot(&outcome.text));
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            Json(final_response).into_response()
        }
        Err(e) => {
            let resp = map_core_error(&e);
            entry.status = status_of(&resp);
            entry.error = Some(e.to_string());
            entry.latency_ms = started.elapsed().as_millis() as u64;
            state.log.push(entry);
            resp
        }
    }
}

/// Uniform 404 for ids that don't exist *or* belong to another identity —
/// deliberately indistinguishable so foreign ids can't be probed.
fn response_not_found(id: &str) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "not_found",
        &format!(
            "Response '{id}' not found for this key (Starfish keeps the last {RESPONSES_CAPACITY} in memory)."
        ),
    )
}

pub async fn get_response(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    drop(cfg);
    let owner = ResponseOwner::of(&identity);
    match state.responses.response_for(&id, &owner) {
        Some(r) => Json(r).into_response(),
        None => response_not_found(&id),
    }
}

pub async fn cancel_response(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    drop(cfg);
    let owner = ResponseOwner::of(&identity);
    match state.responses.cancel_flag_for(&id, &owner) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            state.responses.update(&id, |s| {
                let status = s.response["status"].as_str().unwrap_or("");
                if status == "queued" || status == "in_progress" {
                    s.response["status"] = json!("cancelled");
                }
            });
            Json(
                state
                    .responses
                    .response_for(&id, &owner)
                    .unwrap_or(Value::Null),
            )
            .into_response()
        }
        None => response_not_found(&id),
    }
}

pub async fn list_response_input_items(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let cfg = state.config.read().await;
    let identity = match authenticate(&cfg, &headers, Surface::Openai) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    drop(cfg);
    let owner = ResponseOwner::of(&identity);
    match state.responses.input_items_for(&id, &owner) {
        Some(items) => {
            let data: Vec<Value> = items
                .into_iter()
                .enumerate()
                .map(|(i, mut item)| {
                    if item.get("id").is_none() {
                        item["id"] = json!(format!("item_{i}"));
                    }
                    item
                })
                .collect();
            Json(json!({
                "object": "list",
                "data": data,
                "first_id": data.first().and_then(|v| v.get("id")).cloned().unwrap_or(Value::Null),
                "last_id": data.last().and_then(|v| v.get("id")).cloned().unwrap_or(Value::Null),
                "has_more": false
            }))
            .into_response()
        }
        None => response_not_found(&id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_text_handles_both_shapes() {
        assert_eq!(content_text(&json!("plain")), "plain");
        let parts = json!([
            {"type": "text", "text": "a"},
            {"type": "image_url", "image_url": {"url": "http://x"}},
            {"type": "text", "text": "b"}
        ]);
        let out = content_text(&parts);
        assert!(out.contains("a\n"));
        assert!(out.contains("[image attached"));
        assert!(out.ends_with("b"));
    }

    #[test]
    fn chat_flattening() {
        let messages = vec![
            json!({"role": "system", "content": "be brief"}),
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "hello", "tool_calls": [
                {"function": {"name": "search", "arguments": "{\"q\":\"x\"}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "result!"}),
            json!({"role": "user", "content": "thanks"}),
        ];
        let t = flatten_chat_messages(&messages);
        assert_eq!(t.system.as_deref(), Some("be brief"));
        assert_eq!(t.turns.len(), 4);
        assert!(t.turns[1].1.contains("[called tool search"));
        assert!(t.turns[2].1.contains("[tool result for c1]"));
    }

    #[test]
    fn responses_input_string_and_items() {
        let (t, items) = flatten_responses_input(&json!("do it"), Some("sys"));
        assert_eq!(t.system.as_deref(), Some("sys"));
        assert_eq!(t.turns, vec![("user".to_string(), "do it".to_string())]);
        assert_eq!(items.len(), 1);

        let input = json!([
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "q1"}]},
            {"type": "function_call", "name": "grep", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c", "output": "found"},
            {"type": "message", "role": "user", "content": "q2"}
        ]);
        let (t, items) = flatten_responses_input(&input, None);
        assert_eq!(t.turns.len(), 4);
        assert!(t.turns[1].1.contains("[called tool grep"));
        assert!(t.turns[2].1.contains("found"));
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn response_object_shape() {
        let r = response_object(
            "resp_1",
            123,
            "m",
            "completed",
            "msg_1",
            Some("answer"),
            Some((10, 5)),
            None,
            true,
            false,
        );
        assert_eq!(r["object"], "response");
        assert_eq!(r["status"], "completed");
        assert_eq!(r["output"][0]["content"][0]["text"], "answer");
        assert_eq!(r["usage"]["total_tokens"], 15);
    }

    fn owner(account: &str, key: Option<&str>) -> ResponseOwner {
        ResponseOwner {
            account_id: account.into(),
            key_id: key.map(str::to_string),
        }
    }

    fn stored(id: &str, owner: &ResponseOwner) -> StoredResponse {
        StoredResponse {
            owner: owner.clone(),
            response: json!({"id": id, "status": "in_progress"}),
            input_items: vec![json!({"type": "message", "role": "user", "content": "x"})],
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn registry_caps_and_cancels() {
        let reg = ResponsesRegistry::default();
        let me = owner("acct1", Some("key1"));
        for i in 0..(RESPONSES_CAPACITY + 5) {
            reg.insert(format!("r{i}"), stored(&format!("r{i}"), &me));
        }
        assert!(reg.response_for("r0", &me).is_none());
        let newest = format!("r{}", RESPONSES_CAPACITY + 4);
        assert!(reg.response_for(&newest, &me).is_some());

        let flag = reg.cancel_flag_for(&newest, &me).unwrap();
        assert!(!flag.load(Ordering::Relaxed));
    }

    #[test]
    fn registry_scopes_reads_to_the_creating_identity() {
        let reg = ResponsesRegistry::default();
        let me = owner("acct1", Some("key1"));
        reg.insert("resp_mine".into(), stored("resp_mine", &me));

        // Owner sees everything.
        assert!(reg.response_for("resp_mine", &me).is_some());
        assert!(reg.input_items_for("resp_mine", &me).is_some());
        assert!(reg.cancel_flag_for("resp_mine", &me).is_some());

        // Another account's key sees nothing — same as a nonexistent id.
        let other_account = owner("acct2", Some("key2"));
        assert!(reg.response_for("resp_mine", &other_account).is_none());
        assert!(reg.input_items_for("resp_mine", &other_account).is_none());
        assert!(reg.cancel_flag_for("resp_mine", &other_account).is_none());

        // A different key on the SAME account is still a different identity
        // (keys can belong to different people sharing an account seat).
        let other_key = owner("acct1", Some("key9"));
        assert!(reg.response_for("resp_mine", &other_key).is_none());

        // Anonymous (dev mode) doesn't alias a keyed identity, and vice versa.
        let anon = owner("acct1", None);
        assert!(reg.response_for("resp_mine", &anon).is_none());
        reg.insert("resp_anon".into(), stored("resp_anon", &anon));
        assert!(reg.response_for("resp_anon", &anon).is_some());
        assert!(reg.response_for("resp_anon", &me).is_none());

        // The internal unchecked read (create path only) still resolves.
        assert!(reg.response_unchecked("resp_mine").is_some());
    }
}
