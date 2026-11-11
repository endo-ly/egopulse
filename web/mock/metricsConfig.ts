// Mock data for the Metrics and Config screens: GET /api/status,
// GET /api/metrics, GET /api/config, GET /api/config/yaml, and the
// PATCH /api/config/* updates (src/channels/web/{health,metrics,config}.rs).

const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

/** Deterministic pseudo-random in [0, 1) so reloads look the same. */
function noise(seed: number): number {
  const x = Math.sin(seed * 12.9898) * 43758.5453;
  return x - Math.floor(x);
}

export function runtimeStatus() {
  return {
    ok: true,
    version: "2026.10.31",
    uptime_secs: 3 * 86_400 + 4 * 3600 + 12 * 60,
    pid: 4242,
    db: { ok: true },
    accepting_inputs: true,
    shutdown_started: false,
    owned_task_count: 9,
    channels: {
      web: { state: "running", last_error: null, last_activity: new Date(Date.now() - 40_000).toISOString() },
      discord: { state: "running", last_error: null, last_activity: new Date(Date.now() - 3 * 60_000).toISOString() },
      telegram: { state: "failed", last_error: "Unauthorized: bot token was revoked", last_activity: null },
    },
    mcp: {
      connected: [
        { name: "egograph", transport: "streamable_http", tools: ["search", "recent", "entity", "timeline", "write"] },
        { name: "github", transport: "stdio", tools: ["issues", "pulls", "repos", "search_code"] },
      ],
      failed: [{ name: "calendar", error: "connection refused (127.0.0.1:7801)" }],
    },
    bash_sandbox: { enabled: true },
    active_turns: 2,
    recent_errors_count: 3,
    instance_lock: { held: true },
  };
}

// --- Metrics ---------------------------------------------------------------

const USAGE_AGENTS: { id: string; label: string; weight: number }[] = [
  { id: "lyre", label: "Lyre", weight: 1 },
  { id: "ace", label: "Ace", weight: 0.45 },
  { id: "vega", label: "Vega", weight: 0.7 },
  { id: "orion", label: "Orion", weight: 0.22 },
];

const RANGE_BUCKETS = { "24h": { count: 24, ms: HOUR_MS }, "7d": { count: 7, ms: DAY_MS }, "30d": { count: 30, ms: DAY_MS } };
type RangeKey = keyof typeof RANGE_BUCKETS;

function bucketStarts(range: RangeKey): number[] {
  const { count, ms } = RANGE_BUCKETS[range];
  const now = new Date();
  const last =
    ms === HOUR_MS
      ? new Date(now.getFullYear(), now.getMonth(), now.getDate(), now.getHours()).getTime()
      : new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  return Array.from({ length: count }, (_, i) => last - (count - 1 - i) * ms);
}

export function metricsReport(rangeParam: string | null, agentParam: string | null) {
  const range: RangeKey = rangeParam === "24h" || rangeParam === "30d" ? rangeParam : "7d";
  const hourly = range === "24h";
  const scale = hourly ? 1 / 18 : 1;
  const starts = bucketStarts(range);
  const agents = USAGE_AGENTS.filter((a) => !agentParam || a.id === agentParam);
  const factor = agents.reduce((s, a) => s + a.weight, 0) / USAGE_AGENTS.reduce((s, a) => s + a.weight, 0);

  const series = starts.map((start, i) => {
    const by_agent: Record<string, number> = {};
    const by_kind: Record<string, number> = { agent_loop: 0, pulse: 0, sleep: 0, compaction: 0 };
    const hour = new Date(start).getHours();
    const daytime = hourly ? (hour >= 8 && hour <= 23 ? 1 : 0.25) : 1;
    for (const agent of agents) {
      const conversation = Math.round(2_400_000 * agent.weight * scale * daytime * (0.4 + noise(i + agent.weight * 7)));
      const pulse = Math.round(4_100_000 * agent.weight * scale * (0.6 + noise(i * 3 + agent.weight)) * (agent.id === "lyre" ? 1.6 : 1));
      const sleep = !hourly || hour === 4 ? Math.round(180_000 * agent.weight * (0.5 + noise(i + 11))) : 0;
      const compaction = noise(i * 5 + agent.weight) > 0.8 ? Math.round(40_000 * agent.weight) : 0;
      by_agent[agent.id] = conversation + pulse + sleep + compaction;
      by_kind.agent_loop += conversation;
      by_kind.pulse += pulse;
      by_kind.sleep += sleep;
      by_kind.compaction += compaction;
    }
    for (const kind of Object.keys(by_kind)) if (by_kind[kind] === 0) delete by_kind[kind];
    return { start: new Date(start).toISOString(), by_agent, by_kind };
  });

  const sum = (kind: string) => series.reduce((s, b) => s + (b.by_kind[kind] ?? 0), 0);
  const conversation = sum("agent_loop");
  const pulse = sum("pulse");
  const sleep = sum("sleep");
  const compaction = sum("compaction");
  const tokens = conversation + pulse + sleep + compaction;
  const llmTokens = conversation + pulse + compaction;
  const input = Math.round(llmTokens * 0.94);
  const output = llmTokens - input;
  const conversationCalls = Math.round(conversation / 88_000);
  const pulseCalls = Math.round(pulse / 52_000);
  const compactionCalls = Math.round(compaction / 30_000);
  const calls = conversationCalls + pulseCalls + compactionCalls;
  const days = hourly ? 1 : RANGE_BUCKETS[range].count;

  const models = [
    { provider: "sakura", model: "preview/Kimi-K2.6", share: 0.58, cache: 0.46 },
    { provider: "openrouter", model: "anthropic/claude-sonnet-4", share: 0.31, cache: 0.62 },
    { provider: "openai-codex", model: "gpt-5.3-codex", share: 0.11, cache: null },
  ];

  const intentions = [
    { agent_id: "lyre", intention_id: "research_content", runs: 3 * days, failed: Math.round(days / 7), share: 0.43 },
    { agent_id: "vega", intention_id: "hn_news", runs: 5 * days, failed: 0, share: 0.2 },
    { agent_id: "lyre", intention_id: "user_activity_review", runs: 5 * days, failed: 0, share: 0.08 },
    { agent_id: "ace", intention_id: "riffra_review", runs: 5 * days, failed: 1, share: 0.06 },
    { agent_id: "orion", intention_id: "egopulse_review", runs: 5 * days, failed: 0, share: 0.02 },
    { agent_id: "lyre", intention_id: "idle_murmur", runs: 16 * days, failed: 0, share: 0.04 },
    { agent_id: "vega", intention_id: "idle_murmur", runs: 16 * days, failed: 0, share: 0.04 },
  ]
    .filter((row) => !agentParam || row.agent_id === agentParam)
    .map((row) => {
      const total = Math.round(pulse * row.share / Math.max(factor, 0.1));
      return {
        agent_id: row.agent_id,
        intention_id: row.intention_id,
        runs: hourly ? Math.max(1, Math.round(row.runs / 2)) : row.runs,
        failed_runs: row.failed,
        input_tokens: Math.round(total * 0.97),
        output_tokens: Math.round(total * 0.03),
      };
    })
    .sort((a, b) => b.input_tokens - a.input_tokens);

  const turnTotal = Math.round(conversationCalls / 3.2);
  const failedTurns = Math.max(0, Math.round(turnTotal * 0.018));
  const toolCalls = Math.round(conversationCalls * 2.1 + pulseCalls * 0.8);
  const tools = [
    { tool: "bash", share: 0.34, rate: 0.12 },
    { tool: "read", share: 0.22, rate: 0.01 },
    { tool: "mcp_egograph_search", share: 0.12, rate: 0.06 },
    { tool: "web_fetch", share: 0.09, rate: 0.08 },
    { tool: "edit", share: 0.08, rate: 0.03 },
    { tool: "write", share: 0.05, rate: 0 },
    { tool: "grep", share: 0.1, rate: 0 },
  ].map((t) => {
    const n = Math.round(toolCalls * t.share);
    return { tool: t.tool, calls: n, failed: Math.round(n * t.rate) };
  });
  const toolFailures = tools.reduce((s, t) => s + t.failed, 0);

  const now = Date.now();
  const failures = [
    {
      at: new Date(now - 47 * 60_000).toISOString(),
      source: "turn",
      agent_id: "lyre",
      kind: "llm_error",
      summary: "provider returned 429 Too Many Requests: quota exhausted for preview/Kimi-K2.6",
      session_key: "dev-discussion",
      run_id: null,
    },
    {
      at: new Date(now - 38 * HOUR_MS).toISOString(),
      source: "pulse",
      agent_id: "lyre",
      kind: "evening_check",
      summary: "LLM request timed out after 120s",
      session_key: null,
      run_id: "pulse-e2",
    },
    {
      at: new Date(now - 2.2 * DAY_MS).toISOString(),
      source: "sleep",
      agent_id: "ace",
      kind: "partial_failure",
      summary: "semantic_update: output did not match the expected memory format",
      session_key: null,
      run_id: "run-20260703-ace-scheduled",
    },
    {
      at: new Date(now - 4.5 * DAY_MS).toISOString(),
      source: "turn",
      agent_id: "vega",
      kind: "interrupted",
      summary: "runtime restarted while a tool was running",
      session_key: "morning-notes",
      run_id: null,
    },
  ].filter((f) => (!agentParam || f.agent_id === agentParam) && now - new Date(f.at).getTime() < days * DAY_MS);

  const allAgentsTokens = (id: string) =>
    metricsAgentTokens(range).find((row) => row.id === id)?.tokens ?? 0;

  return {
    ok: true,
    range: {
      key: range,
      start: new Date(starts[0]).toISOString(),
      end: new Date().toISOString(),
      bucket: hourly ? "hour" : "day",
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    },
    agent: agentParam,
    usage: {
      totals: {
        tokens,
        input_tokens: input + Math.round(sleep * 0.9),
        output_tokens: output + Math.round(sleep * 0.1),
        cached_input_tokens: Math.round(input * 0.41),
        cache_reported_input_tokens: Math.round(input * 0.89),
        llm_calls: calls,
      },
      previous_tokens: Math.round(tokens * 0.88),
      series,
      by_kind: [
        { kind: "pulse", tokens: pulse, llm_calls: pulseCalls },
        { kind: "agent_loop", tokens: conversation, llm_calls: conversationCalls },
        { kind: "sleep", tokens: sleep, llm_calls: hourly ? 1 : days * agents.length },
        { kind: "compaction", tokens: compaction, llm_calls: compactionCalls },
      ].filter((k) => k.tokens > 0),
      by_model: models.map((m) => {
        const t = Math.round(llmTokens * m.share);
        const inTokens = Math.round(t * 0.94);
        return {
          provider: m.provider,
          model: m.model,
          llm_calls: Math.round(calls * m.share),
          input_tokens: inTokens,
          output_tokens: t - inTokens,
          cached_input_tokens: m.cache === null ? 0 : Math.round(inTokens * m.cache),
          cache_reported_input_tokens: m.cache === null ? 0 : inTokens,
        };
      }),
      pulse_intentions: intentions,
      pulse_unattributed: { input_tokens: Math.round(pulse * 0.012), output_tokens: 0 },
    },
    reliability: {
      turns: {
        total: turnTotal,
        previous_total: Math.round(turnTotal * 1.04),
        completed: turnTotal - failedTurns - 2,
        failed: failedTurns,
        cancelled: 1,
        uncertain: 1,
        running: 0,
        duration_p50_ms: 8_400,
        duration_p95_ms: 72_000,
      },
      turn_failure_kinds: [
        { kind: "llm_error", count: Math.max(1, failedTurns - 1) },
        { kind: "interrupted", count: 1 },
      ],
      tools: tools.sort((a, b) => b.failed - a.failed || b.calls - a.calls),
      tool_failure_kinds: [
        { kind: "exit_nonzero", count: Math.round(toolFailures * 0.46) },
        { kind: "timeout", count: Math.round(toolFailures * 0.21) },
        { kind: "guard_denied", count: Math.round(toolFailures * 0.14) },
        { kind: "upstream", count: Math.round(toolFailures * 0.12) },
        { kind: "not_found", count: Math.round(toolFailures * 0.07) },
      ],
      runs: {
        pulse: { total: intentions.reduce((s, r) => s + r.runs, 0), failed: intentions.reduce((s, r) => s + r.failed_runs, 0) },
        sleep: { total: hourly ? agents.length : days * agents.length, failed: 0, partial_failure: failures.some((f) => f.source === "sleep") ? 1 : 0 },
      },
      recent_failures: failures,
    },
    agents: [
      ...USAGE_AGENTS.map((agent) => ({
        agent_id: agent.id,
        label: agent.label,
        turns: Math.round((allAgentsTokens(agent.id) / 1_000_000) * 2.6),
        failed_turns: agent.id === "lyre" ? 2 : agent.id === "vega" ? 1 : 0,
        tokens: allAgentsTokens(agent.id),
        last_active_at: new Date(now - (agent.id === "lyre" ? 3 : agent.id === "vega" ? 55 : 600) * 60_000).toISOString(),
        pulse_runs: agent.id === "orion" ? 5 * days : 21 * days,
        pulse_failed: agent.id === "lyre" ? 1 : 0,
        last_sleep_status: agent.id === "ace" ? "partial_failure" : "success",
      })),
      { agent_id: "nova", label: "Nova", turns: 0, failed_turns: 0, tokens: 0, last_active_at: null, pulse_runs: 0, pulse_failed: 0, last_sleep_status: null },
    ],
  };
}

function metricsAgentTokens(range: RangeKey) {
  const days = range === "24h" ? 1 : RANGE_BUCKETS[range].count;
  const scale = range === "24h" ? 1 : days;
  return USAGE_AGENTS.map((agent) => ({ id: agent.id, tokens: Math.round(6_600_000 * agent.weight * scale * (agent.id === "lyre" ? 1.25 : 1)) }));
}

// --- Config ----------------------------------------------------------------

let revision = 42;
let fingerprint = "f-42";

const secretEnv = (ref: string, origin: "process" | "dotenv" = "dotenv") => ({
  kind: "env",
  ref,
  origin,
  configured: true,
});

const config = {
  general: {
    default_agent: "lyre",
    default_provider: "openrouter",
    default_model: null as string | null,
    timezone: "Asia/Tokyo",
    max_history_messages: 50,
    compaction_enabled: true,
    compaction_reserve_tokens: 16384,
    compaction_keep_recent_tokens: 20000,
    default_context_window_tokens: 128000,
    log_level: "info",
    state_root: "/home/user/.egopulse",
  },
  agents: [
    { id: "lyre", label: "Lyre", provider: "sakura", model: "preview/Kimi-K2.6", reasoning_effort: "medium", discord_bot: "lyre", telegram_bot: null, profiles: [{ channel: "voice", provider: "openrouter", model: "openai/gpt-4.1-mini" }] },
    { id: "ace", label: "Ace", provider: null, model: null, reasoning_effort: null, discord_bot: "main", telegram_bot: null, profiles: [] },
    { id: "vega", label: "Vega", provider: "openai-codex", model: "gpt-5.3-codex", reasoning_effort: "high", discord_bot: "vega", telegram_bot: "default", profiles: [] },
    { id: "orion", label: "Orion", provider: "openrouter", model: null, reasoning_effort: null, discord_bot: null, telegram_bot: null, profiles: [] },
  ] as {
    id: string;
    label: string;
    provider: string | null;
    model: string | null;
    reasoning_effort: string | null;
    discord_bot: string | null;
    telegram_bot: string | null;
    profiles: { channel: string; provider: string | null; model: string | null }[];
  }[],
  providers: [
    {
      id: "openrouter",
      label: "OpenRouter",
      base_url: "https://openrouter.ai/api/v1",
      default_model: "anthropic/claude-sonnet-4",
      models: [
        { name: "anthropic/claude-sonnet-4", context_window_tokens: 200000, has_instructions: true },
        { name: "google/gemini-2.5-pro", context_window_tokens: 1048576, has_instructions: false },
        { name: "openai/gpt-4.1-mini", context_window_tokens: null, has_instructions: false },
      ],
      api_key: secretEnv("OPENROUTER_API_KEY", "process") as {
        kind: string | null;
        ref: string | null;
        origin: string | null;
        configured: boolean;
      },
      api_key_optional: false,
      header_names: ["x-client-name"],
    },
    {
      id: "sakura",
      label: "Sakura AI",
      base_url: "https://api.ai.sakura.ad.jp/v1",
      default_model: "preview/Kimi-K2.6",
      models: [{ name: "preview/Kimi-K2.6", context_window_tokens: 262144, has_instructions: false }],
      api_key: { kind: null, ref: null, origin: null, configured: false } as {
        kind: string | null;
        ref: string | null;
        origin: string | null;
        configured: boolean;
      },
      api_key_optional: false,
      header_names: [],
    },
    {
      id: "openai-codex",
      label: "OpenAI Codex",
      base_url: "https://chatgpt.com/backend-api/codex",
      default_model: "gpt-5.3-codex",
      models: [{ name: "gpt-5.3-codex", context_window_tokens: 400000, has_instructions: false }],
      api_key: { kind: null, ref: null, origin: null, configured: false } as {
        kind: string | null;
        ref: string | null;
        origin: string | null;
        configured: boolean;
      },
      api_key_optional: true,
      header_names: [],
    },
  ],
  sleep: {
    enabled: true,
    schedule: "04:00" as string | null,
    agents: null as string[] | null,
    provider: null as string | null,
    model: "openai/gpt-4.1-mini" as string | null,
    reasoning_effort: null as string | null,
    retry_max_attempts: 3,
    retry_interval_minutes: 5,
  },
  pulse: { tick_interval: "1m" },
};

const GENERAL_DEFAULTS = {
  max_history_messages: 50,
  compaction_enabled: true,
  compaction_reserve_tokens: 16384,
  compaction_keep_recent_tokens: 20000,
  default_context_window_tokens: 131072,
  timezone: "UTC",
  log_level: "info",
};

function providerOf(id: string | null) {
  return config.providers.find((p) => p.id === (id ?? config.general.default_provider));
}

function effectiveFor(agent: (typeof config.agents)[number]) {
  const provider = providerOf(agent.provider);
  if (!provider) return null;
  return {
    provider: provider.id,
    model: agent.model ?? config.general.default_model ?? provider.default_model,
    provider_source: agent.provider ? "agent" : "global_default",
    model_source: agent.model ? "agent" : config.general.default_model ? "global_default" : "provider_default",
  };
}

export function configPayload() {
  return {
    ok: true,
    revision,
    fingerprint,
    config_path: "/home/user/.egopulse/egopulse.config.yaml",
    env_overrides: { "channels.web.host": "WEB_HOST" },
    general: { ...config.general, defaults: GENERAL_DEFAULTS },
    agents: config.agents.map((agent) => ({
      ...agent,
      is_default: agent.id === config.general.default_agent,
      effective: effectiveFor(agent),
      profiles: agent.profiles.map((profile) => {
        const provider = providerOf(profile.provider ?? agent.provider);
        return {
          ...profile,
          effective: provider ? { provider: provider.id, model: profile.model ?? provider.default_model } : null,
        };
      }),
    })),
    providers: config.providers.map((provider) => ({
      ...provider,
      used_by: config.agents.filter((agent) => effectiveFor(agent)?.provider === provider.id).map((agent) => agent.id),
    })),
    channels: {
      web: { enabled: true, host: "0.0.0.0", port: 10961, auth_token: secretEnv("WEB_AUTH_TOKEN"), allowed_origins: [] },
      discord: {
        enabled: true,
        bots: [
          { id: "lyre", token: secretEnv("DISCORD_BOT_TOKEN_LYRE"), agents: ["lyre"] },
          { id: "main", token: secretEnv("DISCORD_BOT_TOKEN"), agents: ["ace"] },
          { id: "vega", token: { kind: "exec", ref: null, origin: null, configured: true }, agents: ["vega"] },
        ],
        channels: [
          { id: "1234567890123456789", agents: ["lyre"], require_mention: false, multi_agent: false, secret: false, tool_progress: true },
          { id: "9876547890123456789", agents: ["lyre", "ace", "vega"], require_mention: true, multi_agent: true, secret: false, tool_progress: false },
          { id: "5550001112223334444", agents: ["vega"], require_mention: false, multi_agent: false, secret: true, tool_progress: false },
        ],
      },
      telegram: {
        enabled: true,
        bots: [{ id: "default", token: secretEnv("TELEGRAM_BOT_TOKEN"), agents: ["vega"] }],
        channels: [{ id: "-1001234567890", agents: ["vega"], require_mention: true, multi_agent: false, secret: false, tool_progress: false }],
      },
      voice: {
        enabled: true,
        auth_token: secretEnv("EGOPULSE_VOICE_AUTH_TOKEN"),
        default_surface: "stackchan",
        default_session: "main",
        allowed_surfaces: ["stackchan"],
      },
    },
    sleep: {
      ...config.sleep,
      effective: (() => {
        const provider = providerOf(config.sleep.provider);
        return provider ? { provider: provider.id, model: config.sleep.model ?? config.general.default_model ?? provider.default_model } : null;
      })(),
    },
    pulse: { tick_interval: config.pulse.tick_interval, default_tick_interval: "1m" },
    webhooks: [
      { id: "egograph", token: secretEnv("EGOPULSE_WEBHOOK_EGOGRAPH_TOKEN"), channel: "discord", thread: "1234567890123456789", agent: "lyre" },
      { id: "github", token: { kind: "literal", ref: null, origin: null, configured: true }, channel: "telegram", thread: "-1001234567890", agent: null },
    ],
    backup: { enabled: true, interval_days: 7, time: "03:00", max_generations: 12 },
    web_fetch: {
      allowed_schemes: ["https"],
      timeout_secs: 15,
      max_fetch_bytes: 524288,
      max_output_bytes: 65536,
      allow_private_ips: false,
      denylist: ["*.internal.example"],
      allowlist: [],
      content_validation: { enabled: true, strict_mode: false, max_scan_bytes: 65536 },
    },
  };
}

export function configMcp() {
  return {
    ok: true,
    files: [
      "/home/user/.egopulse/mcp.json",
      "/home/user/.egopulse/mcp.d",
      "/home/user/.egopulse/workspace/mcp.json",
      "/home/user/.egopulse/workspace/mcp.d",
    ],
    servers: [
      { name: "calendar", transport: "stdio", command: "node", endpoint: null, env_keys: ["CALENDAR_TOKEN"], header_keys: [], request_timeout_secs: 30 },
      { name: "egograph", transport: "streamable_http", command: null, endpoint: "https://egograph.example.com", env_keys: [], header_keys: ["Authorization"], request_timeout_secs: 60 },
      { name: "github", transport: "stdio", command: "npx", endpoint: null, env_keys: ["GITHUB_TOKEN"], header_keys: [], request_timeout_secs: 30 },
    ],
    error: null,
  };
}

export function configYaml() {
  const lines = [
    `default_provider: ${config.general.default_provider}`,
    `default_model: ${config.general.default_model ?? "null"}`,
    `default_agent: ${config.general.default_agent}`,
    `timezone: ${config.general.timezone}`,
    `default_context_window_tokens: ${config.general.default_context_window_tokens}`,
    "agents:",
    ...config.agents.flatMap((agent) => [
      `  ${agent.id}:`,
      `    label: ${agent.label}`,
      ...(agent.provider ? [`    provider: ${agent.provider}`] : []),
      ...(agent.model ? [`    model: ${agent.model}`] : []),
      ...(agent.reasoning_effort ? [`    reasoning_effort: ${agent.reasoning_effort}`] : []),
      ...(agent.discord_bot ? [`    discord_bot: ${agent.discord_bot}`] : []),
    ]),
    "providers:",
    "  openrouter:",
    "    label: OpenRouter",
    "    base_url: https://openrouter.ai/api/v1",
    "    api_key:",
    "      source: env",
    "      id: OPENROUTER_API_KEY",
    "    headers:",
    "      x-client-name: '********'",
    "channels:",
    "  discord:",
    "    enabled: true",
    "    bots:",
    "      vega:",
    "        token:",
    "          source: exec",
    "          command: '********'",
    "webhooks:",
    "  receivers:",
    "    github:",
    "      token: '********'",
    "sleep_batch:",
    `  enabled: ${config.sleep.enabled}`,
    `  schedule: '${config.sleep.schedule ?? ""}'`,
    "pulse:",
    `  tick_interval: ${config.pulse.tick_interval}`,
  ];
  return { ok: true, path: "/home/user/.egopulse/egopulse.config.yaml", revision, fingerprint, content: `${lines.join("\n")}\n` };
}

/** Applies a PATCH like the server: stale fingerprint → 409, else merge and bump. */
export function patchConfig(target: string, body: Record<string, unknown>): { status: number; body: unknown } {
  if (body.expected_fingerprint !== fingerprint) {
    return { status: 409, body: { ok: false, error: "config_conflict" } };
  }
  const { expected_fingerprint: _ignored, ...changes } = body;
  const agentMatch = target.match(/^agents\/(.+)$/);
  const providerMatch = target.match(/^providers\/(.+)$/);
  if (target === "general") {
    Object.assign(config.general, changes);
  } else if (target === "sleep") {
    Object.assign(config.sleep, changes);
  } else if (target === "pulse") {
    if (typeof changes.tick_interval === "string" && !/^(\d+[hms])+$/.test(changes.tick_interval)) {
      return { status: 400, body: { ok: false, error: "invalid_tick_interval" } };
    }
    Object.assign(config.pulse, changes);
  } else if (agentMatch) {
    const agent = config.agents.find((a) => a.id === agentMatch[1]);
    if (!agent) return { status: 404, body: { ok: false, error: "agent_not_found" } };
    Object.assign(agent, changes);
  } else if (providerMatch) {
    const provider = config.providers.find((p) => p.id === providerMatch[1]);
    if (!provider) return { status: 404, body: { ok: false, error: "provider_not_found" } };
    const { api_key, models, ...rest } = changes as { api_key?: { action: string }; models?: string[] };
    Object.assign(provider, rest);
    if (models) {
      provider.models = models.map(
        (name) => provider.models.find((m) => m.name === name) ?? { name, context_window_tokens: null, has_instructions: false },
      );
    }
    if (!provider.models.some((m) => m.name === provider.default_model)) {
      provider.models.push({ name: provider.default_model, context_window_tokens: null, has_instructions: false });
    }
    if (api_key?.action === "replace") {
      provider.api_key = {
        kind: "env",
        ref: provider.api_key.ref ?? `${provider.id.toUpperCase().replace(/-/g, "_")}_API_KEY`,
        origin: "dotenv",
        configured: true,
      };
    } else if (api_key?.action === "remove") {
      provider.api_key = { kind: null, ref: null, origin: null, configured: false };
    }
  } else {
    return { status: 404, body: { ok: false, error: "not_found" } };
  }
  revision += 1;
  fingerprint = `f-${revision}`;
  return { status: 200, body: configPayload() };
}

/** Simulates an edit made elsewhere (YAML file, slash command). */
export function editExternally(): void {
  const orion = config.agents.find((a) => a.id === "orion");
  if (orion) orion.model = orion.model ? null : "google/gemini-2.5-pro";
  revision += 1;
  fingerprint = `f-${revision}`;
}
