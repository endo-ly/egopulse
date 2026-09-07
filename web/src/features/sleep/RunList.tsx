import { useEffect, useState } from "react";

import { fetchSleepRuns } from "../../shared/api/sleep";
import { useServerState } from "../../shared/hooks/useServerState";
import { formatRelativeTime } from "../../shared/lib/format";
import { EmptyState } from "../../shared/ui/EmptyState";
import { Spinner } from "../../shared/ui/Spinner";
import { StatusDot } from "../../shared/ui/StatusDot";
import type { AgentEntry, SleepRun } from "../../shared/api/types";
import { statusTone, triggerLabel } from "./sleepStatus";

const PAGE_SIZE = 20;

type RunListProps = {
  agents: AgentEntry[];
  agentFilter: string;
  authToken: string;
  selectedRunId: string | null;
  onSelectRun: (runId: string) => void;
};

const EMPTY_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

export function RunList({ agents, agentFilter, authToken, selectedRunId, onSelectRun }: RunListProps) {
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);

  useEffect(() => {
    setVisibleCount(PAGE_SIZE);
  }, [agentFilter]);

  const runsState = useServerState(
    // authToken is part of the key: data is auth-scoped, and a distinct key
    // per token keeps cached runs from leaking across auth identities.
    `sleep-runs:${agentFilter}:${visibleCount}:${authToken}`,
    () => fetchSleepRuns(agentFilter || undefined, visibleCount, authToken),
    { pollIntervalMs: 10_000 },
  );

  if (runsState.loading && runsState.data === undefined) {
    return (
      <div className="run-list-loading">
        <Spinner />
      </div>
    );
  }

  const runs = runsState.data ?? [];
  if (runs.length === 0) {
    return (
      <EmptyState
        icon={EMPTY_ICON}
        title="No sleep batch runs yet"
        description="Runs are created automatically by the sleep scheduler, or when memory is distilled from conversations."
      />
    );
  }

  const hasMore = runs.length === visibleCount;

  return (
    <nav className="run-list" aria-label="Sleep runs">
      <div className="run-rows">
        {runs.map((run) => (
          <RunRow
            key={run.id}
            run={run}
            agentLabel={agentLabel(agents, run.agent_id)}
            selected={run.id === selectedRunId}
            onSelect={onSelectRun}
          />
        ))}
      </div>
      {hasMore && (
        <button
          type="button"
          className="run-load-more"
          onClick={() => setVisibleCount((count) => count + PAGE_SIZE)}
        >
          Load more
        </button>
      )}
    </nav>
  );
}

function agentLabel(agents: AgentEntry[], agentId: string): string {
  return agents.find((agent) => agent.id === agentId)?.label ?? agentId;
}

function RunRow({
  run,
  agentLabel,
  selected,
  onSelect,
}: {
  run: SleepRun;
  agentLabel: string;
  selected: boolean;
  onSelect: (runId: string) => void;
}) {
  return (
    <button
      type="button"
      className="run-row"
      data-selected={selected || undefined}
      onClick={() => onSelect(run.id)}
      title={run.status}
    >
      <StatusDot tone={statusTone(run.status)} />
      <span className="run-row-agent">{agentLabel}</span>
      <span className="run-row-trigger">{triggerLabel(run.trigger)}</span>
      <span className="run-row-time">{formatRelativeTime(run.started_at)}</span>
    </button>
  );
}
