import { useState } from "react";

import type { SleepView } from "../../app/router";
import type { AgentEntry } from "../../shared/api/types";
import { Button } from "../../shared/ui/Button";
import { EmptyState } from "../../shared/ui/EmptyState";
import { MemoryView } from "./MemoryView";
import { RunDetail } from "./RunDetail";
import { RunList } from "./RunList";

export interface SleepBatchPanelProps {
  agents: AgentEntry[];
  agentId: string;
  authToken: string;
  view: SleepView;
  runId: string | null;
  onViewChange: (view: SleepView) => void;
  onSelectRun: (runId: string) => void;
  onBack: () => void;
  onRefresh: () => void;
}

const REFRESH_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
    <path d="M21 12a9 9 0 1 1-2.64-6.36" strokeLinecap="round" strokeLinejoin="round" />
    <path d="M21 3v6h-6" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

const MOON_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

export function SleepBatchPanel({
  agents,
  agentId,
  authToken,
  view,
  runId,
  onViewChange,
  onSelectRun,
  onBack,
  onRefresh,
}: SleepBatchPanelProps) {
  const [agentFilter, setAgentFilter] = useState("");

  return (
    <div className="sleep-panel">
      <header className="sleep-panel-header">
        <div className="tab-switch" role="tablist" aria-label="Sleep views">
          <button
            type="button"
            role="tab"
            aria-selected={view === "runs"}
            className="tab-switch-item"
            onClick={() => onViewChange("runs")}
          >
            Runs
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === "memory"}
            className="tab-switch-item"
            onClick={() => onViewChange("memory")}
          >
            Memory
          </button>
        </div>
        <Button variant="icon" onClick={onRefresh} aria-label="Refresh" title="Refresh">
          {REFRESH_ICON}
        </Button>
      </header>

      {view === "memory" ? (
        <MemoryView agentId={agentId} authToken={authToken} />
      ) : (
        <div className="sleep-runs-layout" data-mobile-view={runId ? "detail" : "list"}>
          <div className="sleep-runs-pane">
            <select
              className="sleep-agent-filter"
              value={agentFilter}
              onChange={(event) => setAgentFilter(event.target.value)}
              aria-label="Filter runs by agent"
            >
              <option value="">All agents</option>
              {agents.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.label}
                </option>
              ))}
            </select>
            <div className="sleep-runs-scroll">
              <RunList
                agents={agents}
                agentFilter={agentFilter}
                authToken={authToken}
                selectedRunId={runId}
                onSelectRun={onSelectRun}
              />
            </div>
          </div>
          <div className="sleep-detail-pane">
            {runId ? (
              <RunDetail
                runId={runId}
                authToken={authToken}
                agents={agents}
                onBack={onBack}
              />
            ) : (
              <EmptyState
                icon={MOON_ICON}
                title="No run selected"
                description="Select a run from the list to audit what the sleep batch changed."
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}
