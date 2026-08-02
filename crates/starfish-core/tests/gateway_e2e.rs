//! End-to-end gateway tests against the mock upstream: real TCP listener,
//! real HTTP client, no network beyond loopback (ROADMAP: "full request cycle
//! works with no network" + the Phase 0/1 exit tests, CI-side).

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::RwLock;

use starfish_core::config::{AccountRecord, AppConfig};
use starfish_core::gateway::{self, GatewayState};
use starfish_core::keys;
use starfish_core::logbuf::LogBuffer;
use starfish_core::mapping::MappingRule;
use starfish_core::upstream::MockUpstream;

struct TestServer {
    base: String,
    /// Key routed to acct1.
    key: String,
    /// Key routed to acct2 — a *different* identity on the same gateway.
    key2: String,
    handle: gateway::ServerHandle,
    state: Arc<GatewayState>,
}

fn test_account(id: &str, nickname: &str) -> AccountRecord {
    AccountRecord {
        id: id.into(),
        nickname: nickname.into(),
        base_url: "https://hyperagent.com".into(),
        identity: None,
        default_agent_id: Some("mock-researcher".into()),
        created_at: chrono::Utc::now(),
    }
}

fn test_key(id: &str, account_id: &str) -> (String, keys::KeyRecord) {
    let (secret, hash, hint) = keys::generate_key();
    (
        secret,
        keys::KeyRecord {
            id: id.into(),
            name: format!("test {id}"),
            hash,
            hint,
            account_id: account_id.into(),
            default_agent_id: None,
            disabled_tools: vec![],
            created_at: chrono::Utc::now(),
            last_used_at: None,
            revoked: false,
        },
    )
}

async fn spawn_server() -> TestServer {
    let mut cfg = AppConfig::default();
    cfg.server.port = 0; // ephemeral
    cfg.server.poll_interval_ms = 100;
    cfg.accounts.push(test_account("acct1", "Test"));
    cfg.accounts.push(test_account("acct2", "Other"));
    let (secret, record) = test_key("key1", "acct1");
    cfg.keys.push(record);
    let (secret2, record2) = test_key("key2", "acct2");
    cfg.keys.push(record2);
    cfg.mappings.push(MappingRule {
        pattern: "claude-*".into(),
        surface: None,
        agent_id: "mock-coder".into(),
    });

    let state = Arc::new(GatewayState::new(
        Arc::new(RwLock::new(cfg)),
        Arc::new(MockUpstream::new()),
        Arc::new(LogBuffer::new()),
    ));
    let handle = gateway::start(state.clone()).await.expect("server start");
    let base = format!("http://{}", handle.addr);
    TestServer {
        base,
        key: secret,
        key2: secret2,
        handle,
        state,
    }
}

#[tokio::test]
async fn healthz_is_open() {
    let s = spawn_server().await;
    let resp = reqwest::get(format!("{}/healthz", s.base)).await.unwrap();
    assert_eq!(resp.status(), 200);
    s.handle.stop().await;
}

#[tokio::test]
async fn models_requires_key_and_lists_agents() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();

    // No key → OpenAI-shaped 401.
    let resp = client
        .get(format!("{}/v1/models", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_api_key");

    // With key → alias + mock agents.
    let resp = client
        .get(format!("{}/v1/models", s.base))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"hyperagent-default"));
    assert!(ids.contains(&"mock-researcher"));
    assert!(ids.contains(&"mock-coder"));
    s.handle.stop().await;
}

#[tokio::test]
async fn chat_completions_non_stream() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "hyperagent-default",
            "messages": [{"role": "user", "content": "hello starfish"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    let text = body["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(text.contains("hello starfish"));
    assert!(text.contains("mock-researcher")); // routed via account default
    assert!(body["usage"]["total_tokens"].as_u64().unwrap() > 0);

    // Request log captured it.
    let recent = s.state.log.recent(10);
    assert!(recent
        .iter()
        .any(|e| e.endpoint == "/v1/chat/completions" && e.status == 200));
    s.handle.stop().await;
}

#[tokio::test]
async fn chat_completions_stream_emits_chunks_and_done() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "mock-coder",
            "messages": [{"role": "user", "content": "stream me"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    assert!(text.contains("chat.completion.chunk"));
    assert!(text.contains("\"role\":\"assistant\""));
    assert!(text.contains("data: [DONE]"));
    // Reassemble the streamed content.
    let mut streamed = String::new();
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if let Some(d) = v["choices"][0]["delta"]["content"].as_str() {
                    streamed.push_str(d);
                }
            }
        }
    }
    assert!(streamed.contains("stream me"));
    s.handle.stop().await;
}

#[tokio::test]
async fn responses_non_stream_and_lifecycle() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "hyperagent-default",
            "input": "codex says hi",
            "instructions": "be nice"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    let id = body["id"].as_str().unwrap().to_string();
    let text = body["output"][0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("codex says hi"));

    // GET /v1/responses/{id}
    let resp = client
        .get(format!("{}/v1/responses/{id}", s.base))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let fetched: Value = resp.json().await.unwrap();
    assert_eq!(fetched["id"], id.as_str());

    // GET input_items
    let resp = client
        .get(format!("{}/v1/responses/{id}/input_items", s.base))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let items: Value = resp.json().await.unwrap();
    assert_eq!(items["object"], "list");
    assert!(!items["data"].as_array().unwrap().is_empty());
    s.handle.stop().await;
}

#[tokio::test]
async fn responses_stream_has_event_sequence() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "hyperagent-default",
            "input": [{"type": "message", "role": "user",
                        "content": [{"type": "input_text", "text": "stream"}]}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    for event in [
        "event: response.created",
        "event: response.output_item.added",
        "event: response.content_part.added",
        "event: response.output_text.delta",
        "event: response.output_text.done",
        "event: response.completed",
    ] {
        assert!(text.contains(event), "missing {event} in:\n{text}");
    }
    s.handle.stop().await;
}

#[tokio::test]
async fn responses_background_completes() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "hyperagent-default",
            "input": "run me in background",
            "background": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(body["status"] == "queued" || body["status"] == "in_progress");

    // Poll until done.
    let mut status = String::new();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let r: Value = client
            .get(format!("{}/v1/responses/{id}", s.base))
            .bearer_auth(&s.key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        status = r["status"].as_str().unwrap_or("").to_string();
        if status == "completed" {
            assert!(r["output"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("background"));
            break;
        }
    }
    assert_eq!(status, "completed");
    s.handle.stop().await;
}

#[tokio::test]
async fn responses_are_scoped_to_the_creating_key() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();

    // key1 (acct1) starts a background run.
    let body: Value = client
        .post(format!("{}/v1/responses", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "hyperagent-default",
            "input": "acct1's private prompt",
            "background": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = body["id"].as_str().unwrap().to_string();

    // key2 (a different account) must not see it — indistinguishable from a
    // nonexistent id on read, list, and cancel.
    for path in [
        format!("/v1/responses/{id}"),
        format!("/v1/responses/{id}/input_items"),
    ] {
        let resp = client
            .get(format!("{}{path}", s.base))
            .bearer_auth(&s.key2)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404, "GET {path} must be hidden from key2");
        let e: Value = resp.json().await.unwrap();
        assert_eq!(e["error"]["code"], "not_found");
    }
    let resp = client
        .post(format!("{}/v1/responses/{id}/cancel", s.base))
        .bearer_auth(&s.key2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "cancel must be hidden from key2");

    // The foreign cancel attempt must not have interrupted the owner's run.
    let mut status = String::new();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let r: Value = client
            .get(format!("{}/v1/responses/{id}", s.base))
            .bearer_auth(&s.key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        status = r["status"].as_str().unwrap_or("").to_string();
        if status == "completed" {
            break;
        }
    }
    assert_eq!(
        status, "completed",
        "a foreign cancel must not stop the owner's run"
    );

    // The owner keeps full access to its own response.
    let resp = client
        .get(format!("{}/v1/responses/{id}/input_items", s.base))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .post(format!("{}/v1/responses/{id}/cancel", s.base))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    s.handle.stop().await;
}

#[tokio::test]
async fn anthropic_messages_non_stream_with_x_api_key() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", s.base))
        .header("x-api-key", &s.key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "claude-sonnet-4-5-20250929",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi claude code"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["stop_reason"], "end_turn");
    let text = body["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("hi claude code"));
    // claude-* mapped to mock-coder via the mapping table.
    assert!(text.contains("mock-coder"));
    assert!(body["usage"]["input_tokens"].as_u64().unwrap() > 0);
    s.handle.stop().await;
}

#[tokio::test]
async fn anthropic_messages_stream_full_sequence() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", s.base))
        .header("x-api-key", &s.key)
        .json(&json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 512,
            "stream": true,
            "messages": [{"role": "user", "content": "stream please"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    for event in [
        "event: message_start",
        "event: content_block_start",
        "event: content_block_delta",
        "event: content_block_stop",
        "event: message_delta",
        "event: message_stop",
        "event: ping",
    ] {
        assert!(text.contains(event), "missing {event} in:\n{text}");
    }
    assert!(text.contains("text_delta"));
    assert!(text.contains("\"stop_reason\":\"end_turn\""));
    s.handle.stop().await;
}

#[tokio::test]
async fn anthropic_count_tokens() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages/count_tokens", s.base))
        .header("x-api-key", &s.key)
        .json(&json!({
            "model": "claude-3-5-haiku-latest",
            "messages": [{"role": "user", "content": "count me please, four words"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["input_tokens"].as_u64().unwrap() > 0);
    s.handle.stop().await;
}

#[tokio::test]
async fn anthropic_errors_are_anthropic_shaped() {
    let s = spawn_server().await;
    let client = reqwest::Client::new();
    // Wrong key
    let resp = client
        .post(format!("{}/v1/messages", s.base))
        .header("x-api-key", "sk-starfish-wrong")
        .json(&json!({"model": "m", "max_tokens": 1, "messages": [{"role":"user","content":"x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");

    // Missing messages
    let resp = client
        .post(format!("{}/v1/messages", s.base))
        .header("x-api-key", &s.key)
        .json(&json!({"model": "claude-x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    s.handle.stop().await;
}

#[tokio::test]
async fn unknown_model_with_no_default_is_404() {
    let s = spawn_server().await;
    // Remove defaults + mappings.
    {
        let mut cfg = s.state.config.write().await;
        cfg.accounts[0].default_agent_id = None;
        cfg.mappings.clear();
    }
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", s.base))
        .bearer_auth(&s.key)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
    s.handle.stop().await;
}
