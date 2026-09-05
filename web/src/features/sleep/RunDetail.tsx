import { useMemo, useState } from "react";

import { fetchSleepRunDetail } from "../../shared/api/sleep";
import { useServerState } from "../../shared/hooks/useServerState";
import { computeLineDiff } from "../../shared/lib/diff";
import { formatDuration, formatTokens } from "../../shared/lib/format";
import { Spinner } from "../../shared/ui/Spinner";
import { StatusDot } from "../../shared/ui/StatusDot";
import type { AgentEntry, MemorySnapshot, SleepRunStep } from "../../shared/api/types";
import { DiffViewer } from "./DiffViewer";
import { statusLabel, statusTone, stepLabel, triggerLabel } from "./sleepStatus";

type RunDetailProps = {
  runId: string;
  authToken: string;
  agents: AgentEntry[];
  onBack: () => void;
};

export function RunDetail({ runId, authToken, agents, onBack }: RunDetailProps) {
  const detailState = useServerState(
    `sleep-run:${runId}`,
    () => fetchSleepRunDetail(runId, authToken),
    { pollIntervalMs: 10_000 },
  );

  if (detailState.loading && detailState.data === undefined) {
    return (
      <div className="run-detail-loading">
        <Spinner />
      </div>
    );
  }

  if (detailState.error || detailState.data === undefined) {
    return (
      <div className="run-detail">
        <div className="run-error" role="alert">
          {detailState.error?.message ?? "Run not found"}
        </div>
      </div>
    );
  }

  const { run, snapshots, steps } = detailState.data;
  const agentLabel = agents.find((agent) => agent.id === run.agent_id)?.label ?? run.agent_id;
  const duration = run.finished_at
    ? formatDuration(new Date(run.finished_at).getTime() - new Date(run.started_at).getTime())
    : null;

  return (
    <div className="run-detail">
      <header className="run-detail-header">
        <button type="button" className="run-detail-back" onClick={onBack} aria-label="Back to run list">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
            <path d="M15 18 9 12l6-6" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
        <StatusDot tone={statusTone(run.status)} />
        <span className="run-detail-status">{statusLabel(run.status)}</span>
        <span className="run-detail-id">#{run.id.slice(0, 6)}</span>
      </header>

      <div className="run-summary">
        <section className="run-meta" aria-label="Run details">
          <MetaRow label="Agent" value={agentLabel} />
          <MetaRow label="Trigger" value={triggerLabel(run.trigger)} />
          <MetaRow label="Sessions" value={String(run.session_count)} />
          <MetaRow label="Started" value={formatTimestamp(run.started_at)} />
          {run.finished_at && duration && (
            <MetaRow label="Finished" value={`${formatTimestamp(run.finished_at)} (${duration})`} />
          )}
          <MetaRow label="Tokens" value={formatTokens(run.total_tokens)} />
        </section>

        {steps.length > 0 && <StepsSection steps={steps} />}
      </div>

      {/* Runs that failed before any step ran carry only the aggregate error. */}
      {steps.length === 0 && run.error_message && (
        <section className="run-steps">
          <div className="run-step-error">
            <span className="run-step-error-label">Error</span>
            <pre>{run.error_message}</pre>
          </div>
        </section>
      )}

      {run.status !== "skipped" && snapshots.length > 0 && (
        <MemoryChangesSection snapshots={snapshots} />
      )}
    </div>
  );
}

function MetaRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="run-meta-row">
      <span className="run-meta-label">{label}</span>
      <span className="run-meta-value">{value}</span>
    </div>
  );
}

function formatTimestamp(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function StepsSection({ steps }: { steps: SleepRunStep[] }) {
  return (
    <section className="run-steps">
      <h3 className="run-section-title">Steps</h3>
      <ul className="run-steps-list">
        {steps.map((step) => (
          <StepRow key={step.step} step={step} />
        ))}
      </ul>
    </section>
  );
}

function StepRow({ step }: { step: SleepRunStep }) {
  const tokens = step.input_tokens + step.output_tokens;
  return (
    <li className="run-step" data-status={step.status}>
      <span className="run-step-icon">
        {step.status === "running" || step.status === "pending" ? <Spinner size="sm" /> : stepGlyph(step.status)}
      </span>
      <span className="run-step-name">{stepLabel(step.step)}</span>
      {step.error_message && (
        <div className="run-step-error">
          <span className="run-step-error-label">Error</span>
          <pre>{step.error_message}</pre>
        </div>
      )}
      <span className="run-step-tokens">{tokens > 0 ? formatTokens(tokens) : ""}</span>
    </li>
  );
}

function stepGlyph(status: string): string {
  switch (status) {
    case "success":
      return "✓";
    case "failed":
      return "✗";
    case "skipped":
      return "–";
    default:
      return "";
  }
}

const MEMORY_FILES = ["episodic", "semantic", "prospective"] as const;

/** Counts changed lines per snapshot file so tabs can show "+N −N" summaries. */
function changeCounts(snapshots: MemorySnapshot[]): Map<string, { add: number; remove: number }> {
  const counts = new Map<string, { add: number; remove: number }>();
  for (const snapshot of snapshots) {
    if (snapshot.content_before === snapshot.content_after) continue;
    let add = 0;
    let remove = 0;
    for (const line of computeLineDiff(snapshot.content_before, snapshot.content_after)) {
      if (line.type === "add") add += 1;
      if (line.type === "remove") remove += 1;
    }
    counts.set(snapshot.file, { add, remove });
  }
  return counts;
}

function MemoryChangesSection({ snapshots }: { snapshots: MemorySnapshot[] }) {
  const byFile = useMemo(() => {
    const map = new Map<string, MemorySnapshot>();
    for (const snapshot of snapshots) map.set(snapshot.file, snapshot);
    return map;
  }, [snapshots]);
  const counts = useMemo(() => changeCounts(snapshots), [snapshots]);

  const firstChanged =
    MEMORY_FILES.find((file) => counts.has(file)) ??
    MEMORY_FILES.find((file) => byFile.has(file));

  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const activeFile = selectedFile ?? firstChanged;
  const snapshot = activeFile ? byFile.get(activeFile) : undefined;

  if (!snapshot) return null;

  return (
    <section className="run-memory">
      <h3 className="run-section-title">Memory changes</h3>
      <div className="tab-switch" role="tablist" aria-label="Memory files">
        {MEMORY_FILES.filter((file) => byFile.has(file)).map((file) => {
          const changed = counts.get(file);
          return (
            <button
              key={file}
              type="button"
              role="tab"
              aria-selected={file === activeFile}
              className="tab-switch-item"
              onClick={() => setSelectedFile(file)}
            >
              {file}
              {changed && (
                <span className="tab-switch-count">
                  <span className="count-add">+{changed.add}</span>{" "}
                  <span className="count-remove">−{changed.remove}</span>
                </span>
              )}
            </button>
          );
        })}
      </div>
      <DiffViewer
        key={snapshot.file}
        before={snapshot.content_before}
        after={snapshot.content_after}
        fileName={snapshot.file}
      />
    </section>
  );
}
