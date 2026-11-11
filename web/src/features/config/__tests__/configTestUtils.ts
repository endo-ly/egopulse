import { vi } from "vitest";

import type { McpDefinitions, RuntimeConfig, SecretStatus } from "../../../shared/api/config";
import type { RuntimeStatus } from "../../../shared/api/metrics";

const secret = (configured: boolean): SecretStatus => ({
  kind: configured ? "env" : null,
  ref: configured ? "TEST_TOKEN" : null,
  origin: configured ? "dotenv" : null,
  configured,
});

export function runtimeConfig(): RuntimeConfig {
  return {
    revision: 42,
    fingerprint: "fingerprint",
    config_path: "/home/user/.egopulse/egopulse.config.yaml",
    env_overrides: {},
    general: {
      default_agent: "lyre",
      default_provider: "openrouter",
      default_model: null,
      timezone: "Asia/Tokyo",
      max_history_messages: 50,
      compaction_enabled: true,
      compaction_reserve_tokens: 16384,
      compaction_keep_recent_tokens: 20000,
      default_context_window_tokens: 131072,
      log_level: "info",
      state_root: "/home/user/.egopulse",
      defaults: {
        max_history_messages: 50,
        compaction_enabled: true,
        compaction_reserve_tokens: 16384,
        compaction_keep_recent_tokens: 20000,
        default_context_window_tokens: 131072,
        timezone: "UTC",
        log_level: "info",
      },
    },
    agents: [
      {
        id: "lyre",
        label: "Lyre",
        is_default: true,
        provider: "openrouter",
        model: "openai/gpt-5",
        reasoning_effort: "medium",
        discord_bot: null,
        telegram_bot: null,
        effective: {
          provider: "openrouter",
          model: "openai/gpt-5",
          provider_source: "agent",
          model_source: "agent",
        },
        profiles: [],
      },
    ],
    providers: [
      {
        id: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-5",
        models: [{ name: "openai/gpt-5", context_window_tokens: 128_000, has_instructions: false }],
        api_key: secret(true),
        api_key_optional: false,
        header_names: [],
        used_by: ["lyre"],
      },
    ],
    channels: {
      web: { enabled: true, host: "127.0.0.1", port: 10_961, auth_token: secret(true), allowed_origins: [] },
      discord: { enabled: false, bots: [], channels: [] },
      telegram: { enabled: false, bots: [], channels: [] },
      voice: {
        enabled: false,
        auth_token: secret(false),
        default_surface: "web",
        default_session: "main",
        allowed_surfaces: [],
      },
    },
    sleep: {
      enabled: true,
      schedule: "03:00",
      agents: null,
      provider: null,
      model: null,
      reasoning_effort: null,
      retry_max_attempts: 2,
      retry_interval_minutes: 5,
      effective: { provider: "openrouter", model: "openai/gpt-5" },
    },
    pulse: { tick_interval: "1m", default_tick_interval: "1m" },
    webhooks: [],
    backup: { enabled: true, interval_days: 7, time: "03:00", max_generations: 12 },
    web_fetch: {
      allowed_schemes: ["https"],
      timeout_secs: 15,
      max_fetch_bytes: 524_288,
      max_output_bytes: 65_536,
      allow_private_ips: false,
      denylist: [],
      allowlist: [],
      content_validation: { enabled: true, strict_mode: false, max_scan_bytes: 65_536 },
    },
  };
}

export function runtimeStatus(): RuntimeStatus {
  return {
    ok: true,
    version: "test",
    uptime_secs: 60,
    db: { ok: true },
    accepting_inputs: true,
    shutdown_started: false,
    channels: {},
    mcp: { connected: [], failed: [] },
    bash_sandbox: { enabled: true },
    active_turns: 0,
  };
}

export const EMPTY_MCP: McpDefinitions = { files: [], servers: [], error: null };

export const CONFIG_YAML = {
  path: "/home/user/.egopulse/egopulse.config.yaml",
  revision: 42,
  content: "default_agent: lyre\n",
};

export interface ConfigApi {
  config?: RuntimeConfig;
  mcp?: McpDefinitions;
  yaml?: typeof CONFIG_YAML;
  status?: RuntimeStatus;
}

/** Serves the Config APIs from fixtures and records requested URLs. */
export function stubConfigApi(api: ConfigApi = {}): URL[] {
  const requested: URL[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input), "http://localhost");
      requested.push(url);
      let body: unknown = {};
      if (url.pathname === "/api/config") body = api.config ?? runtimeConfig();
      else if (url.pathname === "/api/config/mcp") body = api.mcp ?? EMPTY_MCP;
      else if (url.pathname === "/api/config/yaml") body = api.yaml ?? CONFIG_YAML;
      else if (url.pathname === "/api/status") body = api.status ?? runtimeStatus();
      return Promise.resolve({ ok: true, json: () => Promise.resolve(body) });
    }),
  );
  return requested;
}
