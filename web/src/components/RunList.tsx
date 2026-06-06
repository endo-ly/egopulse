import { formatTokens } from "../api";
import type { SleepRun } from "../types";

type RunListProps = {
  runs: SleepRun[];
  agents: string[];
  selectedAgent: string;
  onSelectAgent: (agent: string) => void;
  onSelectRun: (run: SleepRun) => void;
};

const STATUS_ICONS: Record<string, string> = {
  success: "\u2705",
  partial_failure: "\u26A0\uFE0F",
  failed: "\u274C",
  skipped: "\u23ED",
  running: "\uD83D\uDD04",
};

function statusIcon(status: string): string {
  return STATUS_ICONS[status] ?? status;
}

export function RunList({
  runs,
  agents,
  selectedAgent,
  onSelectAgent,
  onSelectRun,
}: RunListProps) {
  return (
    <div className="run-list">
      <div className="run-filter">
        <select
          value={selectedAgent}
          onChange={(e) => onSelectAgent(e.target.value)}
          className="run-agent-select"
        >
          <option value="">All agents</option>
          {agents.map((agent) => (
            <option key={agent} value={agent}>
              {agent}
            </option>
          ))}
        </select>
      </div>

      {runs.length === 0 ? (
        <p className="run-empty">No sleep batch runs yet</p>
      ) : (
        <div className="run-cards">
          {runs.map((run) => (
            <button
              key={run.id}
              type="button"
              className="run-card"
              onClick={() => onSelectRun(run)}
            >
              <span className="run-status-icon">{statusIcon(run.status)}</span>
              <div className="run-card-meta">
                <strong>{run.agent_id}</strong>
                <span className="run-card-date">
                  {new Date(run.started_at).toLocaleString()}
                </span>
                <span className="run-card-tokens">
                  {formatTokens(run.total_tokens)} tokens
                </span>
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
