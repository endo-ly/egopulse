import { useMemo, useState, type ReactNode } from "react";

import type { AgentEntry } from "../../shared/api/types";
import {
  METRICS_RANGES,
  type AgentMetrics,
  type FailureEntry,
  type MetricsRange,
  type MetricsReport,
  type RuntimeStatus,
} from "../../shared/api/metrics";
import { formatRelativeTime } from "../../shared/lib/format";
import { EmptyState } from "../../shared/ui/EmptyState";
import { QueryState } from "../../shared/ui/QueryState";
import { StatusDot, type StatusTone } from "../../shared/ui/StatusDot";
import { statusLabel, statusTone } from "../schedule/runStatus";
import { StackedBars, type StackSeries } from "./StackedBars";
import {
  KIND_ORDER,
  OTHER_COLOR,
  formatChange,
  formatCount,
  formatLatency,
  formatPercent,
  formatUptime,
  kindLabel,
  rangePhrase,
  seriesColor,
} from "./metricsFormat";
import { useMetricsReport, useRuntimeStatus } from "./metricsQuery";
import { healthIssues } from "./health";

const CHART_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
    <path d="M18 20V10M12 20V4M6 20v-6" strokeLinecap="round" />
  </svg>
);

export interface MetricsFilters {
  range: MetricsRange;
  agentId: string | null;
}

export interface MetricsPanelProps {
  authToken: string;
  agents: AgentEntry[];
  filters: MetricsFilters;
  onChangeFilters: (filters: MetricsFilters) => void;
  onOpenAgent: (agentId: string) => void;
  onOpenSession: (agentId: string, sessionKey: string) => void;
  onOpenRun: (agentId: string, kind: "pulse" | "sleep", runId: string) => void;
}

/** Ids of the scroll targets the sidebar jumps to. */
export const METRICS_SECTIONS = [
  { id: "health", label: "Health" },
  { id: "usage", label: "Usage" },
  { id: "reliability", label: "Reliability" },
  { id: "agents", label: "Agents" },
] as const;
export type MetricsSectionId = (typeof METRICS_SECTIONS)[number]["id"];

export function metricsSectionElementId(id: MetricsSectionId): string {
  return `metrics-${id}`;
}

/**
 * Runtime-wide monitoring: whether anything is broken right now, what the
 * tokens go to, what fails, and how each agent is used.
 */
export function MetricsPanel(props: MetricsPanelProps) {
  const { authToken, agents, filters, onChangeFilters } = props;
  const status = useRuntimeStatus(authToken);
  const report = useMetricsReport(authToken, filters.range, filters.agentId);
  const agentOrder = useMemo(() => agents.map((agent) => agent.id), [agents]);
  const agentLabel = (id: string) => agents.find((agent) => agent.id === id)?.label ?? id;

  return (
    <div className="screen-panel metrics-panel">
      <header className="screen-header metrics-header">
        <div className="metrics-header-text">
          <h1 className="panel-title">Metrics</h1>
          <p className="panel-description">Runtime health, token usage, and failures across every agent.</p>
        </div>
        <div className="metrics-filters">
          <label className="metrics-agent-filter">
            <span className="sr-only">Agent</span>
            <select
              value={filters.agentId ?? ""}
              onChange={(event) => onChangeFilters({ ...filters, agentId: event.target.value || null })}
            >
              <option value="">All agents</option>
              {agents.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.label}
                </option>
              ))}
            </select>
          </label>
          <div className="segmented" role="radiogroup" aria-label="Range">
            {METRICS_RANGES.map((range) => (
              <button
                key={range}
                type="button"
                role="radio"
                aria-checked={filters.range === range}
                className={filters.range === range ? "active" : ""}
                onClick={() => onChangeFilters({ ...filters, range })}
              >
                {range}
              </button>
            ))}
          </div>
        </div>
      </header>
      <div className="agent-panel-body metrics-body">
        <Section id="health" title="Health" aside="Live">
          <QueryState state={status}>{(data) => <HealthView status={data} />}</QueryState>
        </Section>
        <QueryState state={report}>
          {(data) => (
            <>
              <Section id="usage" title="Usage" aside={capitalize(rangePhrase(data.range.key))}>
                <UsageView report={data} agentOrder={agentOrder} agentLabel={agentLabel} />
              </Section>
              <Section id="reliability" title="Reliability" aside={capitalize(rangePhrase(data.range.key))}>
                <ReliabilityView
                  report={data}
                  agentLabel={agentLabel}
                  onOpenSession={props.onOpenSession}
                  onOpenRun={props.onOpenRun}
                />
              </Section>
              <Section id="agents" title="Agents" aside={`${capitalize(rangePhrase(data.range.key))}, all agents`}>
                <AgentsTable
                  rows={data.agents}
                  highlighted={filters.agentId}
                  agentOrder={agentOrder}
                  onOpenAgent={props.onOpenAgent}
                />
              </Section>
            </>
          )}
        </QueryState>
      </div>
    </div>
  );
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function Section({ id, title, aside, children }: { id: MetricsSectionId; title: string; aside?: string; children: ReactNode }) {
  const headingId = `${metricsSectionElementId(id)}-title`;
  return (
    <section id={metricsSectionElementId(id)} className="metrics-section" aria-labelledby={headingId}>
      <div className="metrics-section-head">
        <h2 id={headingId} className="run-section-title">
          {title}
        </h2>
        {aside && <span className="metrics-section-aside">{aside}</span>}
      </div>
      {children}
    </section>
  );
}

function Kpi({
  label,
  value,
  hint,
  tone,
  change,
}: {
  label: string;
  value: string;
  hint?: string;
  tone?: "danger" | "warning";
  change?: { text: string; direction: "up" | "down" | "flat" } | null;
}) {
  return (
    <div className={`metric-kpi ${tone ? `tone-${tone}` : ""}`}>
      <dt>{label}</dt>
      <dd className="metric-kpi-value">
        {value}
        {change && (
          <span className={`metric-change ${change.direction}`} title="Compared with the previous period of the same length">
            {change.direction === "up" ? "▲" : change.direction === "down" ? "▼" : ""} {change.text}
          </span>
        )}
      </dd>
      {hint && <dd className="metric-kpi-hint">{hint}</dd>}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

const CHANNEL_TONE: Record<string, StatusTone> = {
  running: "live",
  starting: "idle",
  stopped: "idle",
  failed: "error",
};

function HealthView({ status }: { status: RuntimeStatus }) {
  const issues = healthIssues(status);
  const channels = Object.entries(status.channels).sort(([a], [b]) => a.localeCompare(b));
  const verdictTone = issues.some((issue) => issue.severity === "danger")
    ? "danger"
    : issues.length > 0
      ? "warning"
      : "ok";

  return (
    <div className={`health-card verdict-${verdictTone}`} aria-live="polite">
      <div className="health-verdict">
        <StatusDot tone={verdictTone === "ok" ? "live" : verdictTone === "danger" ? "error" : "warning"} />
        <strong>
          {issues.length === 0
            ? "All systems normal"
            : `${issues.length} ${issues.length === 1 ? "issue" : "issues"} need attention`}
        </strong>
        <dl className="health-facts">
          <div>
            <dt>Version</dt>
            <dd>{status.version}</dd>
          </div>
          <div>
            <dt>Uptime</dt>
            <dd>{formatUptime(status.uptime_secs)}</dd>
          </div>
          <div>
            <dt>Running turns</dt>
            <dd>{status.active_turns}</dd>
          </div>
        </dl>
      </div>
      {issues.length > 0 && (
        <ul className="health-issues">
          {issues.map((issue) => (
            <li key={issue.key} className={`tone-${issue.severity}`}>
              <strong>{issue.title}</strong>
              {issue.detail && <span>{issue.detail}</span>}
            </li>
          ))}
        </ul>
      )}
      <div className="health-grid">
        <HealthGroup title="Channels">
          {channels.length === 0 ? (
            <span className="muted">No channels started</span>
          ) : (
            channels.map(([name, channel]) => (
              <HealthItem
                key={name}
                tone={CHANNEL_TONE[channel.state] ?? "idle"}
                name={name}
                state={channel.state}
                detail={
                  channel.last_error ??
                  (channel.last_activity ? activeAgo(channel.last_activity) : undefined)
                }
              />
            ))
          )}
        </HealthGroup>
        <HealthGroup title="MCP servers">
          {status.mcp.connected.length + status.mcp.failed.length === 0 ? (
            <span className="muted">None configured</span>
          ) : (
            <>
              {status.mcp.failed.map((server) => (
                <HealthItem key={server.name} tone="error" name={server.name} state="failed" detail={server.error} />
              ))}
              {status.mcp.connected.map((server) => (
                <HealthItem
                  key={server.name}
                  tone="live"
                  name={server.name}
                  state={`${server.tools.length} ${server.tools.length === 1 ? "tool" : "tools"}`}
                />
              ))}
            </>
          )}
        </HealthGroup>
        <HealthGroup title="Runtime">
          <HealthItem tone={status.db.ok ? "live" : "error"} name="Database" state={status.db.ok ? "ok" : "unavailable"} />
          <HealthItem
            tone={status.bash_sandbox.enabled ? "live" : "warning"}
            name="Bash sandbox"
            state={status.bash_sandbox.enabled ? "isolated" : "not isolated"}
            detail={status.bash_sandbox.reason}
          />
          <HealthItem
            tone={status.accepting_inputs ? "live" : "warning"}
            name="Input"
            state={status.accepting_inputs ? "accepting" : "paused"}
          />
        </HealthGroup>
      </div>
    </div>
  );
}

function activeAgo(iso: string): string {
  const age = formatRelativeTime(iso);
  return age === "now" ? "Active just now" : `Active ${age} ago`;
}

function HealthGroup({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="health-group">
      <h3>{title}</h3>
      <ul>{children}</ul>
    </div>
  );
}

function HealthItem({ tone, name, state, detail }: { tone: StatusTone; name: string; state: string; detail?: string }) {
  return (
    <li className="health-item">
      <StatusDot tone={tone} />
      <span className="health-item-name">{name}</span>
      <span className={`health-item-state ${tone === "error" ? "danger" : tone === "warning" ? "warning" : ""}`}>{state}</span>
      {detail && (
        <span className="health-item-detail" title={detail}>
          {detail}
        </span>
      )}
    </li>
  );
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

type UsageSplit = "agent" | "kind";

/** Agents keep their sidebar order so each one keeps its color. */
function agentSeries(report: MetricsReport, agentOrder: string[], agentLabel: (id: string) => string): StackSeries[] {
  const seen = new Set<string>();
  for (const bucket of report.usage.series) for (const id of Object.keys(bucket.by_agent)) seen.add(id);
  const ordered = [...agentOrder.filter((id) => seen.has(id)), ...[...seen].filter((id) => !agentOrder.includes(id)).sort()];
  return ordered.map((id) => ({
    key: id,
    label: agentLabel(id),
    color: agentOrder.includes(id) ? seriesColor(agentOrder.indexOf(id)) : OTHER_COLOR,
  }));
}

function kindSeries(report: MetricsReport): StackSeries[] {
  const seen = new Set<string>();
  for (const bucket of report.usage.series) for (const kind of Object.keys(bucket.by_kind)) seen.add(kind);
  const known = KIND_ORDER.filter((kind) => seen.has(kind));
  const unknown = [...seen].filter((kind) => !KIND_ORDER.includes(kind)).sort();
  return [...known, ...unknown].map((kind) => ({
    key: kind,
    label: kindLabel(kind),
    color: KIND_ORDER.includes(kind) ? seriesColor(KIND_ORDER.indexOf(kind)) : OTHER_COLOR,
  }));
}

function UsageView({
  report,
  agentOrder,
  agentLabel,
}: {
  report: MetricsReport;
  agentOrder: string[];
  agentLabel: (id: string) => string;
}) {
  const [chosenSplit, setSplit] = useState<UsageSplit>("agent");
  // A single agent has nothing to split by agent.
  const split: UsageSplit = report.agent ? "kind" : chosenSplit;
  const { usage, range } = report;
  const totals = usage.totals;

  if (totals.tokens === 0 && usage.pulse_intentions.length === 0) {
    return (
      <EmptyState
        icon={CHART_ICON}
        title="No LLM usage yet"
        description={`Nothing was sent to a model in ${rangePhrase(range.key)}.`}
      />
    );
  }

  const series = split === "agent" ? agentSeries(report, agentOrder, agentLabel) : kindSeries(report);
  const buckets = usage.series.map((bucket) => ({
    start: bucket.start,
    values: split === "agent" ? bucket.by_agent : bucket.by_kind,
  }));
  const cacheRate = formatPercent(totals.cached_input_tokens, totals.cache_reported_input_tokens);

  return (
    <div className="metrics-stack">
      <dl className="metric-kpis">
        <Kpi
          label="Tokens"
          value={formatCount(totals.tokens)}
          hint={`${formatCount(totals.input_tokens)} in, ${formatCount(totals.output_tokens)} out`}
          change={formatChange(totals.tokens, usage.previous_tokens)}
        />
        <Kpi label="LLM calls" value={formatCount(totals.llm_calls)} hint="Sleep runs not included" />
        <Kpi
          label="Cache hit"
          value={cacheRate ?? "—"}
          hint={
            cacheRate === null
              ? "Providers did not report cached tokens"
              : `${formatCount(totals.cached_input_tokens)} input tokens from cache`
          }
        />
        <Kpi
          label="Per call"
          value={totals.llm_calls > 0 ? formatCount(totals.input_tokens / totals.llm_calls) : "—"}
          hint="Average input tokens"
        />
      </dl>

      <div className="metrics-card">
        <div className="metrics-card-head">
          <h3>Tokens per {range.bucket}</h3>
          {!report.agent && (
          <div className="segmented small" role="radiogroup" aria-label="Split by">
            {(["agent", "kind"] as const).map((value) => (
              <button
                key={value}
                type="button"
                role="radio"
                aria-checked={split === value}
                className={split === value ? "active" : ""}
                onClick={() => setSplit(value)}
              >
                {value === "agent" ? "By agent" : "By use"}
              </button>
            ))}
          </div>
          )}
        </div>
        <StackedBars
          buckets={buckets}
          series={series}
          bucket={range.bucket}
          timeZone={range.timezone}
          label={`Tokens per ${range.bucket}, split ${split === "agent" ? "by agent" : "by use"}`}
        />
        <ul className="chart-legend">
          {series.map((s) => (
            <li key={s.key}>
              <span className="legend-swatch" style={{ background: s.color }} />
              {s.label}
            </li>
          ))}
        </ul>
      </div>

      <div className="metrics-grid-2">
        <div className="metrics-card">
          <h3>By use</h3>
          <ShareList
            items={usage.by_kind.map((kind) => ({
              key: kind.kind,
              label: kindLabel(kind.kind),
              value: kind.tokens,
              color: KIND_ORDER.includes(kind.kind) ? seriesColor(KIND_ORDER.indexOf(kind.kind)) : OTHER_COLOR,
              note:
                kind.kind === "sleep"
                  ? `${kind.llm_calls} ${kind.llm_calls === 1 ? "run" : "runs"}`
                  : `${formatCount(kind.llm_calls)} calls`,
            }))}
            total={totals.tokens}
          />
        </div>
        <div className="metrics-card">
          <h3>By model</h3>
          <table className="metrics-table">
            <thead>
              <tr>
                <th scope="col">Model</th>
                <th scope="col" className="num">Calls</th>
                <th scope="col" className="num">Tokens</th>
                <th scope="col" className="num">Cache hit</th>
              </tr>
            </thead>
            <tbody>
              {usage.by_model.map((model) => (
                <tr key={`${model.provider}/${model.model}`}>
                  <td>
                    <span className="cell-main">{model.model}</span>
                    <span className="cell-sub">{model.provider}</span>
                  </td>
                  <td className="num">{formatCount(model.llm_calls)}</td>
                  <td className="num">{formatCount(model.input_tokens + model.output_tokens)}</td>
                  <td className="num">{formatPercent(model.cached_input_tokens, model.cache_reported_input_tokens) ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {usage.pulse_intentions.length > 0 && (
        <div className="metrics-card">
          <div className="metrics-card-head">
            <h3>Pulse intentions</h3>
            <span className="metrics-card-note">Tokens matched to each run by its time window</span>
          </div>
          <PulseIntentionsTable report={report} agentLabel={agentLabel} />
        </div>
      )}
    </div>
  );
}

function ShareList({
  items,
  total,
}: {
  items: { key: string; label: string; value: number; color: string; note: string }[];
  total: number;
}) {
  return (
    <ul className="share-list">
      {items.map((item) => {
        const share = total > 0 ? (item.value / total) * 100 : 0;
        return (
          <li key={item.key}>
            <div className="share-row">
              <span className="legend-swatch" style={{ background: item.color }} />
              <span className="share-label">{item.label}</span>
              <span className="share-value">{formatCount(item.value)}</span>
              <span className="share-pct">{formatPercent(item.value, total) ?? "—"}</span>
            </div>
            <div className="share-track" aria-hidden="true">
              <span style={{ width: `${share}%`, background: item.color }} />
            </div>
            <span className="share-note">{item.note}</span>
          </li>
        );
      })}
    </ul>
  );
}

function PulseIntentionsTable({ report, agentLabel }: { report: MetricsReport; agentLabel: (id: string) => string }) {
  const { pulse_intentions: intentions, pulse_unattributed: unattributed } = report.usage;
  const pulseTotal = report.usage.by_kind.find((kind) => kind.kind === "pulse")?.tokens ?? 0;
  const unattributedTokens = unattributed.input_tokens + unattributed.output_tokens;
  return (
    <table className="metrics-table">
      <thead>
        <tr>
          <th scope="col">Intention</th>
          <th scope="col" className="num">Runs</th>
          <th scope="col" className="num">Per run</th>
          <th scope="col" className="num">Tokens</th>
          <th scope="col" className="num">Share of Pulse</th>
        </tr>
      </thead>
      <tbody>
        {intentions.map((row) => {
          const tokens = row.input_tokens + row.output_tokens;
          return (
            <tr key={`${row.agent_id}/${row.intention_id}`}>
              <td>
                <span className="cell-main">{row.intention_id}</span>
                <span className="cell-sub">{agentLabel(row.agent_id)}</span>
              </td>
              <td className="num">
                {row.runs}
                {row.failed_runs > 0 && <span className="cell-flag danger"> {row.failed_runs} failed</span>}
              </td>
              <td className="num">{row.runs > 0 ? formatCount(tokens / row.runs) : "—"}</td>
              <td className="num">{formatCount(tokens)}</td>
              <td className="num">{formatPercent(tokens, pulseTotal) ?? "—"}</td>
            </tr>
          );
        })}
        {unattributedTokens > 0 && (
          <tr className="row-muted">
            <td>
              <span className="cell-main">Unattributed</span>
              <span className="cell-sub">Outside any run window</span>
            </td>
            <td className="num">—</td>
            <td className="num">—</td>
            <td className="num">{formatCount(unattributedTokens)}</td>
            <td className="num">{formatPercent(unattributedTokens, pulseTotal) ?? "—"}</td>
          </tr>
        )}
      </tbody>
    </table>
  );
}

// ---------------------------------------------------------------------------
// Reliability
// ---------------------------------------------------------------------------

const SOURCE_LABELS: Record<FailureEntry["source"], string> = {
  turn: "Turn",
  pulse: "Pulse",
  sleep: "Sleep",
};

function ReliabilityView({
  report,
  agentLabel,
  onOpenSession,
  onOpenRun,
}: {
  report: MetricsReport;
  agentLabel: (id: string) => string;
  onOpenSession: (agentId: string, sessionKey: string) => void;
  onOpenRun: (agentId: string, kind: "pulse" | "sleep", runId: string) => void;
}) {
  const { turns, tools, runs, recent_failures: failures } = report.reliability;
  const finished = turns.completed + turns.failed + turns.cancelled + turns.uncertain;
  const turnFailures = turns.failed + turns.uncertain;
  const toolCalls = tools.reduce((sum, tool) => sum + tool.calls, 0);
  const toolFailures = tools.reduce((sum, tool) => sum + tool.failed, 0);
  const runFailures = runs.pulse.failed + runs.sleep.failed + runs.sleep.partial_failure;
  const failingTools = tools.filter((tool) => tool.failed > 0);

  return (
    <div className="metrics-stack">
      <dl className="metric-kpis">
        <Kpi
          label="Turns"
          value={formatCount(turns.total)}
          hint={turns.running > 0 ? `${turns.running} still running` : `${formatCount(turns.completed)} completed`}
          change={formatChange(turns.total, turns.previous_total)}
        />
        <Kpi
          label="Turn failures"
          value={formatPercent(turnFailures, finished) ?? "—"}
          hint={`${turnFailures} failed or uncertain`}
          tone={turnFailures > 0 ? "danger" : undefined}
        />
        <Kpi
          label="Turn duration"
          value={formatLatency(turns.duration_p50_ms)}
          hint={`Median; 95th percentile ${formatLatency(turns.duration_p95_ms)}`}
        />
        <Kpi
          label="Tool failures"
          value={formatPercent(toolFailures, toolCalls) ?? "—"}
          hint={`${formatCount(toolFailures)} of ${formatCount(toolCalls)} calls`}
          tone={toolFailures > 0 ? "warning" : undefined}
        />
        <Kpi
          label="Pulse and Sleep"
          value={runFailures === 0 ? "No failures" : `${runFailures} failed`}
          hint={`${runs.pulse.total} Pulse, ${runs.sleep.total} Sleep runs`}
          tone={runFailures > 0 ? "danger" : undefined}
        />
      </dl>

      <div className="metrics-grid-2">
        <div className="metrics-card">
          <h3>Turn failures by kind</h3>
          <CountList items={report.reliability.turn_failure_kinds} empty="No failed turns" />
        </div>
        <div className="metrics-card">
          <h3>Tool failures by kind</h3>
          <CountList items={report.reliability.tool_failure_kinds} empty="No failed tool calls" />
        </div>
      </div>

      {failingTools.length > 0 && (
        <div className="metrics-card">
          <h3>Tools with failures</h3>
          <table className="metrics-table">
            <thead>
              <tr>
                <th scope="col">Tool</th>
                <th scope="col" className="num">Calls</th>
                <th scope="col" className="num">Failed</th>
                <th scope="col" className="num">Rate</th>
              </tr>
            </thead>
            <tbody>
              {failingTools.slice(0, 10).map((tool) => (
                <tr key={tool.tool}>
                  <td>
                    <code>{tool.tool}</code>
                  </td>
                  <td className="num">{formatCount(tool.calls)}</td>
                  <td className="num">{formatCount(tool.failed)}</td>
                  <td className="num">{formatPercent(tool.failed, tool.calls)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="metrics-card">
        <h3>Recent failures</h3>
        {failures.length === 0 ? (
          <p className="metrics-empty-line">No failed turns or runs in {rangePhrase(report.range.key)}.</p>
        ) : (
          <ul className="failure-list">
            {failures.map((failure, index) => {
              const open =
                failure.source === "turn"
                  ? failure.session_key
                    ? () => onOpenSession(failure.agent_id, failure.session_key!)
                    : undefined
                  : failure.run_id
                    ? () => onOpenRun(failure.agent_id, failure.source as "pulse" | "sleep", failure.run_id!)
                    : undefined;
              return (
                <li key={`${failure.source}-${failure.at}-${index}`}>
                  <button type="button" className="failure-row" onClick={open} disabled={!open}>
                    <time dateTime={failure.at} title={new Date(failure.at).toLocaleString()}>
                      {formatRelativeTime(failure.at)}
                    </time>
                    <span className={`failure-source source-${failure.source}`}>{SOURCE_LABELS[failure.source]}</span>
                    <span className="failure-text">
                      <span className="failure-title">
                        {failure.source === "sleep" ? statusLabel(failure.kind) : failure.kind}
                        <span className="failure-agent">{agentLabel(failure.agent_id)}</span>
                      </span>
                      {failure.summary && <span className="failure-summary">{failure.summary}</span>}
                    </span>
                    {open && <span className="failure-open">{failure.source === "turn" ? "Open chat" : "Open run"}</span>}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

function CountList({ items, empty }: { items: { kind: string; count: number }[]; empty: string }) {
  if (items.length === 0) return <p className="metrics-empty-line">{empty}</p>;
  const max = Math.max(...items.map((item) => item.count));
  return (
    <ul className="count-list">
      {items.map((item) => (
        <li key={item.kind}>
          <code>{item.kind}</code>
          <span className="count-track" aria-hidden="true">
            <span style={{ width: `${(item.count / max) * 100}%` }} />
          </span>
          <span className="count-value">{formatCount(item.count)}</span>
        </li>
      ))}
    </ul>
  );
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

function AgentsTable({
  rows,
  highlighted,
  agentOrder,
  onOpenAgent,
}: {
  rows: AgentMetrics[];
  highlighted: string | null;
  agentOrder: string[];
  onOpenAgent: (agentId: string) => void;
}) {
  const totalTokens = rows.reduce((sum, row) => sum + row.tokens, 0);
  return (
    <div className="metrics-card metrics-card-flush">
      <table className="metrics-table agents-table">
        <thead>
          <tr>
            <th scope="col">Agent</th>
            <th scope="col" className="num">Turns</th>
            <th scope="col">Tokens</th>
            <th scope="col" className="num">Failures</th>
            <th scope="col">Pulse</th>
            <th scope="col">Last sleep</th>
            <th scope="col">Last active</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const share = totalTokens > 0 ? (row.tokens / totalTokens) * 100 : 0;
            const index = agentOrder.indexOf(row.agent_id);
            return (
              <tr
                key={row.agent_id}
                className={`row-link ${highlighted === row.agent_id ? "highlighted" : ""}`}
                onClick={() => onOpenAgent(row.agent_id)}
              >
                <th scope="row">
                  <button
                    type="button"
                    className="row-link-button"
                    onClick={(event) => {
                      // The row handles the click; the button is the keyboard target.
                      event.stopPropagation();
                      onOpenAgent(row.agent_id);
                    }}
                  >
                    <span className="legend-swatch" style={{ background: index >= 0 ? seriesColor(index) : OTHER_COLOR }} />
                    {row.label}
                  </button>
                </th>
                <td className="num">{formatCount(row.turns)}</td>
                <td>
                  <span className="agent-share">
                    <span className="agent-share-value">{formatCount(row.tokens)}</span>
                    <span className="share-track" aria-hidden="true">
                      <span style={{ width: `${share}%` }} />
                    </span>
                  </span>
                </td>
                <td className={`num ${row.failed_turns > 0 ? "danger" : ""}`}>{row.failed_turns}</td>
                <td>
                  {row.pulse_runs === 0 ? (
                    <span className="muted">—</span>
                  ) : (
                    <>
                      {row.pulse_runs} {row.pulse_runs === 1 ? "run" : "runs"}
                      {row.pulse_failed > 0 && <span className="cell-flag danger"> {row.pulse_failed} failed</span>}
                    </>
                  )}
                </td>
                <td>
                  {row.last_sleep_status ? (
                    <span className="inline-status">
                      <StatusDot tone={statusTone(row.last_sleep_status)} />
                      {statusLabel(row.last_sleep_status)}
                    </span>
                  ) : (
                    <span className="muted">Never</span>
                  )}
                </td>
                <td>
                  {row.last_active_at ? (
                    <time dateTime={row.last_active_at} title={new Date(row.last_active_at).toLocaleString()}>
                      {formatRelativeTime(row.last_active_at)}
                    </time>
                  ) : (
                    <span className="muted">Never</span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
