import type { AgentEntry } from "../../shared/api/types";
import { EmptyState } from "../../shared/ui/EmptyState";
import { MOON_ICON } from "../../shared/ui/icons";
import { RunDetail } from "./RunDetail";
import { RunList } from "./RunList";

export interface SchedulePanelProps {
  agents: AgentEntry[];
  agentId: string;
  authToken: string;
  runId: string | null;
  onSelectRun: (runId: string) => void;
  onBack: () => void;
}

/** Schedule screen: the selected agent's sleep run history and run audit. */
export function SchedulePanel({
  agents,
  agentId,
  authToken,
  runId,
  onSelectRun,
  onBack,
}: SchedulePanelProps) {
  return (
    <div className="screen-panel">
      <header className="screen-header">
        <h1 className="panel-title">Schedule</h1>
        <p className="panel-description">Sleep runs of this agent and what each one changed.</p>
      </header>

      <div className="sleep-runs-layout" data-mobile-view={runId ? "detail" : "list"}>
        <div className="sleep-runs-pane">
          <h2 className="section-title">SLEEP RUNS</h2>
          <div className="sleep-runs-scroll">
            <RunList
              agentId={agentId}
              authToken={authToken}
              selectedRunId={runId}
              onSelectRun={onSelectRun}
            />
          </div>
        </div>
        <div className="sleep-detail-pane">
          {runId ? (
            <RunDetail runId={runId} authToken={authToken} agents={agents} onBack={onBack} />
          ) : (
            <EmptyState
              icon={MOON_ICON}
              title="No run selected"
              description="Select a run from the list to audit what the sleep batch changed."
            />
          )}
        </div>
      </div>
    </div>
  );
}
