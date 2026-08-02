// DTOs mirrored from starfish-core / src-tauri (serde snake_case).

export interface ServerConfig {
  host: string;
  port: number;
  poll_interval_ms: number;
  run_timeout_secs: number;
  allow_anonymous: boolean;
  i_understand_lan_exposure_risks: boolean;
}

export interface Settings {
  theme: string; // "auto" | "dark" | "light"
  log_level: string;
  autostart_server: boolean;
}

export interface Account {
  id: string;
  nickname: string;
  base_url: string;
  identity?: string | null;
  default_agent_id?: string | null;
  created_at: string;
  token_state: string;
}

export interface KeyRecord {
  id: string;
  name: string;
  hash: string;
  hint: string;
  account_id: string;
  default_agent_id?: string | null;
  disabled_tools?: string[];
  created_at: string;
  last_used_at?: string | null;
  revoked: boolean;
}

export interface CreatedKey {
  key: KeyRecord;
  secret: string;
}

export type Surface = "openai" | "anthropic";

export interface MappingRule {
  pattern: string;
  surface?: Surface | null;
  agent_id: string;
}

export interface AgentInfo {
  id: string;
  name: string;
  description?: string | null;
}

export interface ServerStatus {
  running: boolean;
  host: string;
  port: number;
  base_url: string;
  started_at?: string | null;
  allow_anonymous: boolean;
}

export interface PollEvent {
  at_ms: number;
  note: string;
}

export interface LogEntry {
  id: string;
  started_at: string;
  surface: string;
  method: string;
  endpoint: string;
  model?: string | null;
  agent?: string | null;
  account_id?: string | null;
  key_hint?: string | null;
  stream: boolean;
  status: number;
  latency_ms: number;
  input_tokens_est?: number | null;
  output_tokens_est?: number | null;
  thread_id?: string | null;
  error?: string | null;
  request_snapshot?: string | null;
  response_snapshot?: string | null;
  polls: PollEvent[];
}

export interface DoctorReport {
  mcp_reachable: boolean;
  agents_count: number;
  token_state: string;
  detail?: string | null;
}

export interface AppSnapshot {
  server: ServerConfig;
  settings: Settings;
  accounts: Account[];
  keys: KeyRecord[];
  mappings: MappingRule[];
  onboarded: boolean;
  vault_backend: "os-keychain" | "file";
  mock: boolean;
  server_status: ServerStatus;
  version: string;
}

export interface OAuthProgress {
  stage:
    | "discovering"
    | "registering"
    | "browser"
    | "waiting"
    | "exchanging"
    | "done";
  detail: string;
}
