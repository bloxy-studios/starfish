// Typed wrappers over the Tauri IPC surface.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Account,
  AgentInfo,
  AppSnapshot,
  CreatedKey,
  DoctorReport,
  LogEntry,
  MappingRule,
  OAuthProgress,
  ServerConfig,
  ServerStatus,
  Settings,
} from "./types";

export const api = {
  snapshot: () => invoke<AppSnapshot>("app_snapshot"),

  // server
  serverStart: () => invoke<ServerStatus>("server_start"),
  serverStop: () => invoke<ServerStatus>("server_stop"),
  serverStatus: () => invoke<ServerStatus>("server_status"),
  setServerConfig: (server: ServerConfig) =>
    invoke<ServerStatus>("set_server_config", { server }),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  setOnboarded: (done: boolean) => invoke<void>("set_onboarded", { done }),

  // accounts
  beginSignIn: (nickname?: string, baseUrl?: string) =>
    invoke<Account>("begin_sign_in", { nickname, baseUrl }),
  reauthAccount: (accountId: string) =>
    invoke<Account>("reauth_account", { accountId }),
  removeAccount: (accountId: string) =>
    invoke<void>("remove_account", { accountId }),
  setAccountNickname: (accountId: string, nickname: string) =>
    invoke<void>("set_account_nickname", { accountId, nickname }),
  setAccountDefaultAgent: (accountId: string, agentId: string | null) =>
    invoke<void>("set_account_default_agent", { accountId, agentId }),
  doctor: (accountId: string) => invoke<DoctorReport>("doctor", { accountId }),
  listAgents: (accountId: string, force = false) =>
    invoke<AgentInfo[]>("list_agents", { accountId, force }),

  // keys
  createKey: (name: string, accountId: string, defaultAgentId?: string | null) =>
    invoke<CreatedKey>("create_key", { name, accountId, defaultAgentId }),
  revealKey: (keyId: string) => invoke<string>("reveal_key", { keyId }),
  revokeKey: (keyId: string) => invoke<void>("revoke_key", { keyId }),
  rotateKey: (keyId: string) => invoke<CreatedKey>("rotate_key", { keyId }),
  renameKey: (keyId: string, name: string) =>
    invoke<void>("rename_key", { keyId, name }),
  setKeyAgent: (keyId: string, agentId: string | null) =>
    invoke<void>("set_key_agent", { keyId, agentId }),

  // mappings
  setMappings: (mappings: MappingRule[]) =>
    invoke<void>("set_mappings", { mappings }),

  // logs / misc
  logsRecent: (limit = 200) => invoke<LogEntry[]>("logs_recent", { limit }),
  clearLogs: () => invoke<void>("clear_logs"),
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  setLaunchAtLogin: (enabled: boolean) =>
    invoke<boolean>("set_launch_at_login", { enabled }),
  getLaunchAtLogin: () => invoke<boolean>("get_launch_at_login"),
};

export function onLog(handler: (entry: LogEntry) => void): Promise<UnlistenFn> {
  return listen<LogEntry>("gateway://log", (e) => handler(e.payload));
}

export function onServerStatus(
  handler: (status: ServerStatus) => void
): Promise<UnlistenFn> {
  return listen<ServerStatus>("server://status", (e) => handler(e.payload));
}

export function onOAuthProgress(
  handler: (p: OAuthProgress) => void
): Promise<UnlistenFn> {
  return listen<OAuthProgress>("oauth://status", (e) => handler(e.payload));
}
