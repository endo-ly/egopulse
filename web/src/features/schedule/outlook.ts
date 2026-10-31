import { fetchAgentPulse } from "../../shared/api/pulse";
import { WORKED_SLEEP_STATUSES, fetchSleepOutlook, fetchSleepRuns } from "../../shared/api/sleep";
import type { AgentPulse, PulseIntention, SleepOutlook } from "../../shared/api/types";
import { useScheduleQuery } from "./scheduleQuery";

/**
 * What the agent is set to do and how it last went: the Pulse definition,
 * the sleep outlook, and the latest sleep run that did work.
 */
export function useScheduleOutlook(agentId: string, authToken: string) {
  const pulse = useScheduleQuery(authToken, ["pulse", agentId], () => fetchAgentPulse(agentId, authToken));
  const sleep = useScheduleQuery(authToken, ["sleep-outlook", agentId], () => fetchSleepOutlook(agentId, authToken));
  const lastSleep = useScheduleQuery(authToken, ["sleep-last", agentId], () =>
    fetchSleepRuns(agentId, authToken, { limit: 1, statuses: WORKED_SLEEP_STATUSES }),
  );
  return { pulse, sleep, lastSleep: { ...lastSleep, data: lastSleep.data?.runs[0] } };
}

/** Whether a sleep check would run on the messages so far. */
export function sleepWillRun(outlook: SleepOutlook): boolean {
  return outlook.pending_messages > outlook.threshold;
}

/** `at` is null when the intention is due now. */
export type UpcomingItem =
  | { kind: "pulse"; at: string | null; intention: PulseIntention }
  | { kind: "sleep"; at: string; outlook: SleepOutlook };

function time(item: UpcomingItem): number {
  return item.at === null ? -Infinity : Date.parse(item.at);
}

/** Soonest first, with due-now items ahead of every timed one. */
function compareUpcoming(a: UpcomingItem, b: UpcomingItem): number {
  const aAt = time(a);
  const bAt = time(b);
  if (aAt === bAt) return 0;
  return aAt < bAt ? -1 : 1;
}

/** The next firing of each active intention and the next sleep check, soonest first. */
export function upcomingItems(pulse: AgentPulse | undefined, outlook: SleepOutlook | undefined): UpcomingItem[] {
  const items: UpcomingItem[] = [];
  for (const intention of pulse?.intentions ?? []) {
    if (intention.next?.state === "due_now") {
      items.push({ kind: "pulse", at: null, intention });
    } else if (intention.next?.state === "scheduled" && intention.next.at) {
      items.push({ kind: "pulse", at: intention.next.at, intention });
    }
  }
  if (outlook?.next_check_at) {
    items.push({ kind: "sleep", at: outlook.next_check_at, outlook });
  }
  return items.sort(compareUpcoming);
}
