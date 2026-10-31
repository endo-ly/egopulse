import type { ReactNode } from "react";

import type { ScheduleView } from "../../app/router";
import type { AgentPulse, SleepOutlook } from "../../shared/api/types";
import { LIST_ICON, MOON_ICON, PULSE_ICON } from "../../shared/ui/icons";
import { upcomingItems, useScheduleOutlook } from "./outlook";
import { formatUpcomingTime } from "./scheduleFormat";
import { latestProblems } from "./timeline";

export interface ScheduleSectionNavProps {
  agentId: string;
  authToken: string;
  section: ScheduleView["section"];
  onChangeView: (view: ScheduleView) => void;
}

interface SectionItem {
  view: ScheduleView;
  label: string;
  icon: ReactNode;
  lane?: "pulse" | "sleep";
}

const ITEMS: SectionItem[] = [
  { view: { section: "activity", run: null }, label: "Activity", icon: LIST_ICON },
  { view: { section: "pulse", intentionId: null }, label: "Pulse", icon: PULSE_ICON, lane: "pulse" },
  { view: { section: "sleep" }, label: "Sleep", icon: MOON_ICON, lane: "sleep" },
];

function activitySummary(pulse: AgentPulse, outlook: SleepOutlook, problems: number): string {
  if (problems > 0) return `${problems} need${problems === 1 ? "s" : ""} attention`;
  const next = upcomingItems(pulse, outlook)[0];
  if (next === undefined) return "Nothing scheduled";
  return next.at === null ? "Next: due now" : `Next: ${formatUpcomingTime(next.at)}`;
}

function pulseSummary(pulse: AgentPulse): string {
  if (pulse.error !== null) return "PULSE.yaml error";
  if (!pulse.enabled) return "Off";
  const active = pulse.intentions.filter((intention) => intention.enabled).length;
  return `${active} of ${pulse.intentions.length} active`;
}

function sleepSummary(outlook: SleepOutlook): string {
  if (!outlook.scheduler_enabled) return "Manual only";
  if (!outlook.targeted || outlook.next_check_at === null) return "Not scheduled";
  return formatUpcomingTime(outlook.next_check_at);
}

/** Sidebar context for the Schedule screen, laid out like the Agent sections. */
export function ScheduleSectionNav({ agentId, authToken, section, onChangeView }: ScheduleSectionNavProps) {
  const { pulse, sleep, lastSleep } = useScheduleOutlook(agentId, authToken);
  const summaries: Record<ScheduleView["section"], string | null> = {
    activity:
      pulse.data && sleep.data
        ? activitySummary(pulse.data, sleep.data, latestProblems(pulse.data, lastSleep.data).length)
        : null,
    pulse: pulse.data ? pulseSummary(pulse.data) : null,
    sleep: sleep.data ? sleepSummary(sleep.data) : null,
  };

  return (
    <nav className="agent-section-nav" aria-label="Schedule sections">
      <h2 className="section-title">SECTIONS</h2>
      <ul className="context-list">
        {ITEMS.map((item) => {
          const current = item.view.section === section;
          const summary = summaries[item.view.section];
          return (
            <li key={item.view.section}>
              <button
                type="button"
                className={`context-row ${current ? "active" : ""}`}
                aria-current={current ? "page" : undefined}
                onClick={() => onChangeView(item.view)}
              >
                <span className="context-icon" data-lane={item.lane}>
                  {item.icon}
                </span>
                <span className="context-text">
                  <span className="context-title">{item.label}</span>
                  {summary !== null && <span className="context-sub">{summary}</span>}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
