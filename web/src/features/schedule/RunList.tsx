import { useEffect, useState } from "react";

import { fetchSleepRuns } from "../../shared/api/sleep";
import { useServerState } from "../../shared/hooks/useServerState";
import { formatRelativeTime } from "../../shared/lib/format";
import { EmptyState } from "../../shared/ui/EmptyState";
import { Spinner } from "../../shared/ui/Spinner";
import { StatusDot } from "../../shared/ui/StatusDot";
import type { SleepRun } from "../../shared/api/types";
import { MOON_ICON } from "../../shared/ui/icons";
import { statusTone, triggerLabel } from "./sleepStatus";

const PAGE_SIZE = 20;

type RunListProps = {
  agentId: string;
  authToken: string;
  selectedRunId: string | null;
  onSelectRun: (runId: string) => void;
};

export function RunList({ agentId, authToken, selectedRunId, onSelectRun }: RunListProps) {
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);

  useEffect(() => {
    setVisibleCount(PAGE_SIZE);
  }, [agentId]);

  const runsState = useServerState(
    // authToken is part of the key: data is auth-scoped, and a distinct key
    // per token keeps cached runs from leaking across auth identities.
    `sleep-runs:${agentId}:${visibleCount}:${authToken}`,
    () => fetchSleepRuns(agentId, visibleCount, authToken),
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
        icon={MOON_ICON}
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

function RunRow({
  run,
  selected,
  onSelect,
}: {
  run: SleepRun;
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
      <span className="run-row-title">{triggerLabel(run.trigger)}</span>
      <span className="run-row-meta">
        {run.session_count} {run.session_count === 1 ? "session" : "sessions"}
      </span>
      <span className="run-row-time">{formatRelativeTime(run.started_at)}</span>
    </button>
  );
}
