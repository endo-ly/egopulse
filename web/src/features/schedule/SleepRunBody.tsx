import { useMemo, useState } from "react";

import { fetchSleepRunDetail } from "../../shared/api/sleep";
import type { MemorySnapshot, SleepRun, SleepRunStep, SleepStepStatus } from "../../shared/api/types";
import { countLineChanges } from "../../shared/lib/diff";
import { formatDuration, formatTokens } from "../../shared/lib/format";
import { DiffViewer } from "../../shared/ui/DiffViewer";
import { QueryState } from "../../shared/ui/QueryState";
import { Spinner } from "../../shared/ui/Spinner";
import { OutcomeLabel } from "./parts";
import { statusLabel, statusTone, stepLabel } from "./runStatus";
import { formatRunTime, sleepTitle } from "./scheduleFormat";
import { useScheduleQuery } from "./scheduleQuery";

export interface SleepRunBodyProps {
  runId: string;
  authToken: string;
}

/** Statuses whose memory changes were published; other runs' snapshots are only candidates. */
const PUBLISHED = new Set(["success", "partial_failure"]);

/** One sleep run: what it changed in memory, then how its pipeline went. */
export function SleepRunBody({ runId, authToken }: SleepRunBodyProps) {
  const state = useScheduleQuery(authToken, ["sleep-run", runId], () => fetchSleepRunDetail(runId, authToken));
  return (
    <QueryState state={state}>
      {(detail) => <SleepRunContent run={detail.run} snapshots={detail.snapshots} steps={detail.steps} />}
    </QueryState>
  );
}

function SleepRunContent({
  run,
  snapshots,
  steps,
}: {
  run: SleepRun;
  snapshots: MemorySnapshot[];
  steps: SleepRunStep[];
}) {
  return (
    <>
      <h2 className="run-detail-title">{sleepTitle(run.trigger)}</h2>
      {/* The run's facts and its steps read together, side by side; the memory
          diff needs the whole width below them. */}
      <div className="sleep-run-overview">
        <section className="sleep-run-summary" aria-label="Run summary">
          <OutcomeLabel tone={statusTone(run.status)}>{statusLabel(run.status)}</OutcomeLabel>
          <RunFacts run={run} />
          {run.status === "skipped" && (
            <p className="field-note">Did not run: {run.error_message ?? "no reason recorded"}.</p>
          )}
        </section>
        {steps.length > 0 && <Pipeline steps={steps} />}
      </div>
      {steps.length === 0 && run.status !== "skipped" && run.error_message && (
        <pre className="row-error">{run.error_message}</pre>
      )}
      {PUBLISHED.has(run.status) && <MemoryChanges snapshots={snapshots} />}
    </>
  );
}

/** The run as a few facts: what it cost, when it ran, and which run it was. */
function RunFacts({ run }: { run: SleepRun }) {
  const took = run.finished_at
    ? formatDuration(Date.parse(run.finished_at) - Date.parse(run.started_at))
    : null;
  return (
    <dl className="agent-kv sleep-run-facts">
      {run.total_tokens > 0 && (
        <div className="agent-kv-row">
          <dt>Tokens</dt>
          <dd>{formatTokens(run.total_tokens)}</dd>
        </div>
      )}
      {run.session_count > 0 && (
        <div className="agent-kv-row">
          <dt>Sessions</dt>
          <dd>{run.session_count}</dd>
        </div>
      )}
      <div className="agent-kv-row">
        <dt>Started</dt>
        <dd>{formatRunTime(run.started_at)}</dd>
      </div>
      {took !== null && (
        <div className="agent-kv-row">
          <dt>Took</dt>
          <dd>{took}</dd>
        </div>
      )}
      <div className="agent-kv-row">
        <dt>Run</dt>
        <dd className="row-meta-id" title={run.id}>
          #{run.id.slice(0, 6)}
        </dd>
      </div>
    </dl>
  );
}

/**
 * The pipeline step by step: how far it got, which steps cleared, and what
 * each one cost. Steps stay listed even when all of them succeeded, so the
 * run's shape and cost are readable at a glance.
 */
function Pipeline({ steps }: { steps: SleepRunStep[] }) {
  const failures = steps.filter((step) => step.error_message);
  return (
    <section className="run-detail-section" aria-label="Pipeline">
      <h3 className="run-detail-section-title">Pipeline</h3>
      <ol className="pipeline">
        {steps.map((step) => {
          const tokens = step.input_tokens + step.output_tokens;
          return (
            <li key={step.step} className="pipeline-step" data-status={step.status}>
              <span className="pipeline-step-icon" aria-hidden="true">
                {step.status === "running" ? <Spinner size="sm" /> : stepGlyph(step.status)}
              </span>
              <span className="pipeline-step-name">{stepLabel(step.step)}</span>
              {/* Always rendered so the token column stays aligned. */}
              <span className="pipeline-step-meta">{tokens > 0 ? formatTokens(tokens) : ""}</span>
            </li>
          );
        })}
      </ol>
      {failures.map((step) => (
        <div key={step.step} className="pipeline-failure">
          <span className="pipeline-failure-label">{stepLabel(step.step)} failed</span>
          <pre className="row-error">{step.error_message}</pre>
        </div>
      ))}
    </section>
  );
}

function stepGlyph(status: SleepStepStatus): string {
  switch (status) {
    case "success":
      return "✓";
    case "failed":
      return "✕";
    case "skipped":
      return "–";
    default:
      return "○";
  }
}

const MEMORY_FILES = ["episodic", "semantic", "prospective"] as const;

function MemoryChanges({ snapshots }: { snapshots: MemorySnapshot[] }) {
  const changed = useMemo(
    () =>
      MEMORY_FILES.flatMap((file) => {
        const snapshot = snapshots.find((candidate) => candidate.file === file);
        if (!snapshot || snapshot.content_before === snapshot.content_after) return [];
        return [{ snapshot, counts: countLineChanges(snapshot.content_before, snapshot.content_after) }];
      }),
    [snapshots],
  );
  const [selected, setSelected] = useState<string | null>(null);
  const active = changed.find((entry) => entry.snapshot.file === selected) ?? changed[0];

  return (
    <section className="run-detail-section" aria-label="Memory changes">
      <div className="run-detail-section-head">
        <h3 className="run-detail-section-title">Memory changes</h3>
        {active !== undefined && (
          <div className="file-chips" role="group" aria-label="Changed files">
            {changed.map(({ snapshot, counts }) => (
              <button
                key={snapshot.file}
                type="button"
                className="file-chip"
                aria-pressed={snapshot.file === active.snapshot.file}
                onClick={() => setSelected(snapshot.file)}
              >
                {snapshot.file}
                <span className="count-add">+{counts.add}</span>
                <span className="count-remove">−{counts.remove}</span>
              </button>
            ))}
          </div>
        )}
      </div>
      {active === undefined ? (
        <p className="field-note">Memory was left unchanged.</p>
      ) : (
        <DiffViewer
          key={active.snapshot.file}
          before={active.snapshot.content_before}
          after={active.snapshot.content_after}
          fileName={active.snapshot.file}
        />
      )}
    </section>
  );
}
