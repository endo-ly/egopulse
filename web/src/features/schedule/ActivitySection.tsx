import type { ScheduleRun } from "../../app/router";
import type { AgentPulse, SessionEntry, SleepOutlook, SleepRun } from "../../shared/api/types";
import { StatusDot } from "../../shared/ui/StatusDot";
import { LaneGlyph } from "./parts";
import { PulseRunBody } from "./PulseRunBody";
import { pulseTone, statusTone } from "./runStatus";
import { formatRunTime } from "./scheduleFormat";
import { SleepRunBody } from "./SleepRunBody";
import { latestProblems, type Problem } from "./timeline";
import { Timeline } from "./Timeline";

export interface ActivitySectionProps {
  agentId: string;
  authToken: string;
  pulse: AgentPulse | undefined;
  outlook: SleepOutlook | undefined;
  lastSleep: SleepRun | undefined;
  sessions: readonly SessionEntry[];
  /** The run open beside the timeline. */
  run: ScheduleRun | null;
  onOpenRun: (run: ScheduleRun | null) => void;
  onOpenSession: (sessionKey: string) => void;
  /** Opens an intention in the Pulse section, where it is set up. */
  onOpenIntention: (intentionId: string) => void;
  /** Opens the Sleep section, where the check is set up. */
  onOpenSleep: () => void;
}

/** Activity: problems, what comes next, and what happened, with a run open beside them. */
export function ActivitySection({
  agentId,
  authToken,
  pulse,
  outlook,
  lastSleep,
  sessions,
  run,
  onOpenRun,
  onOpenSession,
  onOpenIntention,
  onOpenSleep,
}: ActivitySectionProps) {
  return (
    <div className="schedule-panel" data-detail={run !== null || undefined}>
      <div className="schedule-main">
        <div className="schedule-column">
          <header className="schedule-head">
            <h1 className="panel-title">Activity</h1>
            <p className="panel-description">What this agent does on its own: what comes next, and what happened.</p>
          </header>
          <Problems pulse={pulse} lastSleep={lastSleep} onOpenRun={onOpenRun} />
          <Timeline
            key={agentId}
            agentId={agentId}
            authToken={authToken}
            pulse={pulse}
            outlook={outlook}
            sessions={sessions}
            openRun={run}
            onOpenRun={onOpenRun}
            onOpenIntention={onOpenIntention}
            onOpenSleep={onOpenSleep}
          />
        </div>
      </div>
      {run !== null && (
        <RunDetail
          key={`${run.kind}:${run.id}`}
          run={run}
          authToken={authToken}
          sessions={sessions}
          onClose={() => onOpenRun(null)}
          onOpenSession={onOpenSession}
          onOpenIntention={onOpenIntention}
        />
      )}
    </div>
  );
}

function problemText(problem: Problem): string {
  if (problem.run.kind === "sleep") {
    return problem.status === "failed" ? "failed on its last run" : "partly failed on its last run";
  }
  return problem.status === "failed" ? "failed on its last firing" : "could not deliver its last notification";
}

/** Intentions and Sleep whose latest run went wrong, and a PULSE.yaml that does not parse. */
function Problems({
  pulse,
  lastSleep,
  onOpenRun,
}: {
  pulse: AgentPulse | undefined;
  lastSleep: SleepRun | undefined;
  onOpenRun: (run: ScheduleRun) => void;
}) {
  const problems = latestProblems(pulse, lastSleep);
  const parseError = pulse?.error ?? null;
  if (problems.length === 0 && parseError === null) return null;

  return (
    <div className="schedule-alert" role="alert">
      {parseError !== null && (
        <div className="schedule-alert-item">
          <p>PULSE.yaml could not be read, so no intention fires until it is fixed.</p>
          <pre className="row-error">{parseError}</pre>
        </div>
      )}
      {problems.length > 0 && (
        <ul className="schedule-alert-list">
          {problems.map((problem) => {
            return (
              <li key={`${problem.run.kind}:${problem.run.id}`} className="schedule-alert-item">
                <StatusDot tone={problem.run.kind === "pulse" ? pulseTone(problem.status) : statusTone(problem.status)} />
                <span>
                  <strong>{problem.intentionId ?? "Sleep"}</strong>{" "}
                  {problemText(problem)}
                  <span className="schedule-alert-time"> · {formatRunTime(problem.startedAt)}</span>
                </span>
                <button type="button" className="text-link" onClick={() => onOpenRun(problem.run)}>
                  View
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

function RunDetail({
  run,
  authToken,
  sessions,
  onClose,
  onOpenSession,
  onOpenIntention,
}: {
  run: ScheduleRun;
  authToken: string;
  sessions: readonly SessionEntry[];
  onClose: () => void;
  onOpenSession: (sessionKey: string) => void;
  onOpenIntention: (intentionId: string) => void;
}) {
  const label = run.kind === "pulse" ? "Pulse run" : "Sleep run";
  return (
    <aside className="run-detail" aria-label={label}>
      <div className="run-detail-bar">
        <span className="routine-kind" data-lane={run.kind}>
          <LaneGlyph kind={run.kind} />
          {label}
        </span>
        <button type="button" className="run-detail-close" aria-label="Close" onClick={onClose}>
          ×
        </button>
      </div>
      <div className="run-detail-body">
        {run.kind === "pulse" ? (
          <PulseRunBody
            runId={run.id}
            authToken={authToken}
            sessions={sessions}
            onOpenSession={onOpenSession}
            onOpenIntention={onOpenIntention}
          />
        ) : (
          <SleepRunBody runId={run.id} authToken={authToken} />
        )}
      </div>
    </aside>
  );
}
