import { useEffect, useState } from "react";
import { api, onOAuthProgress } from "../api";
import type { AgentInfo, AppSnapshot, CreatedKey, OAuthProgress } from "../types";
import { CodeBlock } from "../ui";
import { claudeCodeSnippet, codexSnippet } from "../snippets";

/// First-run wizard: sign in → pick agent → key → connect (MISSION.md §6G).
export function Onboarding({ snap, refresh, done }: { snap: AppSnapshot; refresh: () => void; done: () => void }) {
  const [step, setStep] = useState(snap.accounts.length > 0 ? 1 : 0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<OAuthProgress | null>(null);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [agentId, setAgentId] = useState("");
  const [created, setCreated] = useState<CreatedKey | null>(null);

  const account = snap.accounts[0];

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onOAuthProgress(setProgress).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    if (step === 1 && account) {
      api
        .listAgents(account.id)
        .then((a) => {
          setAgents(a);
          if (a.length > 0) setAgentId(account.default_agent_id ?? a[0].id);
        })
        .catch((e) => setError(String(e)));
    }
  }, [step, account?.id]);

  const signIn = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.beginSignIn("Personal");
      refresh();
      setStep(1);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const pickAgent = async () => {
    if (!account) return;
    setBusy(true);
    setError(null);
    try {
      await api.setAccountDefaultAgent(account.id, agentId || null);
      const key = await api.createKey("My first key", account.id, null);
      setCreated(key);
      await api.serverStart().catch(() => {});
      refresh();
      setStep(2);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const finish = async () => {
    await api.setOnboarded(true);
    done();
  };

  const baseUrl = snap.server_status.base_url;

  return (
    <div className="onboarding">
      <div className="onboarding-card">
        <div className="onboarding-logo">⭐</div>
        <h1>Starfish</h1>
        <p className="tagline">
          Your Hyperagent agents, behind the two APIs every AI tool already speaks.
        </p>

        <div className="steps">
          {["Sign in", "Pick an agent", "Connect"].map((label, i) => (
            <div key={label} className={`step ${i === step ? "current" : i < step ? "done" : ""}`}>
              <span className="step-n">{i < step ? "✓" : i + 1}</span> {label}
            </div>
          ))}
        </div>

        {step === 0 && (
          <>
            <p>
              Sign in to Hyperagent in your browser. Starfish stores only OAuth tokens — in your
              OS keychain, never on disk.
            </p>
            {busy && progress && <p className="dim small">{progress.detail}</p>}
            <button className="btn btn-primary btn-lg" onClick={signIn} disabled={busy || snap.mock}>
              {busy ? "Waiting for browser…" : "Sign in to Hyperagent"}
            </button>
            {snap.mock && (
              <p className="dim small">
                Mock upstream is active — sign-in is disabled. Restart without
                STARFISH_MOCK_UPSTREAM to use a real account.
              </p>
            )}
          </>
        )}

        {step === 1 && (
          <>
            <p>
              Pick the agent that answers by default (clients can also address any agent by name,
              or use <code>hyperagent-default</code>).
            </p>
            {agents.length === 0 ? (
              <div className="banner banner-warn">
                No named agents on this account. Create one in Hyperagent, then{" "}
                <button className="linklike" onClick={() => api.listAgents(account.id, true).then(setAgents)}>
                  refresh
                </button>
                .
              </div>
            ) : (
              <select value={agentId} onChange={(e) => setAgentId(e.target.value)} className="onboarding-select">
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name} — {a.description ?? a.id}
                  </option>
                ))}
              </select>
            )}
            <button className="btn btn-primary btn-lg" onClick={pickAgent} disabled={busy || !agentId}>
              {busy ? "Setting up…" : "Create my key & start the gateway"}
            </button>
          </>
        )}

        {step === 2 && created && (
          <>
            <p>
              The gateway is live on <code className="mono">{baseUrl}</code>. Connect a client —
              here's Claude Code and Codex with your real key:
            </p>
            <CodeBlock caption="Claude Code" code={claudeCodeSnippet({ baseUrl, key: created.secret })} />
            <CodeBlock caption="Codex (~/.codex/config.toml)" code={codexSnippet({ baseUrl, key: created.secret })} />
            <p className="dim small">
              Reveal this key anytime from Keys. Requests run full agent turns — expect seconds,
              and remember token counts are estimates. You're responsible for using your own
              account within Hyperagent's Terms of Service.
            </p>
            <button className="btn btn-primary btn-lg" onClick={finish}>
              Take me to the dashboard
            </button>
          </>
        )}

        {error && <div className="banner banner-err">{error}</div>}
        {step < 2 && (
          <button className="linklike dim skip" onClick={finish}>
            Skip setup
          </button>
        )}
      </div>
    </div>
  );
}
