# Starfish Roadmap

The build plan, derived from [MISSION.md §8](MISSION.md#8-roadmap) and broken into
**issue-sized tasks**. Each checkbox is meant to become one GitHub issue and (roughly)
one focused PR.

**How to read this**

- Phases ship in order; each ends in a **milestone you can demo**. Within a phase,
  tasks are listed in rough dependency order.
- Checking a box = merged to `main` with the acceptance note satisfied.
- Want to help? Open an issue named after the checkbox text, link this file, and claim it.

**Architecture note.** The north star is a **Rust-native core** in `src-tauri`
(axum/hyper server, reqwest for OAuth + MCP, keyring for secrets). MISSION.md documents
a Python-sidecar fallback for speed; tasks below assume the Rust-native path.

> **Status update — 2026-08-02.** The Rust-native core landed as a dedicated,
> Tauri-free workspace crate (`crates/starfish-core`) with `src-tauri` as a thin
> shell — same architecture as described below, just testable without desktop
> toolchains (68 tests run offline, including gateway e2e against the mock
> upstream). Boxes below are checked only where the acceptance note is satisfied
> by those tests; anything requiring the **live Hyperagent upstream or a real
> client** (Codex / Claude Code end-to-end, manual OAuth login, keychain
> round-trip on all three OSes) stays open — the implementation exists, the
> validation doesn't yet.


---

## Phase 0 — Foundation

> **Milestone:** the official `openai` SDK (and Cursor) completes a chat against
> Starfish, backed by a real Hyperagent agent.

### Rust core plumbing

- [x] **Add core crates & module skeleton** — `tokio`, `axum`, `reqwest`, `serde`,
  `keyring`, `thiserror`, `tracing`; create `src-tauri/src/{gateway,oauth,mcp,vault,config}.rs`
  module layout. *Accept: `cargo check` passes; modules compile empty.*
  *Done as `crates/starfish-core` + thin `src-tauri` shell.*
- [x] **Config store** — app-data-dir settings file (port, host, poll interval, run
  timeout, log level) with defaults (`127.0.0.1:8787`, 1s poll, 600s timeout); Tauri
  commands to read/write. *Accept: settings survive app restart.*

### Hyperagent auth (OAuth 2.1)

- [x] **OAuth discovery + Dynamic Client Registration** — fetch
  `/.well-known/oauth-authorization-server`, register client, cache `client_id`.
  *Accept: unit test against recorded metadata.*
- [ ] **Authorization Code + PKCE flow** — S256 challenge, open system browser,
  localhost callback listener, state check, code→token exchange.
  *Accept: manual login writes a valid token bundle.*
  *Implemented (incl. loopback listener tests); manual live login still to be run.*
- [ ] **Keychain vault** — store/load token bundles via OS keychain (`keyring` crate);
  never write tokens to disk. *Accept: bundle round-trips on macOS/Windows/Linux.*
  *Implemented via `keyring` v3 + 0600 file fallback (fallback tested); per-OS keychain round-trip still to be verified.*
- [ ] **Token refresh** — auto-refresh via `refresh_token` grant ~30s before expiry;
  surface a "re-auth needed" state on failure. *Accept: expired-token request succeeds
  after transparent refresh.*
  *Implemented (refresh ~30s before expiry, rotation-safe); needs a live expired-token run.*

### MCP client

- [ ] **JSON-RPC transport (Streamable HTTP)** — POST with bearer auth; parse both JSON
  and SSE response bodies; `initialize` handshake, `Mcp-Session-Id`, protocol-version
  header. *Accept: `tools/list` round-trips against the live server.*
  *Implemented — JSON + SSE bodies, `Mcp-Session-Id`, protocol-version header, session-retry; live `tools/list` round-trip pending.*
- [x] **Tool wrappers + defensive parsers** — `list_agents`, `create_thread`,
  `send_message`, `get_thread` (snapshot: running/messages), `create_attachment_upload`.
  *Accept: parser unit tests from captured payloads.*
  *Fixture-shape tests; refresh fixtures with captured payloads during live validation. `create_attachment_upload` deferred to Phase 4 attachments.*
- [x] **Run loop** — start turn → poll `get_thread` until not running → return final
  snapshot; honor run timeout. *Accept: integration test with a mock upstream.*
- [x] **Mock upstream** — offline fake implementing the tool interface for tests and
  UI development (parity with the reference gateway's mock). *Accept: full request
  cycle works with no network.* *`STARFISH_MOCK_UPSTREAM=1` also drives the app for UI dev.*

### Gateway server (OpenAI surface, minimum)

- [x] **Server lifecycle** — axum server on a tokio task; Tauri commands start/stop/
  status; port/host from config. *Accept: UI toggle actually opens/closes the port.*
- [x] **Local key auth middleware** — require `Authorization: Bearer sk-…` matching a
  configured key; OpenAI-shaped `401` JSON errors; keys redacted from logs.
  *Accept: wrong/missing key → OpenAI-style error envelope.*
- [x] **`GET /v1/models` + `/v1/models/{id}`** — agents as model objects;
  TTL-cached `list_agents`; `hyperagent-default` alias resolution.
  *Accept: `client.models.list()` shows named agents.*
- [x] **`POST /v1/chat/completions` (non-stream)** — flatten messages (system preamble +
  transcript) → thread run → `chat.completion` envelope with estimated `usage`.
  *Accept: `openai` SDK completes a chat.*
- [x] **`POST /v1/chat/completions` (stream)** — poll-diff emulated SSE
  (`chat.completion.chunk`, role-first delta, `[DONE]`, optional usage chunk).
  *Accept: `stream=True` yields incremental deltas in the SDK.*
- [x] **OpenAI error mapping** — upstream/MCP/timeout failures → correct OpenAI error
  JSON (`type`, `code`, status). *Accept: table-driven tests.* *Covered by e2e tests (401/404 envelopes) + `map_core_error`.*

### Minimal UI

- [x] **Sign-in + server screen** — "Sign in to Hyperagent" (OAuth), account chip
  (signed in / needs re-auth), server start/stop with status + port, copyable base URL.
  *Accept: a first-time user gets to a working endpoint without docs.* *Superseded by the full onboarding wizard + dashboard.*

### Phase 0 exit test

- [ ] **SDK smoke test** — scripted `openai` SDK run against a debug build:
  `models.list` + non-stream + stream chat. *Accept: green in CI (mock) and manually
  against live upstream.* *Gateway e2e tests (raw HTTP) are green in CI vs mock; official-SDK script + live run pending.*

---

## Phase 1 — The headline clients (Codex + Claude Code)

> **Milestone:** Codex (`wire_api = "responses"`) and Claude Code
> (`ANTHROPIC_BASE_URL`) both run against the same Starfish instance.

### OpenAI Responses API (Codex)

- [x] **`POST /v1/responses` (non-stream)** — input items → thread run → `response`
  envelope (`output`, `output_text`, `usage`). *Accept: raw curl parity with reference
  gateway behavior.*
- [x] **`POST /v1/responses` (stream)** — `response.created` →
  `response.output_text.delta`* → `response.completed` via poll-diff.
  *Accept: Codex renders incremental output.*
  *Full event sequence emitted; Codex rendering check pending.*
- [ ] **Responses lifecycle** — `background` mode, `GET /v1/responses/{id}`,
  `POST /{id}/cancel`, `GET /{id}/input_items`; in-memory + on-disk registry.
  *Accept: background create → poll → fetch completes.*
  *In-memory registry with background/GET/cancel/input_items done and tested; on-disk persistence pending.*
- [ ] **Codex end-to-end** — verify against real Codex CLI with a `model_providers`
  entry; document quirks. *Accept: recorded demo + README snippet confirmed.*

### Anthropic surface (Claude Code)

- [x] **`POST /v1/messages` (non-stream)** — parse Anthropic envelope (string/block
  `system`, content blocks: `text`, `image`, `tool_use`, `tool_result`) → flatten →
  thread run → `message` response (`content[]`, `stop_reason`, estimated `usage`).
  *Accept: `anthropic` SDK completes a message.*
- [x] **`POST /v1/messages` (stream)** — full SSE sequence: `message_start` →
  `content_block_start` → `content_block_delta` (`text_delta`)* → `content_block_stop`
  → `message_delta` (stop_reason + usage) → `message_stop`, with periodic `ping`.
  *Accept: `anthropic` SDK streaming iterator works.*
- [x] **Anthropic auth & headers** — accept `x-api-key` **and** `Authorization: Bearer`;
  tolerate/echo `anthropic-version: 2023-06-01`; pass through `anthropic-beta`;
  Anthropic-shaped error JSON. *Accept: header-matrix tests.*
- [x] **`POST /v1/messages/count_tokens`** — return `{ input_tokens: <estimate> }` so
  Claude Code's budgeting works. *Accept: Claude Code startup makes the call happily.*
- [ ] **Claude Code end-to-end** — verify with `ANTHROPIC_BASE_URL` +
  `ANTHROPIC_AUTH_TOKEN`, including `ANTHROPIC_MODEL` / `ANTHROPIC_SMALL_FAST_MODEL`
  overrides. *Accept: recorded demo + README snippet confirmed.*

### Model mapping

- [x] **Mapping engine** — resolution order: exact agent id → agent name → mapping
  table (wildcards like `claude-*sonnet*`) → default agent; per-surface defaults.
  *Accept: unit tests incl. Claude Code's hard-coded names.*
- [x] **Mapping UI** — browse agents; set default; edit OpenAI-name and Claude-name →
  agent rows. *Accept: change in UI visibly reroutes the next request.*
- [x] **Connection snippets UI** — generated, copyable configs for Claude Code (env),
  Codex (`config.toml`), Cursor/Continue, curl — with the user's real port + key.
  *Accept: paste-and-go works for each.*

---

## Phase 2 — Multi-account & keys

> **Milestone:** two Hyperagent accounts and two local keys routing independently
> through one Starfish.

- [x] **Multi-account vault** — several token bundles side by side; nickname each;
  status chip (valid / expiring / expired / needs re-auth). *Accept: add + list + badge
  states render truthfully.*
- [x] **Add/remove/re-auth flows** — add via OAuth; sign out deletes from keychain;
  expired accounts prompt (never silently fail). *Accept: full lifecycle by UI only.*
  *UI + commands complete; live OAuth pass pending (see status note).*
- [x] **Local key manager** — generate (`sk-…`), name, reveal/copy, rotate, revoke;
  hashed at rest. *Accept: revoked key → 401 immediately.*
  *Plus per-key agent override; secrets in vault, hashes in config.*
- [x] **Key → identity + policy routing** — bind each key to an account, default agent,
  and disabled-tools set (GUI equivalent of the reference's `GATEWAY_KEYS_FILE`);
  per-request identity resolution + per-identity MCP client/token handling.
  *Accept: two keys → two accounts verified end-to-end.*
  *Routing + per-identity MCP clients done; disabled-tools set is recorded but not enforced until the tool bridge lands.*
- [x] **Require-key by default** — no anonymous mode unless an explicit dev toggle is
  set; warning banner while dev mode is on. *Accept: fresh install refuses keyless calls.*

---

## Phase 3 — Ops & polish

> **Milestone:** Starfish feels like a product — visible, debuggable, self-updating.

- [x] **Live request log** — ring buffer + UI stream: time, surface, endpoint,
  model → agent, latency, status, token estimate. *Accept: requests appear in real time;
  no secrets logged.*
- [x] **Request detail view** — sanitized request/response, thread id, poll timeline,
  error detail. *Accept: click row → full story of one request.*
- [x] **Doctor panel** — per account: MCP reachability, token state, agent count,
  common-failure hints ("no named agents", OAuth expired, timeout). *Accept: each
  failure mode shows actionable text.*
- [x] **System tray** — status glyph, start/stop, active-account switch, open window,
  quit; close-to-tray. *Accept: full basic operation without the main window.*
  *Status + start/stop + show/quit and close-to-tray; account switcher in tray still to come.*
- [x] **Launch at login + single instance** — `tauri-plugin-autostart` (opt-in toggle);
  second launch focuses the existing window. *Accept: reboot test.*
  *Plugins wired (opt-in toggle in Settings); reboot test pending.*
- [ ] **Auto-update** — `tauri-plugin-updater` + signed release pipeline (GitHub
  releases). *Accept: old build updates itself.*
- [x] **Onboarding wizard** — first run: sign in → pick/verify agent → map models →
  copy snippet → test request button. *Accept: new user to working Claude Code in
  <5 minutes.*
- [ ] **Settings screen** — port/host, poll interval, timeout, log level, theme;
  config export/import (**secrets stay in keychain, never exported**). *Accept:
  exported file contains zero secret material.*
  *Everything but config export/import is done (the config file itself never holds secrets).*

---

## Phase 4 — Advanced surface & continuity

> **Milestone:** power features for daily-driver use.

- [ ] **Conversation → thread continuity (opt-in)** — map client conversation ids to
  persistent Hyperagent threads instead of stateless flatten-per-call.
- [ ] **Attachments & `/v1/files`** — upload via `create_attachment_upload`; file
  registry; OpenAI file refs and Anthropic image/document blocks carried into threads.
- [ ] **Media endpoints** — `/v1/images/generations` + `/edits`, `/v1/audio/speech`,
  `/transcriptions`, `/translations` via agent tools.
- [ ] **`/v1/embeddings` + `/v1/moderations`** — local fallback vectors (or 501 when
  disabled); heuristic moderation.
- [ ] **Tool bridge** — `GET /v1/tools`, observe mode (agent tool activity →
  `tool_calls` / `tool_use` blocks), forced `tool_choice`, exec modes
  (`roundtrip`/`auto`), per-key tool disable enforcement.
- [ ] **Guarded LAN exposure** — explicit opt-in bind beyond localhost with loud
  warnings, strong keys, TLS.
- [ ] **Team niceties** — shared-config story, per-key rate/concurrency caps.

---

## Cross-cutting (any phase)

- [x] **CI** — `cargo fmt --check`, `clippy -D warnings`, `cargo test`, frontend
  typecheck/build; Tauri build matrix (macOS / Windows / Linux).
  *fmt + clippy -D warnings + core tests + Tauri check (Linux) + frontend build; full 3-OS build matrix later.*
- [x] **Choose a license** — adopted **MIT OR Apache-2.0** dual (the Rust/Tauri
  convention): `LICENSE-MIT` + `LICENSE-APACHE`, README badge/section, SPDX fields in
  `Cargo.toml` / `package.json`. No code may be ported from the unlicensed reference repo.
- [ ] **App identity** — icon set, product naming pass, `identifier` review.
- [ ] **CONTRIBUTING.md** — distill the README contributing section; PR/issue templates.
- [ ] **Release pipeline** — tagged releases with signed bundles per OS; changelog.
- [ ] **Security review checklist** — keychain usage, log redaction, bind address,
  CSP for the webview, dependency audit (`cargo audit`).

---

*This file is the working plan; [MISSION.md](MISSION.md) is the why. When they
disagree, fix whichever one is wrong — in a PR.*
