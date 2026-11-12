import { apiFetch } from "./client";

/** How a secret is supplied. Values never leave the server. */
export interface SecretStatus {
  kind: "env" | "exec" | "literal" | null;
  /** Environment variable name for `env`. */
  ref: string | null;
  /** For `env`: whether the value comes from the process environment or `.env`. */
  origin: "process" | "dotenv" | null;
  configured: boolean;
}

export type LlmSource = "profile" | "agent" | "global_default" | "provider_default";

/** Compaction token settings for a single model. */
export interface CompactionModelOverride {
  model: string;
  reserve_tokens: number | null;
  keep_recent_tokens: number | null;
}

export interface GeneralConfig {
  default_agent: string;
  default_provider: string;
  default_model: string | null;
  timezone: string;
  max_history_messages: number;
  compaction_enabled: boolean;
  compaction_reserve_tokens: number;
  compaction_keep_recent_tokens: number;
  /** Token settings that apply to one `provider/model` instead of the global ones. */
  compaction_model_overrides: CompactionModelOverride[];
  default_context_window_tokens: number;
  log_level: string;
  state_root: string;
  defaults: {
    max_history_messages: number;
    compaction_enabled: boolean;
    compaction_reserve_tokens: number;
    compaction_keep_recent_tokens: number;
    default_context_window_tokens: number;
    timezone: string;
    log_level: string;
  };
}

export interface AgentConfigEntry {
  id: string;
  label: string;
  is_default: boolean;
  provider: string | null;
  model: string | null;
  reasoning_effort: string | null;
  discord_bot: string | null;
  telegram_bot: string | null;
  effective: { provider: string; model: string; provider_source: LlmSource; model_source: LlmSource } | null;
  profiles: {
    channel: string;
    provider: string | null;
    model: string | null;
    effective: { provider: string; model: string } | null;
  }[];
}

export interface ProviderConfigEntry {
  id: string;
  label: string;
  base_url: string;
  default_model: string;
  models: { name: string; context_window_tokens: number | null; has_instructions: boolean }[];
  api_key: SecretStatus;
  /** Providers like local servers or OAuth presets that work without a key. */
  api_key_optional: boolean;
  header_names: string[];
  used_by: string[];
}

export interface BotEntry {
  id: string;
  token: SecretStatus;
  agents: string[];
}

export interface ChatChannelEntry {
  id: string;
  agents: string[];
  require_mention: boolean;
  multi_agent: boolean;
  secret: boolean;
  tool_progress: boolean;
}

export interface ChannelsConfig {
  web: { enabled: boolean; host: string; port: number; auth_token: SecretStatus; allowed_origins: string[] };
  discord: { enabled: boolean; bots: BotEntry[]; channels: ChatChannelEntry[] };
  telegram: { enabled: boolean; bots: BotEntry[]; channels: ChatChannelEntry[] };
  voice: {
    enabled: boolean;
    auth_token: SecretStatus;
    default_surface: string;
    default_session: string;
    allowed_surfaces: string[];
  };
}

export interface SleepConfig {
  enabled: boolean;
  schedule: string | null;
  /** null targets every agent. */
  agents: string[] | null;
  provider: string | null;
  model: string | null;
  reasoning_effort: string | null;
  retry_max_attempts: number;
  retry_interval_minutes: number;
  effective: { provider: string; model: string } | null;
}

export interface WebhookEntry {
  id: string;
  token: SecretStatus;
  channel: string;
  thread: string;
  agent: string | null;
}

export interface McpServerEntry {
  name: string;
  transport: string;
  /** Program name only; arguments are not returned. */
  command: string | null;
  /** `scheme://host[:port]` only. */
  endpoint: string | null;
  env_keys: string[];
  header_keys: string[];
  request_timeout_secs: number;
}

export interface RuntimeConfig {
  revision: number;
  fingerprint: string;
  config_path: string;
  /** Field path → environment variable that currently overrides it. */
  env_overrides: Record<string, string>;
  general: GeneralConfig;
  agents: AgentConfigEntry[];
  providers: ProviderConfigEntry[];
  channels: ChannelsConfig;
  sleep: SleepConfig;
  pulse: { tick_interval: string; default_tick_interval: string };
  webhooks: WebhookEntry[];
  backup: { enabled: boolean; interval_days: number; time: string; max_generations: number };
  web_fetch: {
    allowed_schemes: string[];
    timeout_secs: number;
    max_fetch_bytes: number;
    max_output_bytes: number;
    allow_private_ips: boolean;
    denylist: string[];
    allowlist: string[];
    content_validation: { enabled: boolean; strict_mode: boolean; max_scan_bytes: number };
  };
}

export interface McpDefinitions {
  /** Definition files in the order they are read; later ones win. */
  files: string[];
  servers: McpServerEntry[];
  error: string | null;
}

export async function fetchConfig(authToken: string): Promise<RuntimeConfig> {
  return apiFetch<RuntimeConfig>("/api/config", authToken);
}

export async function fetchConfigYaml(
  authToken: string,
): Promise<{ path: string; revision: number; content: string }> {
  return apiFetch("/api/config/yaml", authToken);
}

export async function fetchConfigMcp(authToken: string): Promise<McpDefinitions> {
  return apiFetch<McpDefinitions>("/api/config/mcp", authToken);
}

/** The config targets the WebUI can update, with the path each one uses. */
export type ConfigTarget =
  | { kind: "general" }
  | { kind: "agent"; id: string }
  | { kind: "provider"; id: string }
  | { kind: "sleep" }
  | { kind: "pulse" };

function targetPath(target: ConfigTarget): string {
  switch (target.kind) {
    case "general":
    case "sleep":
    case "pulse":
      return `/api/config/${target.kind}`;
    case "agent":
      return `/api/config/agents/${encodeURIComponent(target.id)}`;
    case "provider":
      return `/api/config/providers/${encodeURIComponent(target.id)}`;
  }
}

/** Sent when a request was built from an outdated config. */
export class ConfigConflictError extends Error {
  constructor() {
    super("The config changed elsewhere.");
  }
}

/** Sends only the changed fields; the server rejects a stale fingerprint. */
export async function patchConfig(
  authToken: string,
  target: ConfigTarget,
  fingerprint: string,
  changes: Record<string, unknown>,
): Promise<RuntimeConfig> {
  try {
    return await apiFetch<RuntimeConfig>(targetPath(target), authToken, {
      method: "PATCH",
      body: JSON.stringify({ expected_fingerprint: fingerprint, ...changes }),
    });
  } catch (error) {
    if (error instanceof Error && error.message === "config_conflict") throw new ConfigConflictError();
    throw error;
  }
}
