import { useState } from "react";

import { formatTokens } from "../api";
import { DiffViewer } from "./DiffViewer";

import type { SleepRun, MemorySnapshot } from "../types";

type RunDetailProps = {
  run: SleepRun;
  snapshots: MemorySnapshot[];
  onBack: () => void;
};

const STATUS_ICONS: Record<string, string> = {
  success: "\u2705",
  failed: "\u274C",
  skipped: "\u23ED",
  running: "\uD83D\uDD04",
};

function statusIcon(status: string): string {
  return STATUS_ICONS[status] ?? status;
}

export function RunDetail({ run, snapshots, onBack }: RunDetailProps) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  function toggleFile(file: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(file)) {
        next.delete(file);
      } else {
        next.add(file);
      }
      return next;
    });
  }

  return (
    <div className="run-detail">
      <button type="button" className="secondary-button" onClick={onBack}>
        ← Back
      </button>

      <div className="run-detail-meta">
        <div className="run-detail-row">
          <span className="run-detail-label">Status</span>
          <span>
            {statusIcon(run.status)} {run.status}
          </span>
        </div>
        <div className="run-detail-row">
          <span className="run-detail-label">Agent</span>
          <span>{run.agent_id}</span>
        </div>
        <div className="run-detail-row">
          <span className="run-detail-label">Trigger</span>
          <span>{run.trigger_type}</span>
        </div>
        <div className="run-detail-row">
          <span className="run-detail-label">Started</span>
          <span>{new Date(run.started_at).toLocaleString()}</span>
        </div>
        {run.finished_at && (
          <div className="run-detail-row">
            <span className="run-detail-label">Finished</span>
            <span>{new Date(run.finished_at).toLocaleString()}</span>
          </div>
        )}
        <div className="run-detail-row">
          <span className="run-detail-label">Tokens</span>
          <span>{formatTokens(run.total_tokens)}</span>
        </div>
      </div>

      {run.error_message && (
        <div className="run-error">{run.error_message}</div>
      )}

      <div className="run-snapshots">
        {snapshots.map((snapshot) => {
          const isOpen = expanded.has(snapshot.file);
          return (
            <div key={snapshot.file} className="diff-file-section">
              <button
                type="button"
                className="diff-file-header"
                onClick={() => toggleFile(snapshot.file)}
              >
                <span>{isOpen ? "▾" : "▸"}</span>
                <span>{snapshot.file}</span>
              </button>
              {isOpen && (
                <DiffViewer
                  before={snapshot.content_before}
                  after={snapshot.content_after}
                  fileName={snapshot.file}
                />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
