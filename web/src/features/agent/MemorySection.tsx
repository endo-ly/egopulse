import { useMemo, useState } from "react";

import { fetchAgentMemory, fetchMemoryHistory } from "../../shared/api/agents";
import { MEMORY_LAYERS, type MemoryChange } from "../../shared/api/types";
import { countLineChanges } from "../../shared/lib/diff";
import { formatRelativeTime } from "../../shared/lib/format";
import { DiffViewer } from "../../shared/ui/DiffViewer";
import { MOON_ICON } from "../../shared/ui/icons";
import { EmptyState } from "../../shared/ui/EmptyState";
import { useAgentQuery } from "./agentQuery";
import { DocumentCard } from "./DocumentCard";
import { QueryState } from "../../shared/ui/QueryState";

export interface MemorySectionProps {
  agentId: string;
  authToken: string;
  onOpenSleepRun: (runId: string) => void;
}

/** Long-term memory: the published layers, then how Sleep changed them. */
export function MemorySection({ agentId, authToken, onOpenSleepRun }: MemorySectionProps) {
  const memoryState = useAgentQuery(authToken, ["memory", agentId], () =>
    fetchAgentMemory(agentId, authToken),
  );
  const historyState = useAgentQuery(authToken, ["memory-history", agentId], () =>
    fetchMemoryHistory(agentId, authToken),
  );

  return (
    <div className="agent-page">
      <section className="agent-page-section" aria-labelledby="memory-files-title">
        <h2 id="memory-files-title" className="run-section-title">
          Files
        </h2>
        <QueryState state={memoryState}>
          {(memory) => (
            <div className="doc-card-stack">
              {MEMORY_LAYERS.map((layer) => {
                const updatedAt = historyState.data?.find((change) => change.file === layer)?.created_at;
                const chars = memory[layer].length;
                return (
                  <DocumentCard
                    key={layer}
                    eyebrow="memory"
                    path={`${layer}.md`}
                    status={[
                      chars > 0 ? `${chars.toLocaleString()} chars` : null,
                      updatedAt ? `updated ${formatRelativeTime(updatedAt)}` : null,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                    tone={chars > 0 ? "applied" : "inactive"}
                    content={memory[layer]}
                    collapsible
                  />
                );
              })}
            </div>
          )}
        </QueryState>
      </section>

      <section className="agent-page-section" aria-labelledby="memory-changes-title">
        <h2 id="memory-changes-title" className="run-section-title">
          Changes
        </h2>
        <QueryState state={historyState}>
          {(changes) =>
            changes.length === 0 ? (
              <EmptyState
                icon={MOON_ICON}
                title="No changes yet"
                description="Changes that sleep batches publish to memory are listed here."
              />
            ) : (
              <ol className="memory-changes">
                {changes.map((change, index) => (
                  <MemoryChangeItem
                    key={`${change.run_id}:${change.file}`}
                    change={change}
                    defaultOpen={index === 0}
                    onOpenSleepRun={onOpenSleepRun}
                  />
                ))}
              </ol>
            )
          }
        </QueryState>
      </section>
    </div>
  );
}

function MemoryChangeItem({
  change,
  defaultOpen,
  onOpenSleepRun,
}: {
  change: MemoryChange;
  defaultOpen: boolean;
  onOpenSleepRun: (runId: string) => void;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const counts = useMemo(
    () => countLineChanges(change.content_before, change.content_after),
    [change.content_before, change.content_after],
  );

  return (
    <li className="memory-change" data-open={open || undefined}>
      <div className="memory-change-row">
        <button
          type="button"
          className="memory-change-toggle"
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
        >
          <span className="memory-change-chevron" aria-hidden="true">
            ›
          </span>
          <span className="memory-change-file">{change.file}</span>
          <time dateTime={change.created_at}>{new Date(change.created_at).toLocaleString()}</time>
          <span className="tab-switch-count">
            <span className="count-add">+{counts.add}</span>{" "}
            <span className="count-remove">−{counts.remove}</span>
          </span>
        </button>
        <button
          type="button"
          className="doc-card-action"
          onClick={() => onOpenSleepRun(change.run_id)}
          title="Open the sleep run in Schedule"
        >
          Sleep run #{change.run_id.slice(0, 6)}
        </button>
      </div>
      {open && (
        <DiffViewer before={change.content_before} after={change.content_after} fileName={change.file} />
      )}
    </li>
  );
}
