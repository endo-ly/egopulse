import { useMemo, useState } from "react";

import { fetchSleepRunDetail } from "../../shared/api/sleep";
import type { MemorySnapshot, SleepRun, SleepRunStep } from "../../shared/api/types";
import { countLineChanges } from "../../shared/lib/diff";
import { formatDuration, formatTokens } from "../../shared/lib/format";
import { DiffViewer } from "../../shared/ui/DiffViewer";
import { QueryState } from "../../shared/ui/QueryState";
import { Spinner } from "../../shared/ui/Spinner";
import { MetaLine, OutcomeLabel } from "./parts";
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
      <header className="run-detail-head">
        <h2 className="run-detail-title">{sleepTitle(run.trigger)}</h2>
        <MetaLine
          items={[
            <OutcomeLabel key="status" tone={statusTone(run.status)}>
              {statusLabel(run.status)}
            </OutcomeLabel>,
            <span key="at">{formatRunTime(run.started_at)}</span>,
            run.finished_at && (
              <span key="took">took {formatDuration(Date.parse(run.finished_at) - Date.parse(run.started_at))}</span>
            ),
            run.total_tokens > 0 && <span key="tokens">{formatTokens(run.total_tokens)} tokens</span>,
            <span key="id" className="row-meta-id">
              #{run.id.slice(0, 6)}
            </span>,
          ]}
        />
      </header>
      {run.status === "skipped" ? (
        <p className="field-note">Did not run: {run.error_message ?? "no reason recorded"}.</p>
      ) : (
        <>
          {steps.length === 0 && run.error_message && <pre className="row-error">{run.error_message}</pre>}
          {PUBLISHED.has(run.status) && <MemoryChanges snapshots={snapshots} />}
          {steps.length > 0 && <Pipeline steps={steps} />}
        </>
      )}
    </>
  );
}

/** The pipeline in one line while every step succeeded; step by step otherwise. */
function Pipeline({ steps }: { steps: SleepRunStep[] }) {
  const failures = steps.filter((step) => step.error_message);
  const allDone = steps.every((step) => step.status === "success");
  if (allDone) {
    return (
      <p className="run-detail-line pipeline-done">
        <span className="pipeline-step-icon" aria-hidden="true">
          ✓
        </span>
        All {steps.length} steps done
      </p>
    );
  }
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
              {step.status === "success" && tokens > 0 && (
                <span className="pipeline-step-meta">{formatTokens(tokens)}</span>
              )}
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

function stepGlyph(status: string): string {
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
      <h3 className="run-detail-section-title">Memory changes</h3>
      {active === undefined ? (
        <p className="field-note">Memory was left unchanged.</p>
      ) : (
        <>
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
          <DiffViewer
            key={active.snapshot.file}
            before={active.snapshot.content_before}
            after={active.snapshot.content_after}
            fileName={active.snapshot.file}
          />
        </>
      )}
    </section>
  );
}
