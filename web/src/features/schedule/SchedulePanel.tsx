import type { ScheduleView } from "../../app/router";
import type { SessionEntry } from "../../shared/api/types";
import { ActivitySection } from "./ActivitySection";
import { useScheduleOutlook } from "./outlook";
import { PulseSection } from "./PulseSection";
import { SleepSection } from "./SleepSection";

export interface SchedulePanelProps {
  agentId: string;
  authToken: string;
  view: ScheduleView;
  sessions: readonly SessionEntry[];
  onChangeView: (view: ScheduleView) => void;
  onOpenSession: (sessionKey: string) => void;
}

const HEADINGS = {
  pulse: { title: "Pulse", description: "Wakes the agent at set times to check on something." },
  sleep: { title: "Sleep", description: "Distills recent conversations into long-term memory." },
} as const;

/**
 * Schedule screen: what the agent does on its own. Activity watches what
 * comes next and what happened; Pulse and Sleep hold how each is set up.
 */
export function SchedulePanel({ agentId, authToken, view, sessions, onChangeView, onOpenSession }: SchedulePanelProps) {
  const { pulse, sleep, lastSleep } = useScheduleOutlook(agentId, authToken);

  if (view.section === "activity") {
    return (
      <ActivitySection
        agentId={agentId}
        authToken={authToken}
        pulse={pulse.data}
        outlook={sleep.data}
        lastSleep={lastSleep.data}
        sessions={sessions}
        run={view.run}
        onOpenRun={(run) => onChangeView({ section: "activity", run })}
        onOpenSession={onOpenSession}
        onOpenIntention={(intentionId) => onChangeView({ section: "pulse", intentionId })}
        onOpenSleep={() => onChangeView({ section: "sleep" })}
      />
    );
  }

  const heading = HEADINGS[view.section];
  return (
    <div className="screen-panel">
      <header className="screen-header">
        <h1 className="panel-title">{heading.title}</h1>
        <p className="panel-description">{heading.description}</p>
      </header>
      <div className="agent-panel-body">
        {view.section === "pulse" ? (
          <PulseSection
            pulse={pulse}
            sessions={sessions}
            intentionId={view.intentionId}
            onSelectIntention={(intentionId) => onChangeView({ section: "pulse", intentionId })}
            onOpenSession={onOpenSession}
          />
        ) : (
          <SleepSection outlook={sleep} />
        )}
      </div>
    </div>
  );
}
