import { useMemo, useState } from "react";
import { api } from "../api";
import type { AppSnapshot } from "../types";
import { CodeBlock, EmptyState, Section } from "../ui";
import {
  anthropicSdkSnippet,
  claudeCodeSnippet,
  codexSnippet,
  cursorSnippet,
  curlSnippet,
  pythonSnippet,
} from "../snippets";

const CLIENTS = [
  { id: "claude-code", label: "Claude Code" },
  { id: "codex", label: "Codex CLI" },
  { id: "cursor", label: "Cursor" },
  { id: "openai-sdk", label: "OpenAI SDK" },
  { id: "anthropic-sdk", label: "Anthropic SDK" },
  { id: "curl", label: "curl" },
] as const;

type ClientId = (typeof CLIENTS)[number]["id"];

export function Connect({ snap }: { snap: AppSnapshot }) {
  const [client, setClient] = useState<ClientId>("claude-code");
  const [keyId, setKeyId] = useState<string>("");
  const [secret, setSecret] = useState<string | undefined>();
  const activeKeys = snap.keys.filter((k) => !k.revoked);
  const baseUrl = snap.server_status.base_url;

  const embedKey = async (id: string) => {
    setKeyId(id);
    setSecret(id ? await api.revealKey(id).catch(() => undefined) : undefined);
  };

  const input = useMemo(
    () => ({ baseUrl, key: secret, model: "hyperagent-default" }),
    [baseUrl, secret]
  );

  const snippet = {
    "claude-code": claudeCodeSnippet(input),
    codex: codexSnippet(input),
    cursor: cursorSnippet(input),
    "openai-sdk": pythonSnippet(input),
    "anthropic-sdk": anthropicSdkSnippet(input),
    curl: curlSnippet(input),
  }[client];

  const note = {
    "claude-code":
      "Claude Code hard-codes claude-* model names — add the Claude Code preset in Models so they resolve to your agent. Requests run a full agent turn: expect seconds, not milliseconds.",
    codex:
      "Codex speaks the Responses API (wire_api = \"responses\"), which Starfish serves natively — including background mode and cancel.",
    cursor: "Cursor's custom-OpenAI settings work with any model id from GET /v1/models.",
    "openai-sdk": "Works with any OpenAI-compatible SDK — just base_url + api_key.",
    "anthropic-sdk": "The Anthropic SDK accepts base_url; streaming works too.",
    curl: "Both surfaces, one port — handy for a quick smoke test.",
  }[client];

  return (
    <Section title="Connect a client">
      {activeKeys.length === 0 ? (
        <EmptyState icon="🔌" title="Create a key first">
          Snippets embed a real key once you have one — head to Keys.
        </EmptyState>
      ) : (
        <>
          <div className="connect-controls">
            <div className="tabs">
              {CLIENTS.map((c) => (
                <button
                  key={c.id}
                  className={`tab ${client === c.id ? "active" : ""}`}
                  onClick={() => setClient(c.id)}
                >
                  {c.label}
                </button>
              ))}
            </div>
            <select value={keyId} onChange={(e) => embedKey(e.target.value)}>
              <option value="">placeholder key</option>
              {activeKeys.map((k) => (
                <option key={k.id} value={k.id}>
                  embed: {k.name}
                </option>
              ))}
            </select>
          </div>
          <p className="dim">{note}</p>
          <CodeBlock code={snippet} caption={secret ? "contains your real key — paste, don't screenshot" : "replace the placeholder key"} />
          {!snap.server_status.running && (
            <div className="banner banner-warn">
              The gateway isn't running — start it from the Dashboard before connecting.
            </div>
          )}
        </>
      )}
    </Section>
  );
}
