import { useEffect, useState } from "react";
import { api, onOAuthProgress } from "../api";
import type { Account, AgentInfo, AppSnapshot, DoctorReport, OAuthProgress } from "../types";
import { EmptyState, Field, Modal, Pill, Section, tokenTone } from "../ui";

function SignInModal({
  onClose,
  onDone,
  reauthId,
  title,
}: {
  onClose: () => void;
  onDone: () => void;
  reauthId?: string;
  title: string;
}) {
  const [nickname, setNickname] = useState("");
  const [progress, setProgress] = useState<OAuthProgress | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onOAuthProgress(setProgress).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  const start = async () => {
    setRunning(true);
    setError(null);
    try {
      if (reauthId) await api.reauthAccount(reauthId);
      else await api.beginSignIn(nickname || undefined);
      onDone();
      onClose();
    } catch (e) {
      setError(String(e));
      setRunning(false);
    }
  };

  const stages: OAuthProgress["stage"][] = [
    "discovering",
    "registering",
    "browser",
    "waiting",
    "exchanging",
    "done",
  ];
  const stageIndex = progress ? stages.indexOf(progress.stage) : -1;

  return (
    <Modal title={title} onClose={onClose}>
      {!running && !reauthId && (
        <Field label="Nickname" hint="How this account shows up in Starfish (e.g. “Work”, “Personal”).">
          <input
            value={nickname}
            onChange={(e) => setNickname(e.target.value)}
            placeholder="Personal"
            autoFocus
          />
        </Field>
      )}
      {!running ? (
        <>
          <p className="dim">
            Your browser will open to Hyperagent's sign-in page. Starfish never sees your
            password — it receives OAuth tokens and stores them in the OS keychain.
          </p>
          <div className="modal-actions">
            <button className="btn btn-ghost" onClick={onClose}>
              Cancel
            </button>
            <button className="btn btn-primary" onClick={start}>
              Open browser &amp; sign in
            </button>
          </div>
        </>
      ) : (
        <div className="oauth-progress">
          {stages.slice(0, 5).map((s, i) => (
            <div key={s} className={`oauth-step ${i <= stageIndex ? "active" : ""}`}>
              <span className="oauth-dot" />
              <span>
                {s === "discovering" && "Discovering the authorization server"}
                {s === "registering" && "Registering Starfish (DCR)"}
                {s === "browser" && "Opening your browser"}
                {s === "waiting" && "Waiting for approval — check your browser"}
                {s === "exchanging" && "Exchanging code for tokens"}
              </span>
            </div>
          ))}
          {progress && <p className="dim small">{progress.detail}</p>}
        </div>
      )}
      {error && <div className="banner banner-err">{error}</div>}
    </Modal>
  );
}

function AccountCard({
  account,
  refresh,
}: {
  account: Account;
  refresh: () => void;
}) {
  const [agents, setAgents] = useState<AgentInfo[] | null>(null);
  const [report, setReport] = useState<DoctorReport | null>(null);
  const [checking, setChecking] = useState(false);
  const [reauth, setReauth] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.listAgents(account.id).then(setAgents).catch(() => setAgents(null));
  }, [account.id, account.token_state]);

  const runDoctor = async () => {
    setChecking(true);
    setError(null);
    try {
      setReport(await api.doctor(account.id));
    } catch (e) {
      setError(String(e));
    } finally {
      setChecking(false);
    }
  };

  const remove = async () => {
    if (
      !confirm(
        `Remove “${account.nickname}”? Its tokens are deleted from the keychain and keys routed to it are revoked.`
      )
    )
      return;
    await api.removeAccount(account.id);
    refresh();
  };

  return (
    <div className="card">
      <div className="card-head">
        <div>
          <h3>{account.nickname}</h3>
          <div className="dim small mono">{account.base_url}</div>
        </div>
        <Pill tone={tokenTone(account.token_state)}>{account.token_state}</Pill>
      </div>

      <Field label="Default agent" hint="Requests with no better match (incl. hyperagent-default) run on this agent.">
        <select
          value={account.default_agent_id ?? ""}
          onChange={async (e) => {
            await api.setAccountDefaultAgent(account.id, e.target.value || null);
            refresh();
          }}
        >
          <option value="">— none —</option>
          {(agents ?? []).map((a) => (
            <option key={a.id} value={a.id}>
              {a.name} ({a.id})
            </option>
          ))}
        </select>
      </Field>

      <div className="card-actions">
        <button className="btn btn-ghost btn-sm" onClick={runDoctor} disabled={checking}>
          {checking ? "Checking…" : "Run doctor"}
        </button>
        <button className="btn btn-ghost btn-sm" onClick={() => setReauth(true)}>
          Re-authenticate
        </button>
        <button className="btn btn-ghost btn-sm danger" onClick={remove}>
          Remove
        </button>
      </div>

      {report && (
        <div className={`doctor ${report.mcp_reachable ? "ok" : "bad"}`}>
          <div>
            MCP: {report.mcp_reachable ? "reachable ✓" : "unreachable ✗"} · agents:{" "}
            {report.agents_count} · token: {report.token_state}
          </div>
          {report.detail && <div className="small">{report.detail}</div>}
        </div>
      )}
      {error && <div className="banner banner-err">{error}</div>}

      {reauth && (
        <SignInModal
          title={`Re-authenticate “${account.nickname}”`}
          reauthId={account.id}
          onClose={() => setReauth(false)}
          onDone={refresh}
        />
      )}
    </div>
  );
}

export function Accounts({ snap, refresh }: { snap: AppSnapshot; refresh: () => void }) {
  const [adding, setAdding] = useState(false);
  return (
    <Section
      title="Hyperagent accounts"
      aside={
        <button className="btn btn-primary" onClick={() => setAdding(true)} disabled={snap.mock}>
          + Add account
        </button>
      }
    >
      <p className="dim">
        Each account signs in with OAuth 2.1 (PKCE) and keeps its tokens in the{" "}
        {snap.vault_backend === "os-keychain" ? "OS keychain" : "file vault (no keychain found)"}.
        Local keys route requests to a specific account.
      </p>
      {snap.accounts.length === 0 ? (
        <EmptyState icon="⭐" title="Sign in to get started">
          Starfish needs at least one Hyperagent account (with at least one named agent) to serve
          requests.
        </EmptyState>
      ) : (
        <div className="cards">
          {snap.accounts.map((a) => (
            <AccountCard key={a.id} account={a} refresh={refresh} />
          ))}
        </div>
      )}
      {adding && (
        <SignInModal
          title="Add a Hyperagent account"
          onClose={() => setAdding(false)}
          onDone={refresh}
        />
      )}
    </Section>
  );
}
