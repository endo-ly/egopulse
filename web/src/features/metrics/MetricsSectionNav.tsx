import type { ReactNode } from "react";

import { healthIssues } from "./health";
import { METRICS_SECTIONS, metricsSectionElementId, type MetricsFilters, type MetricsSectionId } from "./MetricsPanel";
import { formatCount } from "./metricsFormat";
import { useMetricsReport, useRuntimeStatus } from "./metricsQuery";

export interface MetricsSectionNavProps {
  authToken: string;
  filters: MetricsFilters;
}

const ICONS: Record<MetricsSectionId, ReactNode> = {
  health: <path d="M3 12h4l3 8 4-16 3 8h4" />,
  usage: <path d="M18 20V10M12 20V4M6 20v-6" />,
  reliability: <path d="M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />,
  agents: <path d="M16 21v-2a4 4 0 0 0-8 0v2M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z" />,
};

/** Sidebar context for Metrics: jumps to a section, each with its headline. */
export function MetricsSectionNav({ authToken, filters }: MetricsSectionNavProps) {
  const status = useRuntimeStatus(authToken);
  const report = useMetricsReport(authToken, filters.range, filters.agentId);

  const issues = status.data ? healthIssues(status.data) : null;
  const data = report.data;
  const failures = data
    ? data.reliability.turns.failed +
      data.reliability.turns.uncertain +
      data.reliability.runs.pulse.failed +
      data.reliability.runs.sleep.failed +
      data.reliability.runs.sleep.partial_failure
    : null;

  const summaries: Record<MetricsSectionId, { text: string; tone?: "danger" | "warning" } | null> = {
    health:
      issues === null
        ? null
        : issues.length === 0
          ? { text: "All normal" }
          : {
              text: `${issues.length} ${issues.length === 1 ? "issue" : "issues"}`,
              tone: issues.some((issue) => issue.severity === "danger") ? "danger" : "warning",
            },
    usage: data ? { text: `${formatCount(data.usage.totals.tokens)} tokens in ${data.range.key}` } : null,
    reliability:
      failures === null
        ? null
        : failures === 0
          ? { text: "No failures" }
          : { text: `${failures} ${failures === 1 ? "failure" : "failures"}`, tone: "danger" },
    agents: data ? { text: `${data.agents.length} ${data.agents.length === 1 ? "agent" : "agents"}` } : null,
  };

  const jump = (id: MetricsSectionId) => {
    document.getElementById(metricsSectionElementId(id))?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  return (
    <nav className="agent-section-nav" aria-label="Metrics sections">
      <h2 className="section-title">SECTIONS</h2>
      <ul className="context-list">
        {METRICS_SECTIONS.map((section) => {
          const summary = summaries[section.id];
          return (
            <li key={section.id}>
              <button type="button" className="context-row" onClick={() => jump(section.id)}>
                <span className="context-icon">
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    {ICONS[section.id]}
                  </svg>
                </span>
                <span className="context-text">
                  <span className="context-title">{section.label}</span>
                  {summary && (
                    <span className={`context-sub ${summary.tone ? `tone-${summary.tone}` : ""}`}>{summary.text}</span>
                  )}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
