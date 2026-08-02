import { useEffect, useState } from "react";
import { api } from "../api";
import type { AppSnapshot, ServerConfig } from "../types";
import { Field, Section, Toggle } from "../ui";

export function SettingsPage({ snap, refresh }: { snap: AppSnapshot; refresh: () => void }) {
  const [server, setServer] = useState<ServerConfig>(snap.server);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);

  useEffect(() => setServer(snap.server), [snap.server]);
  useEffect(() => {
    api.getLaunchAtLogin().then(setLaunchAtLogin).catch(() => {});
  }, []);

  const edit = (patch: Partial<ServerConfig>) => {
    setServer((s) => ({ ...s, ...patch }));
    setDirty(true);
    setSaved(false);
  };

  const save = async () => {
    setError(null);
    try {
      await api.setServerConfig(server);
      setDirty(false);
      setSaved(true);
      refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const setSetting = async (patch: Partial<AppSnapshot["settings"]>) => {
    await api.setSettings({ ...snap.settings, ...patch });
    refresh();
  };

  return (
    <>
      <Section
        title="Gateway"
        aside={
          <button className="btn btn-primary btn-sm" onClick={save} disabled={!dirty}>
            {saved ? "Saved ✓" : "Save"}
          </button>
        }
      >
        <div className="grid-2">
          <Field label="Host" hint="Keep 127.0.0.1 unless you really know what you're doing.">
            <input value={server.host} onChange={(e) => edit({ host: e.target.value })} className="mono" />
          </Field>
          <Field label="Port">
            <input
              type="number"
              value={server.port}
              onChange={(e) => edit({ port: Number(e.target.value) })}
              className="mono"
            />
          </Field>
          <Field label="Poll interval (ms)" hint="How often Starfish polls a running agent thread.">
            <input
              type="number"
              value={server.poll_interval_ms}
              onChange={(e) => edit({ poll_interval_ms: Number(e.target.value) })}
              className="mono"
            />
          </Field>
          <Field label="Run timeout (s)" hint="Agent runs longer than this fail with 504.">
            <input
              type="number"
              value={server.run_timeout_secs}
              onChange={(e) => edit({ run_timeout_secs: Number(e.target.value) })}
              className="mono"
            />
          </Field>
        </div>
        <Toggle
          checked={server.allow_anonymous}
          onChange={(v) => edit({ allow_anonymous: v })}
          label="Dev mode: accept requests without a key"
          hint="Off by default and should stay off — anyone on this machine could use your Hyperagent account."
        />
        {server.host !== "127.0.0.1" && server.host !== "localhost" && server.host !== "::1" && (
          <Toggle
            checked={server.i_understand_lan_exposure_risks}
            onChange={(v) => edit({ i_understand_lan_exposure_risks: v })}
            label="I understand the risks of binding beyond localhost"
            hint="Non-loopback binds expose your agents (and account spend) to the network. There is no TLS yet."
          />
        )}
        {error && <div className="banner banner-err">{error}</div>}
        {dirty && snap.server_status.running && (
          <div className="banner banner-warn">Changes apply the next time the gateway starts.</div>
        )}
      </Section>

      <Section title="App">
        <Toggle
          checked={snap.settings.autostart_server}
          onChange={(v) => setSetting({ autostart_server: v })}
          label="Start the gateway when Starfish launches"
        />
        <Toggle
          checked={launchAtLogin}
          onChange={async (v) => {
            try {
              setLaunchAtLogin(await api.setLaunchAtLogin(v));
            } catch (e) {
              setError(String(e));
            }
          }}
          label="Launch Starfish at login"
          hint="Pairs well with the setting above for an always-on gateway."
        />
        <Field label="Theme">
          <select value={snap.settings.theme || "dark"} onChange={(e) => setSetting({ theme: e.target.value })}>
            <option value="dark">Dark</option>
            <option value="light">Light</option>
          </select>
        </Field>
      </Section>

      <Section title="Security">
        <ul className="facts">
          <li>
            <span className="dim">Secrets vault</span>
            <span>
              {snap.vault_backend === "os-keychain"
                ? "OS keychain (Keychain / Credential Manager / Secret Service)"
                : "0600 file fallback — no OS keychain was found on this system"}
            </span>
          </li>
          <li>
            <span className="dim">Config file</span>
            <span>never contains tokens or key secrets — safe to export/share</span>
          </li>
          <li>
            <span className="dim">Token counts</span>
            <span>estimates only; the upstream exposes no exact numbers</span>
          </li>
          <li>
            <span className="dim">Version</span>
            <span className="mono">{snap.version}</span>
          </li>
        </ul>
        <p className="dim small">
          Starfish routes traffic through your own authenticated Hyperagent account(s). Don't use
          it to evade usage limits, share an identity against the ToS, or resell access.
        </p>
      </Section>
    </>
  );
}
