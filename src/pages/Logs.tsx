import { useState } from "react";
import { api } from "../api";
import type { LogEntry } from "../types";
import { EmptyState, Modal, Pill, Section, fmtLatency, fmtTime } from "../ui";

function Detail({ entry, onClose }: { entry: LogEntry; onClose: () => void }) {
  return (
    <Modal title={`${entry.method} ${entry.endpoint}`} onClose={onClose} wide>
      <div className="detail-grid">
        <div>
          <span className="dim">When</span> {new Date(entry.started_at).toLocaleString()}
        </div>
        <div>
          <span className="dim">Surface</span> {entry.surface}
          {entry.stream ? " (stream)" : ""}
        </div>
        <div>
          <span className="dim">Status</span>{" "}
          <Pill tone={entry.status < 400 ? "ok" : "err"}>{entry.status}</Pill>
        </div>
        <div>
          <span className="dim">Latency</span> {fmtLatency(entry.latency_ms)}
        </div>
        <div>
          <span className="dim">Model → agent</span>{" "}
          <span className="mono">
            {entry.model ?? "—"} → {entry.agent ?? "—"}
          </span>
        </div>
        <div>
          <span className="dim">Tokens (est.)</span> {entry.input_tokens_est ?? "—"} in /{" "}
          {entry.output_tokens_est ?? "—"} out
        </div>
        <div>
          <span className="dim">Thread</span> <span className="mono">{entry.thread_id ?? "—"}</span>
        </div>
        <div>
          <span className="dim">Key</span>{" "}
          <span className="mono">{entry.key_hint ? `sk-starfish-${entry.key_hint}…` : "—"}</span>
        </div>
      </div>

      {entry.error && <div className="banner banner-err">{entry.error}</div>}

      {entry.polls.length > 0 && (
        <>
          <h4>Poll timeline</h4>
          <div className="timeline">
            {entry.polls.map((p, i) => (
              <div key={i} className="timeline-row mono small">
                <span className="dim">{(p.at_ms / 1000).toFixed(1)}s</span>
                <span>{p.note}</span>
              </div>
            ))}
          </div>
        </>
      )}

      {entry.request_snapshot && (
        <>
          <h4>Request (sanitized)</h4>
          <pre className="snapshot">{entry.request_snapshot}</pre>
        </>
      )}
      {entry.response_snapshot && (
        <>
          <h4>Response text</h4>
          <pre className="snapshot">{entry.response_snapshot}</pre>
        </>
      )}
    </Modal>
  );
}

export function Logs({ logs, setLogs }: { logs: LogEntry[]; setLogs: (l: LogEntry[]) => void }) {
  const [selected, setSelected] = useState<LogEntry | null>(null);
  const [filter, setFilter] = useState<"all" | "openai" | "anthropic" | "errors">("all");

  const visible = logs.filter((l) => {
    if (filter === "all") return true;
    if (filter === "errors") return l.status >= 400 || !!l.error;
    return l.surface === filter;
  });

  return (
    <Section
      title="Request log"
      aside={
        <div className="inline-controls">
          <select value={filter} onChange={(e) => setFilter(e.target.value as typeof filter)}>
            <option value="all">all</option>
            <option value="openai">openai</option>
            <option value="anthropic">anthropic</option>
            <option value="errors">errors</option>
          </select>
          <button
            className="btn btn-ghost btn-sm"
            onClick={async () => {
              await api.clearLogs();
              setLogs([]);
            }}
          >
            Clear
          </button>
        </div>
      }
    >
      <p className="dim">
        Live view of every gateway request. Keys are redacted; bodies are truncated snapshots.
        Token counts are <b>estimates</b> — the upstream doesn't expose exact numbers.
      </p>
      {visible.length === 0 ? (
        <EmptyState icon="〰️" title="No requests captured">
          Requests appear here in real time once a client talks to the gateway.
        </EmptyState>
      ) : (
        <table className="table table-click">
          <thead>
            <tr>
              <th>Time</th>
              <th>Surface</th>
              <th>Endpoint</th>
              <th>Model → agent</th>
              <th>Status</th>
              <th>Latency</th>
              <th>Tokens (est.)</th>
            </tr>
          </thead>
          <tbody>
            {visible.map((l) => (
              <tr key={l.id} onClick={() => setSelected(l)}>
                <td className="dim">{fmtTime(l.started_at)}</td>
                <td>
                  <Pill tone={l.surface === "anthropic" ? "accent" : "dim"}>{l.surface}</Pill>
                  {l.stream && <span className="dim small"> ⚡</span>}
                </td>
                <td className="mono small">{l.endpoint}</td>
                <td className="mono small dim">
                  {l.model ?? "—"} → {l.agent ?? "—"}
                </td>
                <td>
                  <Pill tone={l.status < 400 ? "ok" : "err"}>{l.status}</Pill>
                </td>
                <td className="dim">{fmtLatency(l.latency_ms)}</td>
                <td className="dim small">
                  {l.input_tokens_est ?? "—"} / {l.output_tokens_est ?? "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {selected && <Detail entry={selected} onClose={() => setSelected(null)} />}
    </Section>
  );
}
