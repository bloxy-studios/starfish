import { useEffect, useState } from "react";
import { api } from "../api";
import type { AgentInfo, AppSnapshot, MappingRule, Surface } from "../types";
import { EmptyState, Pill, Section } from "../ui";

const CLAUDE_CODE_PRESET = (agentId: string, fastAgentId: string): MappingRule[] => [
  { pattern: "claude-*opus*", surface: "anthropic", agent_id: agentId },
  { pattern: "claude-*sonnet*", surface: "anthropic", agent_id: agentId },
  { pattern: "claude-*haiku*", surface: "anthropic", agent_id: fastAgentId },
  { pattern: "claude-*", surface: "anthropic", agent_id: agentId },
];

export function Models({ snap, refresh }: { snap: AppSnapshot; refresh: () => void }) {
  const [accountId, setAccountId] = useState(snap.accounts[0]?.id ?? "");
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [rules, setRules] = useState<MappingRule[]>(snap.mappings);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => setRules(snap.mappings), [snap.mappings]);

  useEffect(() => {
    if (!accountId) return;
    setLoading(true);
    api
      .listAgents(accountId)
      .then(setAgents)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [accountId]);

  const mutate = (f: (rules: MappingRule[]) => MappingRule[]) => {
    setRules((r) => f([...r]));
    setDirty(true);
  };

  const save = async () => {
    const cleaned = rules.filter((r) => r.pattern.trim() && r.agent_id.trim());
    await api.setMappings(cleaned);
    setDirty(false);
    refresh();
  };

  const addClaudePreset = () => {
    if (agents.length === 0) return;
    const main = agents[0].id;
    const fast = agents[Math.min(1, agents.length - 1)].id;
    mutate((r) => {
      const existing = new Set(r.map((x) => `${x.surface ?? "both"}|${x.pattern}`));
      const additions = CLAUDE_CODE_PRESET(main, fast).filter(
        (p) => !existing.has(`${p.surface ?? "both"}|${p.pattern}`)
      );
      return [...r, ...additions];
    });
  };

  if (snap.accounts.length === 0) {
    return (
      <Section title="Models & mapping">
        <EmptyState icon="🗺️" title="Sign in first">
          Model mapping routes client model names to your Hyperagent agents — add an account to
          see agents.
        </EmptyState>
      </Section>
    );
  }

  return (
    <>
      <Section
        title="Your agents"
        aside={
          <div className="inline-controls">
            <select value={accountId} onChange={(e) => setAccountId(e.target.value)}>
              {snap.accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.nickname}
                </option>
              ))}
            </select>
            <button
              className="btn btn-ghost btn-sm"
              onClick={() => api.listAgents(accountId, true).then(setAgents)}
            >
              ↻ Refresh
            </button>
          </div>
        }
      >
        <p className="dim">
          Every named agent is a “model”: clients can use its id or name directly, or the{" "}
          <code>hyperagent-default</code> alias. These are what <code>GET /v1/models</code>{" "}
          returns.
        </p>
        {loading ? (
          <div className="dim">Loading agents…</div>
        ) : agents.length === 0 ? (
          <EmptyState icon="🤖" title="No named agents on this account">
            The MCP server can only start threads on <b>named agents</b> — create one in
            Hyperagent first, then refresh.
          </EmptyState>
        ) : (
          <ul className="rowlist">
            {agents.map((a) => (
              <li key={a.id}>
                <div>
                  <span className="row-title">{a.name}</span>
                  <span className="mono dim small"> {a.id}</span>
                  {a.description && <div className="dim small">{a.description}</div>}
                </div>
                {snap.accounts.find((x) => x.id === accountId)?.default_agent_id === a.id && (
                  <Pill tone="accent">default</Pill>
                )}
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section
        title="Mapping table"
        aside={
          <div className="inline-controls">
            <button className="btn btn-ghost btn-sm" onClick={addClaudePreset} disabled={agents.length === 0}>
              + Claude Code preset
            </button>
            <button
              className="btn btn-ghost btn-sm"
              onClick={() => mutate((r) => [...r, { pattern: "", surface: null, agent_id: agents[0]?.id ?? "" }])}
            >
              + Add rule
            </button>
            <button className="btn btn-primary btn-sm" onClick={save} disabled={!dirty}>
              Save
            </button>
          </div>
        }
      >
        <p className="dim">
          Wildcard patterns route model names clients hard-code (Claude Code sends{" "}
          <code>claude-*</code> names) to the agent you pick. Resolution order: exact agent id →
          agent name → these rules → default agent.
        </p>
        {rules.length === 0 ? (
          <EmptyState icon="🧭" title="No rules — defaults handle everything">
            Add the Claude Code preset if you plan to use Claude Code.
          </EmptyState>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Pattern</th>
                <th>Surface</th>
                <th>Agent</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {rules.map((r, i) => (
                <tr key={i}>
                  <td>
                    <input
                      className="mono"
                      value={r.pattern}
                      placeholder="claude-*sonnet*"
                      onChange={(e) => mutate((rs) => ((rs[i] = { ...rs[i], pattern: e.target.value }), rs))}
                    />
                  </td>
                  <td>
                    <select
                      value={r.surface ?? "both"}
                      onChange={(e) =>
                        mutate((rs) => {
                          const v = e.target.value;
                          rs[i] = { ...rs[i], surface: v === "both" ? null : (v as Surface) };
                          return rs;
                        })
                      }
                    >
                      <option value="both">both</option>
                      <option value="openai">openai</option>
                      <option value="anthropic">anthropic</option>
                    </select>
                  </td>
                  <td>
                    <select
                      value={r.agent_id}
                      onChange={(e) => mutate((rs) => ((rs[i] = { ...rs[i], agent_id: e.target.value }), rs))}
                    >
                      {r.agent_id && !agents.some((a) => a.id === r.agent_id) && (
                        <option value={r.agent_id}>{r.agent_id} (other account)</option>
                      )}
                      {agents.map((a) => (
                        <option key={a.id} value={a.id}>
                          {a.name}
                        </option>
                      ))}
                    </select>
                  </td>
                  <td className="row-actions">
                    <button
                      className="btn btn-ghost btn-sm danger"
                      onClick={() => mutate((rs) => rs.filter((_, j) => j !== i))}
                    >
                      ✕
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {error && <div className="banner banner-err">{error}</div>}
      </Section>
    </>
  );
}
