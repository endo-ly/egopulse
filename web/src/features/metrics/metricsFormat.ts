import type { MetricsRange } from "../../shared/api/metrics";

/** Compact count: 950, 12.3k, 4.56M, 1.2B. */
export function formatCount(n: number): string {
  const abs = Math.abs(n);
  if (abs < 1000) return String(Math.round(n));
  const units: [number, string][] = [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "k"],
  ];
  for (const [size, unit] of units) {
    if (abs >= size) {
      const value = n / size;
      const digits = Math.abs(value) >= 100 ? 0 : Math.abs(value) >= 10 ? 1 : 2;
      const fixed = value.toFixed(digits);
      return `${fixed.includes(".") ? fixed.replace(/\.?0+$/, "") : fixed}${unit}`;
    }
  }
  return String(n);
}

/** A ratio as a percentage; null when the denominator is zero. */
export function formatPercent(numerator: number, denominator: number): string | null {
  if (denominator <= 0) return null;
  const pct = (numerator / denominator) * 100;
  if (pct > 0 && pct < 0.1) return "<0.1%";
  return `${pct >= 10 || pct === 0 ? pct.toFixed(0) : pct.toFixed(1)}%`;
}

/**
 * Average input tokens per LLM call. Sleep tokens are part of `totals.input_tokens`
 * but not of `llm_calls` or `by_model`, so both sides use `by_model`.
 */
export function averageInputTokensPerCall(usage: {
  totals: { input_tokens: number; llm_calls: number };
  by_model: { input_tokens: number }[];
}): number | null {
  if (usage.totals.llm_calls <= 0) return null;
  const inputTokens = usage.by_model.reduce((sum, model) => sum + model.input_tokens, 0);
  return inputTokens / usage.totals.llm_calls;
}

/** Change against the previous period, e.g. "+12%" — null without a baseline. */
export function formatChange(current: number, previous: number): { text: string; direction: "up" | "down" | "flat" } | null {
  if (previous <= 0) return null;
  const pct = ((current - previous) / previous) * 100;
  if (Math.abs(pct) < 1) return { text: "±0%", direction: "flat" };
  const rounded = Math.abs(pct) >= 10 ? Math.round(pct) : Number(pct.toFixed(1));
  return { text: `${rounded > 0 ? "+" : ""}${rounded}%`, direction: rounded > 0 ? "up" : "down" };
}

/** Milliseconds as "850ms", "8.2s", "1m 12s". */
export function formatLatency(ms: number | null): string {
  if (ms === null) return "—";
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${Math.round(seconds % 60)}s`;
}

/** Uptime as "3d 4h", "5h 12m", "8m". */
export function formatUptime(totalSeconds: number): string {
  const days = Math.floor(totalSeconds / 86_400);
  const hours = Math.floor((totalSeconds % 86_400) / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

const RANGE_PHRASES: Record<MetricsRange, string> = {
  "24h": "the last 24 hours",
  "7d": "the last 7 days",
  "30d": "the last 30 days",
};

export function rangePhrase(range: MetricsRange): string {
  return RANGE_PHRASES[range];
}

const KIND_LABELS: Record<string, string> = {
  agent_loop: "Conversation",
  pulse: "Pulse",
  sleep: "Sleep",
  compaction: "Compaction",
};

/** Human name of an LLM request kind. */
export function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

/** Fixed order so a kind keeps its color whatever the data holds. */
export const KIND_ORDER = ["agent_loop", "pulse", "sleep", "compaction"];

/**
 * Categorical series colors stepped for the dark surface, assigned in this
 * fixed order (validated: adjacent CVD ΔE ≥ 8.4, contrast ≥ 3:1 on panel).
 */
const SERIES_COLORS = [
  "#3987e5",
  "#d95926",
  "#199e70",
  "#c98500",
  "#d55181",
  "#008300",
  "#9085e9",
  "#e66767",
];

/** Color for the series at `index`; past the palette, everything is "Other". */
export function seriesColor(index: number): string {
  return SERIES_COLORS[index] ?? OTHER_COLOR;
}

export const OTHER_COLOR = "#6b7280";

/** Bucket axis label: "14:00" for hourly buckets, "Oct 3" for days. */
export function bucketLabel(iso: string, bucket: "hour" | "day", timeZone: string): string {
  const date = new Date(iso);
  try {
    return bucket === "hour"
      ? date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", timeZone })
      : date.toLocaleDateString(undefined, { month: "short", day: "numeric", timeZone });
  } catch {
    return bucket === "hour" ? date.toLocaleTimeString() : date.toLocaleDateString();
  }
}

/** Full bucket label for tooltips: "Tue, Oct 3" or "Oct 3, 14:00–15:00". */
export function bucketTitle(iso: string, bucket: "hour" | "day", timeZone: string): string {
  const date = new Date(iso);
  try {
    if (bucket === "day") {
      return date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric", timeZone });
    }
    const end = new Date(date.getTime() + 3_600_000);
    const day = date.toLocaleDateString(undefined, { month: "short", day: "numeric", timeZone });
    const time = (d: Date) => d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", timeZone });
    return `${day}, ${time(date)}–${time(end)}`;
  } catch {
    return date.toLocaleString();
  }
}
