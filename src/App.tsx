import { useCallback, useEffect, useState } from "react";
import "./App.css";
import { api, onLog, onServerStatus } from "./api";
import type { AppSnapshot, LogEntry } from "./types";
import { Dashboard } from "./pages/Dashboard";
import { Accounts } from "./pages/Accounts";
import { Keys } from "./pages/Keys";
import { Models } from "./pages/Models";
import { Logs } from "./pages/Logs";
import { Connect } from "./pages/Connect";
import { SettingsPage } from "./pages/Settings";
import { Onboarding } from "./pages/Onboarding";

const NAV = [
  { id: "dashboard", label: "Dashboard", icon: "◉" },
  { id: "accounts", label: "Accounts", icon: "⭐" },
  { id: "keys", label: "Keys", icon: "🗝" },
  { id: "models", label: "Models", icon: "🧭" },
  { id: "connect", label: "Connect", icon: "🔌" },
  { id: "logs", label: "Logs", icon: "〰" },
  { id: "settings", label: "Settings", icon: "⚙" },
] as const;

type Page = (typeof NAV)[number]["id"];

const MAX_LOGS = 500;

export default function App() {
  const [snap, setSnap] = useState<AppSnapshot | null>(null);
  const [page, setPage] = useState<Page>("dashboard");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api.snapshot().then(setSnap).catch((e) => setLoadError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
    api.logsRecent(200).then(setLogs).catch(() => {});
    const unlisteners: Array<() => void> = [];
    onLog((entry) => setLogs((l) => [entry, ...l].slice(0, MAX_LOGS))).then((u) =>
      unlisteners.push(u)
    );
    onServerStatus(() => refresh()).then((u) => unlisteners.push(u));
    return () => unlisteners.forEach((u) => u());
  }, [refresh]);

  useEffect(() => {
    document.documentElement.dataset.theme = snap?.settings.theme || "dark";
  }, [snap?.settings.theme]);

  if (loadError) {
    return (
      <div className="boot-error">
        <h1>Starfish couldn't start</h1>
        <pre>{loadError}</pre>
      </div>
    );
  }
  if (!snap) {
    return <div className="boot">⭐</div>;
  }

  // First run: walk through sign-in → agent → key → connect.
  if (!snap.onboarded) {
    return <Onboarding snap={snap} refresh={refresh} done={refresh} />;
  }

  return (
    <div className="app">
      <nav className="sidebar">
        <div className="brand">
          <span className="brand-mark">⭐</span>
          <span className="brand-name">Starfish</span>
        </div>
        <div className="nav">
          {NAV.map((n) => (
            <button
              key={n.id}
              className={`nav-item ${page === n.id ? "active" : ""}`}
              onClick={() => setPage(n.id)}
            >
              <span className="nav-icon" aria-hidden>
                {n.icon}
              </span>
              {n.label}
            </button>
          ))}
        </div>
        <div className="sidebar-foot">
          <span className={`beacon ${snap.server_status.running ? "on" : ""}`} />
          <span className="mono small">
            {snap.server_status.running ? `:${snap.server_status.port}` : "stopped"}
          </span>
        </div>
      </nav>
      <main className="content">
        {page === "dashboard" && (
          <Dashboard snap={snap} logs={logs} refresh={refresh} go={(p) => setPage(p as Page)} />
        )}
        {page === "accounts" && <Accounts snap={snap} refresh={refresh} />}
        {page === "keys" && <Keys snap={snap} refresh={refresh} />}
        {page === "models" && <Models snap={snap} refresh={refresh} />}
        {page === "connect" && <Connect snap={snap} />}
        {page === "logs" && <Logs logs={logs} setLogs={setLogs} />}
        {page === "settings" && <SettingsPage snap={snap} refresh={refresh} />}
      </main>
    </div>
  );
}
