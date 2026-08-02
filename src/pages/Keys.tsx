import { useEffect, useState } from "react";
import { api } from "../api";
import type { AgentInfo, AppSnapshot, CreatedKey, KeyRecord } from "../types";
import { CodeBlock, EmptyState, Field, Modal, Pill, Section } from "../ui";

function CreateKeyModal({
  snap,
  onClose,
  onCreated,
}: {
  snap: AppSnapshot;
  onClose: () => void;
  onCreated: (k: CreatedKey) => void;
}) {
  const [name, setName] = useState("");
  const [accountId, setAccountId] = useState(snap.accounts[0]?.id ?? "");
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [agentId, setAgentId] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!accountId) return;
    api.listAgents(accountId).then(setAgents).catch(() => setAgents([]));
  }, [accountId]);

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      onCreated(await api.createKey(name || "unnamed key", accountId, agentId || null));
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <Modal title="Create a local key" onClose={onClose}>
      <Field label="Name" hint="What will use this key? (e.g. “Claude Code — laptop”)">
        <input value={name} onChange={(e) => setName(e.target.value)} autoFocus placeholder="Claude Code" />
      </Field>
      <Field label="Account" hint="Requests with this key run as this Hyperagent account.">
        <select value={accountId} onChange={(e) => setAccountId(e.target.value)}>
          {snap.accounts.map((a) => (
            <option key={a.id} value={a.id}>
              {a.nickname}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Default agent (optional)" hint="Overrides the account default for this key only.">
        <select value={agentId} onChange={(e) => setAgentId(e.target.value)}>
          <option value="">— use account default —</option>
          {agents.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name} ({a.id})
            </option>
          ))}
        </select>
      </Field>
      {error && <div className="banner banner-err">{error}</div>}
      <div className="modal-actions">
        <button className="btn btn-ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="btn btn-primary" onClick={create} disabled={busy || !accountId}>
          Create key
        </button>
      </div>
    </Modal>
  );
}

function KeyRow({
  k,
  snap,
  refresh,
  showSecret,
}: {
  k: KeyRecord;
  snap: AppSnapshot;
  refresh: () => void;
  showSecret: (title: string, secret: string) => void;
}) {
  const account = snap.accounts.find((a) => a.id === k.account_id);

  const reveal = async () => {
    try {
      showSecret(`Key “${k.name}”`, await api.revealKey(k.id));
    } catch (e) {
      alert(String(e));
    }
  };

  const rotate = async () => {
    if (!confirm(`Rotate “${k.name}”? The old secret stops working immediately.`)) return;
    const created = await api.rotateKey(k.id);
    showSecret(`New secret for “${k.name}”`, created.secret);
    refresh();
  };

  const revoke = async () => {
    if (!confirm(`Revoke “${k.name}”? Clients using it get 401 immediately.`)) return;
    await api.revokeKey(k.id);
    refresh();
  };

  return (
    <tr className={k.revoked ? "row-dead" : ""}>
      <td>
        <div className="row-title">{k.name}</div>
        <div className="mono dim small">sk-starfish-{k.hint}…</div>
      </td>
      <td>{account?.nickname ?? <span className="dim">missing account</span>}</td>
      <td className="mono dim small">{k.default_agent_id ?? "account default"}</td>
      <td>
        {k.revoked ? <Pill tone="err">revoked</Pill> : <Pill tone="ok">active</Pill>}
      </td>
      <td className="row-actions">
        {!k.revoked && (
          <>
            <button className="btn btn-ghost btn-sm" onClick={reveal}>
              Reveal
            </button>
            <button className="btn btn-ghost btn-sm" onClick={rotate}>
              Rotate
            </button>
            <button className="btn btn-ghost btn-sm danger" onClick={revoke}>
              Revoke
            </button>
          </>
        )}
      </td>
    </tr>
  );
}

export function Keys({ snap, refresh }: { snap: AppSnapshot; refresh: () => void }) {
  const [creating, setCreating] = useState(false);
  const [secretModal, setSecretModal] = useState<{ title: string; secret: string } | null>(null);

  return (
    <Section
      title="Local API keys"
      aside={
        <button
          className="btn btn-primary"
          onClick={() => setCreating(true)}
          disabled={snap.accounts.length === 0}
        >
          + Create key
        </button>
      }
    >
      <p className="dim">
        Clients authenticate to the gateway with these keys (as{" "}
        <code>Authorization: Bearer</code> or <code>x-api-key</code>). Only a hash is kept in
        config — the secret itself lives in the vault.
      </p>
      {snap.accounts.length === 0 ? (
        <EmptyState icon="🔑" title="Sign in first">
          Keys route to an account — add a Hyperagent account before creating keys.
        </EmptyState>
      ) : snap.keys.length === 0 ? (
        <EmptyState icon="🔑" title="No keys yet">
          Create one key per client (Claude Code, Codex, Cursor…) so you can revoke them
          independently.
        </EmptyState>
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th>Key</th>
              <th>Account</th>
              <th>Agent override</th>
              <th>Status</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {snap.keys.map((k) => (
              <KeyRow key={k.id} k={k} snap={snap} refresh={refresh} showSecret={(title, secret) => setSecretModal({ title, secret })} />
            ))}
          </tbody>
        </table>
      )}

      {creating && (
        <CreateKeyModal
          snap={snap}
          onClose={() => setCreating(false)}
          onCreated={(created) => {
            setSecretModal({ title: `Key “${created.key.name}” created`, secret: created.secret });
            refresh();
          }}
        />
      )}
      {secretModal && (
        <Modal title={secretModal.title} onClose={() => setSecretModal(null)}>
          <p className="dim">
            Copy this into your client. You can reveal it again later from this screen.
          </p>
          <CodeBlock code={secretModal.secret} />
          <div className="modal-actions">
            <button className="btn btn-primary" onClick={() => setSecretModal(null)}>
              Done
            </button>
          </div>
        </Modal>
      )}
    </Section>
  );
}
