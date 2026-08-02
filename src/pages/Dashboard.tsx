import { useEffect, useState } from "react";
import { api } from "../api";
import type { AppSnapshot, LogEntry } from "../types";
import { CopyButton, EmptyState, Pill, Section, fmtLatency, fmtTime, tokenTone } from "../ui";

export function Dashboard({
  snap,
  logs,
  refresh,
  go,
}: {
  snap: AppSnapshot;
  logs: LogEntry[];
  refresh: () => void;
  go: (page: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const status = snap.server_status;
  const activeKeys = snap.keys.filter((k) => !k.revoked);

  useEffect(() => setError(null), [status.running]);

  const toggle = async () => {
    setBusy(true);
    setError(null);
    try {
      if (status.running) await api.serverStop();
      else await api.serverStart();
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className={`hero ${status.running ? "hero-on" : ""}`}>
        <div className="hero-main">
          <div className="hero-status">
            <span className={`beacon ${status.running ? "on" : ""}`} />
            <h1>{status.running ? "Gateway running" : "Gateway stopped"}</h1>
          </div>
          <div className="hero-url mono">
            {status.base_url}
            {status.running && <CopyButton text={status.base_url} />}
          </div>
          <div className="hero-sub">
            OpenAI surface <code>/v1/chat/completions · /v1/responses</code> — Anthropic surface{" "}
            <code>/v1/messages</code>
          </div>
        </div>
        <div className="hero-actions">
          <button
            className={`btn ${status.running ? "btn-danger" : "btn-primary"} btn-lg`}
            onClick={toggle}
            disabled={busy}
          >
            {busy ? "…" : status.running ? "Stop" : "Start gateway"}
          </button>
        </div>
      </div>
      {error && <div className="banner banner-err">{error}</div>}
      {snap.mock && (
        <div className="banner banner-warn">
          Mock upstream active (STARFISH_MOCK_UPSTREAM) — requests are answered by an offline
          fake, not Hyperagent.
        </div>
      )}
      {status.allow_anonymous && (
        <div className="banner banner-warn">
          Dev mode: the gateway accepts requests <b>without a key</b>. Turn this off in Settings
          before doing anything real.
        </div>
      )}
      {snap.vault_backend === "file" && (
        <div className="banner banner-warn">
          No OS keychain available — secrets are stored in a 0600 file instead. Fine for
          development, not ideal for daily use.
        </div>
      )}

      <div className="grid-2">
        <Section
          title="Accounts"
          aside={
            <button className="btn btn-ghost btn-sm" onClick={() => go("accounts")}>
              Manage →
            </button>
          }
        >
          {snap.accounts.length === 0 ? (
            <EmptyState icon="⭐" title="No Hyperagent account yet">
              <button className="btn btn-primary" onClick={() => go("accounts")}>
                Sign in to Hyperagent
              </button>
            </EmptyState>
          ) : (
            <ul className="rowlist">
              {snap.accounts.map((a) => (
                <li key={a.id}>
                  <span className="row-title">{a.nickname}</span>
                  <Pill tone={tokenTone(a.token_state)}>{a.token_state}</Pill>
                </li>
              ))}
            </ul>
          )}
        </Section>

        <Section
          title="Local keys"
          aside={
            <button className="btn btn-ghost btn-sm" onClick={() => go("keys")}>
              Manage →
            </button>
          }
        >
          {activeKeys.length === 0 ? (
            <EmptyState icon="🔑" title="No client keys yet">
              <button className="btn btn-primary" onClick={() => go("keys")}>
                Create a key
              </button>
            </EmptyState>
          ) : (
            <ul className="rowlist">
              {activeKeys.slice(0, 4).map((k) => (
                <li key={k.id}>
                  <span className="row-title">{k.name}</span>
                  <span className="mono dim">sk-starfish-{k.hint}…</span>
                </li>
              ))}
            </ul>
          )}
        </Section>
      </div>

      <Section
        title="Recent requests"
        aside={
          <button className="btn btn-ghost btn-sm" onClick={() => go("logs")}>
            Full log →
          </button>
        }
      >
        {logs.length === 0 ? (
          <EmptyState icon="〰️" title="Nothing yet">
            Point a client at the gateway and requests will appear here. Grab a config from{" "}
            <button className="linklike" onClick={() => go("connect")}>
              Connect
            </button>
            .
          </EmptyState>
        ) : (
          <table className="table">
            <tbody>
              {logs.slice(0, 6).map((l) => (
                <tr key={l.id}>
                  <td className="dim">{fmtTime(l.started_at)}</td>
                  <td>
                    <Pill tone={l.surface === "anthropic" ? "accent" : "dim"}>{l.surface}</Pill>
                  </td>
                  <td className="mono">{l.endpoint}</td>
                  <td className="mono dim">
                    {l.model} → {l.agent ?? "?"}
                  </td>
                  <td>
                    <Pill tone={l.status < 400 ? "ok" : "err"}>{l.status}</Pill>
                  </td>
                  <td className="dim">{fmtLatency(l.latency_ms)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>
    </>
  );
}
