# Starfish

**Your Hyperagent agents, behind the two APIs every AI tool already speaks.**

![Status](https://img.shields.io/badge/status-pre--alpha_·_design_phase-orange)
![Tauri](https://img.shields.io/badge/Tauri-v2-24C8DB?logo=tauri&logoColor=white)
![Rust](https://img.shields.io/badge/core-Rust-DEA584?logo=rust&logoColor=white)
![Frontend](https://img.shields.io/badge/UI-React_+_TypeScript-3178C6?logo=typescript&logoColor=white)
![License](https://img.shields.io/badge/license-TBD-lightgrey)

Starfish is a cross-platform desktop app (Tauri v2) that signs in to your
[Hyperagent](https://hyperagent.com) account(s) and serves **local OpenAI- and
Anthropic-compatible API endpoints**. Point Codex, Claude Code, Cursor, Continue, or any
OpenAI/Anthropic SDK at `http://127.0.0.1:8787` with a Starfish key — and the "model"
answering is a full Hyperagent **agent** that can search the web, run code, drive a
browser, generate media, and call your integrations.

```text
Codex ──── OPENAI_BASE_URL ────┐
Claude Code ─ ANTHROPIC_BASE_URL ─┤──▶  STARFISH (local gateway + GUI)  ──▶  Hyperagent MCP  ──▶  your agent
Cursor / SDKs / any client ────┘        accounts · keys · model mapping · logs
```

> **⚠️ Status: pre-alpha.** There is nothing to install yet — this repo currently holds
> the project's design ([MISSION.md](MISSION.md)), the build plan ([ROADMAP.md](ROADMAP.md)),
> and a bare Tauri v2 scaffold. Star/watch the repo to follow along, or grab a roadmap
> item and help build it.

---

## Why

The OpenAI and Anthropic wire formats are the two universal power sockets of AI tooling —
almost every IDE plugin, agent CLI, and chat UI speaks one or both. Hyperagent's only
public programmatic door is a hosted **MCP server**, a protocol none of those tools speak.

Starfish is the wall socket: your tools plug in the way they always have; behind the
wall, the electricity comes from Hyperagent.

It's the GUI successor to the terminal-only
[`hyperagent-openai-gateway`](https://github.com/dinhhung893/hyperagent-openai-gateway),
adding the three things a CLI can't give you:

| | CLI gateway (reference) | **Starfish** |
| --- | --- | --- |
| Interface | Terminal, `.env` files | Desktop GUI, tray, onboarding wizard |
| API surface | OpenAI only | **OpenAI + Anthropic** (Claude Code works) |
| Credentials | Plaintext JSON (`0600`) | **OS keychain**, multi-account |
| Client keys | Env var / JSON file | Visual key manager with per-key routing |

## What it will do

- **Sign in to Hyperagent in-app** (OAuth 2.1 + PKCE) — tokens live in the OS keychain,
  refresh automatically, and multiple accounts sit side by side.
- **Serve both API surfaces on one local port**, path-routed:
  - OpenAI: `/v1/models`, `/v1/chat/completions`, `/v1/responses` (Codex's default wire API)
  - Anthropic: `/v1/messages`, `/v1/messages/count_tokens` (what Claude Code calls)
- **Map models to agents** — `GET /v1/models` lists your named agents; a mapping table
  resolves hard-coded client model names (like Claude Code's `claude-*`) to the agent you choose.
- **Manage local API keys** — generate, rotate, revoke; bind each key to an account +
  policy (default agent, disabled tools).
- **Watch it work** — live request log (endpoint, model → agent, latency, status), health
  checks, and clear errors instead of silent failures.
- **Behave like a good desktop citizen** — system tray, launch-at-login, auto-update.

How it works under the hood (thread-per-request, emulated streaming, poll-based runs) is
covered in [MISSION.md §4](MISSION.md#4-how-it-works-architecture).

## Connecting your tools (planned UX)

Once Starfish runs, connecting a client is two values — a base URL and a key. The app
generates these snippets for you:

**Claude Code**

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
export ANTHROPIC_AUTH_TOKEN=sk-starfish-your-local-key
claude   # backed by the Hyperagent agent you mapped
```

**Codex CLI** (`~/.codex/config.toml`)

```toml
[model_providers.starfish]
name = "Starfish (Hyperagent)"
base_url = "http://127.0.0.1:8787/v1"
env_key = "STARFISH_API_KEY"
wire_api = "responses"

[profiles.starfish]
model_provider = "starfish"
model = "hyperagent-default"
```

**Any OpenAI SDK**

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:8787/v1", api_key="sk-starfish-your-local-key")
r = client.chat.completions.create(
    model="hyperagent-default",
    messages=[{"role": "user", "content": "Research X and summarize."}],
)
```

> Requests run a full agent pipeline — expect seconds, not milliseconds. Streaming is
> emulated and token counts are estimates (the upstream exposes neither); Starfish is
> honest about both.

## Roadmap

Development happens in phases, each ending in a usable milestone:

| Phase | Ships | Milestone |
| --- | --- | --- |
| **0 — Foundation** | OAuth login, MCP client, `/v1/models` + `/v1/chat/completions` | An OpenAI SDK works against Starfish |
| **1 — The headline clients** | `/v1/responses` + the Anthropic surface + model mapping | **Codex and Claude Code both work** |
| **2 — Multi-account & keys** | Account switcher, key manager, per-key routing | One Starfish, many identities |
| **3 — Ops & polish** | Request log, doctor, tray, autostart, updater, onboarding | Feels like a product |
| **4 — Advanced** | Files/media endpoints, tool bridge, thread continuity, LAN+TLS | Power features |

The full issue-sized breakdown lives in **[ROADMAP.md](ROADMAP.md)**.

## Development

Starfish is a standard Tauri v2 app: React 19 + TypeScript + Vite on the front,
Rust in `src-tauri/` (where the gateway core will live).

**Prerequisites**

- [Rust](https://rustup.rs) (stable)
- [Bun](https://bun.sh)
- Tauri v2 [platform prerequisites](https://v2.tauri.app/start/prerequisites/)
  (WebView2 on Windows, Xcode CLT on macOS, `webkit2gtk` etc. on Linux)

**Run**

```bash
bun install
bun run tauri dev     # dev app with hot reload
bun run tauri build   # production bundle
```

**Layout**

```text
src/           React UI
src-tauri/     Rust core — Tauri commands; gateway server, OAuth, MCP client will live here
MISSION.md     the why, the architecture, the full feature set
ROADMAP.md     the plan, checkbox by checkbox
```

## Contributing

Early contributors shape the foundations — welcome.

1. Read [MISSION.md](MISSION.md) (10 minutes) so we share a mental model.
2. Pick an unclaimed checkbox from [ROADMAP.md](ROADMAP.md) — respect phase order for
   anything with dependencies — and open an issue to claim it.
3. Keep PRs focused (one roadmap item ≈ one PR). Conventional commits
   (`feat:`, `fix:`, `docs:`…) appreciated. `cargo fmt` + `cargo clippy` clean.
4. Design debates live in issues/discussions, decisions get reflected back into
   MISSION.md.

Security posture is non-negotiable: localhost-only by default, secrets in the OS
keychain, client keys required, no credentials in logs. See
[MISSION.md §7](MISSION.md#7-security--responsible-use).

## Provenance & fair use

Starfish stands on the shoulders of
[`dinhhung893/hyperagent-openai-gateway`](https://github.com/dinhhung893/hyperagent-openai-gateway),
which proved the OpenAI↔MCP adapter mechanism. Starfish re-homes those ideas in a
desktop app, adds the Anthropic surface, and moves secrets into the keychain.

Starfish routes traffic through **your own authenticated Hyperagent account(s)**. It is
not a tool for evading usage limits or reselling access; you are responsible for
complying with Hyperagent's Terms of Service and those of any client you connect.

> "OpenAI" and "Anthropic" are trademarks of their respective owners. Starfish is an
> independent compatibility layer, not affiliated with OpenAI, Anthropic, or Hyperagent.

## License

**TBD.** No license has been chosen yet — until one is added, all rights are reserved.
If you'd like to contribute and the missing license blocks you, open an issue.
