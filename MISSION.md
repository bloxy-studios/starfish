# Starfish — Mission

> **Starfish is a cross-platform desktop app that turns your Hyperagent account(s)
> into a local, drop-in OpenAI *and* Anthropic API.** Sign in once, flip a switch,
> and point Codex, Claude Code, Cursor, Continue, or any OpenAI/Anthropic client at
> `http://127.0.0.1:8787` — the "model" answering is a full Hyperagent agent that can
> search the web, run code, drive a browser, generate media, and call your integrations.

---

## 1. The one-liner

**Use your Hyperagent agents from any OpenAI- or Anthropic-compatible tool, managed from a real GUI.**

Starfish is the friendly, secure, multi-account successor to the terminal-only
[`hyperagent-openai-gateway`](https://github.com/dinhhung893/hyperagent-openai-gateway).
It keeps that project's core idea — an adapter that speaks a de-facto-standard API on
the front and Hyperagent's MCP server on the back — and adds the three things that
project can't give you:

1. **A desktop GUI** for logins, accounts, keys, model mapping, and live logs — no
   `.env` files, no `export`, no remembering CLI flags.
2. **Anthropic (Claude Code) compatibility** via `/v1/messages` — the reference
   gateway is OpenAI-only, so Claude Code has nowhere to point today. Starfish fixes that.
3. **Secure, multi-account credential management** in the OS keychain, with per-key
   identity routing surfaced visually instead of through a hand-edited JSON file.

---

## 2. Why this exists

**The OpenAI and Anthropic wire formats are the two universal power sockets of the AI
tooling world.** Almost every IDE plugin, agent CLI, and chat UI knows how to talk to
one or both. **Hyperagent's only public programmatic door is a hosted MCP server** —
powerful, but a language none of those tools speak.

Starfish is the wall socket. Your device (Codex, Claude Code, Cursor…) plugs in exactly
the way it always has. Behind the wall, the electricity comes from Hyperagent.

Concretely, that unlocks:

- **Claude Code, powered by a Hyperagent agent.** Set `ANTHROPIC_BASE_URL` and go. Your
  agent's web search, shell, browser, and integrations back every Claude Code turn.
- **Codex / OpenAI-CLI, powered by a Hyperagent agent.** Set `OPENAI_BASE_URL` and go.
- **Every OpenAI/Anthropic app you already use**, with zero code changes — just a new
  base URL and API key.

The reference project proved the mechanism works. Starfish makes it a product: safe to
run, easy to operate, and usable by tools on *both* sides of the OpenAI/Anthropic divide.

---

## 3. Who it's for

- **Solo developers** who live inside Codex / Claude Code / Cursor and want their
  Hyperagent agent as the backend without touching a terminal.
- **People juggling multiple Hyperagent accounts** (personal + work seat, or several
  named agents) who need clean switching and secure storage.
- **Small teams** who want one machine (or one trusted box) to expose a shared,
  policy-controlled gateway to their tools.
- **Anyone who tried the CLI gateway** and wanted a real app — especially Claude Code
  users the CLI never supported.

---

## 4. How it works (architecture)

```text
┌────────────────────────────────────────────────────────────────────────┐
│  Client tools                                                            │
│  • Codex / OpenAI CLI  → OPENAI_BASE_URL   → /v1/chat  /v1/responses     │
│  • Claude Code         → ANTHROPIC_BASE_URL→ /v1/messages  /count_tokens │
│  • Cursor / Continue / LibreChat / openai & anthropic SDKs               │
└───────────────┬──────────────────────────────────────────────────────────┘
                │ HTTP on 127.0.0.1:8787   (Bearer sk-… or x-api-key: sk-…)
                ▼
┌────────────────────────────────────────────────────────────────────────┐
│  STARFISH  (Tauri v2 desktop app)                                        │
│                                                                          │
│  React/TS UI  ⇄  Rust core (Tauri commands + events)                     │
│                                                                          │
│  Rust core hosts:                                                        │
│   • Local HTTP gateway (axum/hyper) — ONE port, path-routed:             │
│       – OpenAI surface   /v1/chat/completions, /v1/responses, /v1/models…│
│       – Anthropic surface /v1/messages, /v1/messages/count_tokens        │
│   • Auth: local API key  →  resolves to a Hyperagent Identity + policy   │
│   • Translator: OpenAI/Anthropic wire shapes  ⇄  Hyperagent thread ops   │
│   • MCP client: JSON-RPC 2.0 over Streamable HTTP + OAuth 2.1 tokens     │
│   • Credential vault: OS keychain (Keychain / Cred Manager / Secret Svc) │
└───────────────┬──────────────────────────────────────────────────────────┘
                │ MCP JSON-RPC 2.0 over HTTPS  (OAuth 2.1 Bearer, auto-refresh)
                ▼
        Hyperagent MCP server   https://hyperagent.com/api/mcp
                │  list_agents · create_thread · get_thread (poll) · send_message · …
                ▼
        Your Hyperagent agent runs the request end-to-end
        (web search · browser · shell · files · images/audio · integrations)
```

### Four load-bearing ideas (inherited from the reference, kept intact)

1. **`model` = a Hyperagent *named* agent.** `GET /v1/models` lists your agents; the
   client picks one as the `model` (or uses the `hyperagent-default` alias). The MCP
   server only starts threads on **named agents**, so an account needs at least one.
2. **A request = a thread run.** Hyperagent runs work in the background, so the gateway
   **polls `get_thread`** until the run finishes, then renders the reply.
3. **Streaming is emulated.** Hyperagent doesn't push tokens, so Starfish diffs the
   assistant message across polls and emits standard SSE — OpenAI `chat.completion.chunk`
   for the OpenAI surface, Anthropic `content_block_delta` events for the Anthropic surface.
4. **Stateless by design.** Each call is self-contained; conversation context is carried
   in the request (flattened transcript) rather than relying on fragile upstream memory.
   An optional local map (client conversation → Hyperagent `threadId`) can enable true
   thread continuity later.

### The Hyperagent handshake (what the Rust core must implement)

- **OAuth 2.1**: discovery at `https://hyperagent.com/.well-known/oauth-authorization-server`
  → Dynamic Client Registration → Authorization Code + **PKCE (S256)** → token exchange.
  Scopes: `threads:read threads:write approvals:read approvals:write offline_access`.
  Store the bundle `{access_token, refresh_token, expires_at, client_id, token_endpoint}`
  and refresh via the `refresh_token` grant ~30s before expiry.
- **MCP transport**: JSON-RPC 2.0 over HTTP POST (Streamable HTTP; response may be JSON
  **or** an SSE stream — handle both). `initialize` handshake, honor `Mcp-Session-Id`,
  send `MCP-Protocol-Version`. Core tools: `list_agents`, `create_thread(agentId, message,
  attachmentIds?)`, `send_message(threadId, message)`, `get_thread(threadId)`,
  `create_attachment_upload(filename, mimeType, sizeBytes)`.

### Architecture decision: Rust-native core (recommended) vs. Python sidecar

Starfish already ships a Tauri v2 + Rust backend, so the **north-star is a Rust-native
gateway** living in `src-tauri` (`axum`/`hyper` server on a `tokio` task, `reqwest` for
MCP + OAuth, `keyring` for secrets, `tauri-plugin-*` for tray/deep-link/autostart/updater).
Benefits: one self-contained binary, no bundled Python runtime, native OS-keychain
security, best desktop UX.

> **Pragmatic fallback:** to reach a working MVP faster, the mature Python reference
> gateway can be bundled as a **Tauri sidecar** (packaged with PyInstaller) that the Rust
> app spawns and supervises, with the GUI managing accounts and process lifecycle. This
> trades a heavier bundle and two languages for reusing tested code. Treat it as a
> temporary accelerator, not the destination — the Anthropic surface and keychain vault
> should be built the Rust-native way regardless.

---

## 5. The two API surfaces

Both surfaces are served on **one local port** and distinguished by path, so a single
Starfish instance simultaneously backs Codex (OpenAI paths) and Claude Code (Anthropic
paths). Model strings on either surface resolve to a Hyperagent agent via the mapping UI.

### 5a. OpenAI-compatible surface (Codex, Cursor, Continue, `openai` SDK)

| Endpoint | Purpose | MVP |
| --- | --- | --- |
| `GET /v1/models`, `/v1/models/{id}` | List account's Hyperagent agents as models | ✅ |
| `POST /v1/chat/completions` (stream + non-stream) | Core chat | ✅ |
| `POST /v1/responses` (+ background, cancel, input_items) | **Codex CLI default wire API** | ✅ |
| `POST /v1/completions` (legacy) | Legacy text completion | later |
| `GET /v1/tools` + forced `tool_choice` | Tool bridge catalog | later |
| `POST /v1/embeddings` | Local fallback vectors (or 501) | later |
| `POST /v1/images/generations`, `/images/edits` | Media via agent tools | later |
| `POST /v1/audio/speech`, `/transcriptions`, `/translations` | Audio via agent tools | later |
| `POST /v1/files`, `GET/DELETE /v1/files/{id}`, `/content` | Attachments | later |
| `POST /v1/moderations` | Heuristic moderation | later |

**Codex connects with:** `OPENAI_BASE_URL=http://127.0.0.1:8787/v1`, a Starfish local key,
and a `~/.codex/config.toml` `model_providers` entry (`base_url`, `env_key`,
`wire_api = "responses"`). Starfish generates this snippet for one-click copy.

### 5b. Anthropic-compatible surface (Claude Code, `anthropic` SDK) — the headline feature

| Endpoint | Purpose | MVP |
| --- | --- | --- |
| `POST /v1/messages` (stream + non-stream) | Claude Code's core endpoint | ✅ |
| `POST /v1/messages/count_tokens` | Claude Code calls this before/around turns | ✅ |
| Model listing behavior | Answer Claude Code's model expectations via mapping | ✅ |

**What the translator must do on this surface:**

- **Headers:** accept `x-api-key: <key>` (Claude Code's default) *and* `Authorization:
  Bearer <key>`; accept and echo `anthropic-version: 2023-06-01` and pass-through
  `anthropic-beta`.
- **Request → thread:** flatten Anthropic `system` (string or blocks) into a preamble;
  map `messages[]` with content blocks (`text`, `image`, `tool_use`, `tool_result`) into
  a self-contained Hyperagent message. Preserve turn order.
- **Response (non-stream):** render
  `{ id:"msg_…", type:"message", role:"assistant", model, content:[{type:"text", text}],
  stop_reason:"end_turn"|"tool_use"|"max_tokens"|"stop_sequence", stop_sequence:null,
  usage:{ input_tokens, output_tokens } }`. Token counts are best-effort estimates
  (Hyperagent doesn't expose exact counts) — clearly labeled as such.
- **Response (stream):** emit the Anthropic SSE event sequence via poll-diff —
  `message_start` → `content_block_start` → repeated `content_block_delta` (`text_delta`)
  → `content_block_stop` → `message_delta` (with `stop_reason` + `usage`) → `message_stop`,
  with periodic `ping` events to keep the connection alive.
- **`count_tokens`:** return `{ input_tokens: <estimate> }` so Claude Code's budgeting
  doesn't break.

**Claude Code connects with:** `ANTHROPIC_BASE_URL=http://127.0.0.1:8787`,
`ANTHROPIC_AUTH_TOKEN=<Starfish local key>` (or `ANTHROPIC_API_KEY`). Because Claude Code
hard-codes `claude-*` model names, Starfish provides a **model-mapping table** (e.g.
`claude-*-sonnet-* → <agent A>`, and the small/fast model via `ANTHROPIC_SMALL_FAST_MODEL`
→ `<agent B>`) so those names resolve to your chosen agents.

---

## 6. Feature set

Grouped by capability; the roadmap (§8) sequences them.

### A. Account & identity management
- Add a Hyperagent account via an **in-app OAuth browser flow** (Auth Code + PKCE + DCR),
  handling the redirect either through a localhost callback listener or a custom
  URI-scheme deep link (`tauri-plugin-deep-link`).
- **Secure vault**: token bundles stored in the **OS keychain**, never plaintext.
- Multiple accounts side by side — nickname each, show identity/email, and a status chip
  (valid / expiring / expired / needs re-auth).
- **Automatic token refresh** with expiry tracking; graceful re-auth prompts on failure.
- Sign out / remove an account (delete from keychain).

### B. Local API key management (client-facing)
- Generate, name, rotate, reveal, copy, and revoke local gateway keys (`sk-…`).
- **Route each key → a Hyperagent identity + policy** (default agent, disabled tools) —
  the GUI equivalent of the reference's `GATEWAY_KEYS_FILE`, so one Starfish can serve
  several accounts/users at once.
- Secure-by-default: require a key (no "accept any key" unless an explicit dev toggle).

### C. Gateway server engine
- Start / stop / restart the local server; configurable host + port (default
  `127.0.0.1:8787`); status indicator.
- Both API surfaces (§5) on one port via path routing; emulated streaming for each.
- Agent resolution cache (`list_agents` with TTL); `hyperagent-default` alias.
- Configurable poll interval and run timeout.

### D. Agent / model mapping
- Browse the active account's agents (name, description).
- Assign the default agent; build the **OpenAI-model → agent** and **Claude-model → agent**
  maps (the latter is essential for Claude Code).

### E. Tool-bridge controls
- Expose Hyperagent's tool activity through OpenAI `tool_calls` / Anthropic `tool_use`
  (observe / direct / run modes); toggle exposed vs disabled tools globally and per key;
  choose exec mode (`roundtrip` vs `auto`).

### F. Observability & ops
- **Live request log**: method, endpoint, model → agent, latency, status, token estimate,
  with a detail view (request/response, thread id, poll timeline).
- Per-account **health / doctor** panel (MCP reachability, agent list, token state).
- Clear surfacing of the common failure modes: OAuth expiry, MCP errors, "no named
  agents", run timeouts.
- **One-click connection snippets** for Codex, Claude Code, Cursor, and raw SDK/curl.

### G. System integration & UX
- **System-tray** presence: quick start/stop, active-account switch, status; run in
  background; **launch-at-login** (`tauri-plugin-autostart`).
- Single-instance; deep-link OAuth handler; **auto-update** (`tauri-plugin-updater`).
- First-run **onboarding wizard**: sign in → pick agents → map models → copy config → done.
- Settings (port/host, poll interval, timeout, log level, theme); export/import config
  (secrets always stay in the keychain, never in the export).

### H. Security & responsible use — see §7.

---

## 7. Security & responsible use

**Security posture**
- **Localhost-only by default.** Bind to `127.0.0.1`; exposing on `0.0.0.0`/LAN requires
  an explicit, warned opt-in (and, when added, TLS + strong keys).
- **Secrets live in the OS keychain**, not on disk. If a token is ever exported, write it
  `0600` and warn.
- **A client key is required by default.** Redact keys and tokens in all logs.
- Optional per-key rate-limit / concurrency caps to protect the upstream account.

**Responsible use.** Starfish routes traffic through **your own authenticated Hyperagent
account(s)** and is intended for legitimate personal, developer, and team use — switching
between accounts you hold, and giving your own tools an agent backend. It is **not** a tool
for evading usage limits, sharing one identity in violation of terms, or reselling access.
Every user is responsible for complying with Hyperagent's Terms of Service and with the
terms of any client tool they connect. Starfish should make the compliant path the easy
path (clear per-account attribution, honest "estimated token" labeling, no limit-evasion
features) and surface a short responsible-use note during onboarding.

---

## 8. Roadmap

**Phase 0 — Foundation (Rust core online)**
- OAuth 2.1 (discovery + DCR + PKCE) and keychain-backed single account.
- MCP client (`initialize`, `list_agents`, `create_thread`, `get_thread` poll, `send_message`).
- OpenAI `GET /v1/models` + `POST /v1/chat/completions` (stream + non-stream).
- Minimal UI: sign in, start/stop server, see status. **Milestone: Cursor/`openai` SDK works.**

**Phase 1 — The two clients that motivated this**
- `POST /v1/responses` (+ background/cancel/input_items) → **Codex** works.
- `POST /v1/messages` (stream + non-stream) + `/v1/messages/count_tokens` → **Claude Code** works.
- Model-mapping UI (incl. Claude's hard-coded names). One-click connection snippets.
- **Milestone: Codex and Claude Code both run against Starfish.**

**Phase 2 — Multi-account & keys**
- Multiple accounts; local key management; per-key identity + policy routing; tool
  enable/disable per key.

**Phase 3 — Ops & polish**
- Live request logs + detail view; per-account health/doctor; system tray; launch-at-login;
  auto-update; onboarding wizard.

**Phase 4 — Advanced surface & continuity**
- Optional conversation → persistent-thread mapping; images/audio/files/embeddings/
  moderations endpoints; tool-bridge "run" mode; guarded LAN exposure with TLS; team features.

---

## 9. Non-goals (for now)

- **Not** a from-scratch reimplementation of Hyperagent — it's an adapter to the hosted
  MCP server.
- **Not** true token-level streaming or exact token accounting (upstream doesn't expose
  either; Starfish emulates and estimates, and says so).
- **Not** a hosted/multi-tenant SaaS in v1 — it's a local desktop app (a hardened shared
  deployment is a later, opt-in concern).
- **Not** a general LLM router across arbitrary providers — the upstream is Hyperagent.

---

## 10. What "done" looks like (success criteria)

1. A non-technical-enough user installs Starfish, clicks **Sign in**, and is authenticated
   to Hyperagent with tokens safely in the keychain — no terminal, no files.
2. With the server running, **Codex** (via `OPENAI_BASE_URL`) and **Claude Code** (via
   `ANTHROPIC_BASE_URL`) both work against the *same* Starfish instance, each backed by the
   Hyperagent agent chosen in the mapping UI.
3. The user can add a **second account**, mint a **second local key** bound to it, and
   switch the active identity from the tray — all visible, all secure.
4. When something breaks (expired token, no named agent, timeout), the UI says **what**
   broke and **how to fix it**, instead of failing silently.

---

## 11. Provenance & credits

Starfish stands on the shoulders of
[`dinhhung893/hyperagent-openai-gateway`](https://github.com/dinhhung893/hyperagent-openai-gateway),
which established the OpenAI-compatibility mechanism, the MCP/OAuth handshake, the
stateless translate-and-poll model, and the emulated-streaming approach. Starfish
re-homes those ideas in a Tauri desktop app, adds the Anthropic/Claude Code surface,
and replaces `.env` + `0600` JSON with a GUI and the OS keychain.

> "OpenAI" and "Anthropic" are trademarks of their respective owners. Starfish is an
> independent compatibility layer and is not affiliated with, endorsed by, or sponsored
> by OpenAI, Anthropic, or Hyperagent.
