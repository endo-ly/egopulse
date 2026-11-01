import { apiFetch } from "./client";

export const METRICS_RANGES = ["24h", "7d", "30d"] as const;
export type MetricsRange = (typeof METRICS_RANGES)[number];

export interface UsageTotals {
  tokens: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  /** Input tokens of the calls whose provider reported cache usage. */
  cache_reported_input_tokens: number;
  llm_calls: number;
}

export interface UsageBucket {
  start: string;
  by_agent: Record<string, number>;
  by_kind: Record<string, number>;
}

export interface KindUsage {
  kind: string;
  tokens: number;
  /** LLM calls; Sleep reports runs instead. */
  llm_calls: number;
}

export interface ModelUsage {
  provider: string;
  model: string;
  llm_calls: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  cache_reported_input_tokens: number;
}

export interface IntentionUsage {
  agent_id: string;
  intention_id: string;
  runs: number;
  failed_runs: number;
  input_tokens: number;
  output_tokens: number;
}

export interface TurnSummary {
  total: number;
  previous_total: number;
  completed: number;
  failed: number;
  cancelled: number;
  uncertain: number;
  running: number;
  duration_p50_ms: number | null;
  duration_p95_ms: number | null;
}

export interface KindCount {
  kind: string;
  count: number;
}

export interface ToolSummary {
  tool: string;
  calls: number;
  failed: number;
}

export interface FailureEntry {
  at: string;
  source: "turn" | "pulse" | "sleep";
  agent_id: string;
  /** Turn error kind, Pulse intention id, or Sleep status. */
  kind: string;
  summary: string | null;
  session_key: string | null;
  run_id: string | null;
}

export interface AgentMetrics {
  agent_id: string;
  label: string;
  turns: number;
  failed_turns: number;
  tokens: number;
  last_active_at: string | null;
  pulse_runs: number;
  pulse_failed: number;
  last_sleep_status: string | null;
}

export interface MetricsReport {
  range: { key: MetricsRange; start: string; end: string; bucket: "hour" | "day"; timezone: string };
  agent: string | null;
  usage: {
    totals: UsageTotals;
    previous_tokens: number;
    series: UsageBucket[];
    by_kind: KindUsage[];
    by_model: ModelUsage[];
    pulse_intentions: IntentionUsage[];
    pulse_unattributed: { input_tokens: number; output_tokens: number };
  };
  reliability: {
    turns: TurnSummary;
    turn_failure_kinds: KindCount[];
    tools: ToolSummary[];
    tool_failure_kinds: KindCount[];
    runs: {
      pulse: { total: number; failed: number };
      sleep: { total: number; failed: number; partial_failure: number };
    };
    recent_failures: FailureEntry[];
  };
  agents: AgentMetrics[];
}

export async function fetchMetrics(
  authToken: string,
  range: MetricsRange,
  agentId: string | null,
): Promise<MetricsReport> {
  const params = new URLSearchParams({ range });
  if (agentId) params.set("agent", agentId);
  return apiFetch<MetricsReport>(`/api/metrics?${params}`, authToken);
}

export type ChannelState = "starting" | "running" | "failed" | "stopped";

export interface RuntimeStatus {
  ok: boolean;
  version: string;
  uptime_secs: number;
  db: { ok: boolean };
  accepting_inputs: boolean;
  shutdown_started: boolean;
  critical_task_failure?: string;
  channels: Record<string, { state: ChannelState; last_error: string | null; last_activity: string | null }>;
  mcp: {
    connected: { name: string; transport: string; tools: string[] }[];
    failed: { name: string; error: string }[];
  };
  bash_sandbox: { enabled: boolean; reason?: string };
  active_turns: number;
}

export async function fetchRuntimeStatus(authToken: string): Promise<RuntimeStatus> {
  return apiFetch<RuntimeStatus>("/api/status", authToken);
}
