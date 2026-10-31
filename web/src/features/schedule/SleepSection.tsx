import type { SleepOutlook } from "../../shared/api/types";
import { QueryState } from "../../shared/ui/QueryState";
import { sleepWillRun } from "./outlook";
import { OutcomeLabel } from "./parts";
import { formatUpcomingTime } from "./scheduleFormat";

export interface SleepSectionProps {
  outlook: { data: SleepOutlook | undefined; loading: boolean; error: Error | null };
}

function nextCheck(outlook: SleepOutlook): string {
  if (!outlook.scheduler_enabled) return "Scheduled sleep is off; it runs only when started by hand.";
  if (!outlook.targeted || outlook.next_check_at === null) return "This agent is not included in scheduled sleep.";
  return formatUpcomingTime(outlook.next_check_at);
}

/** Sleep: when the scheduler next checks, and whether it would run. */
export function SleepSection({ outlook }: SleepSectionProps) {
  return (
    <QueryState state={outlook}>
      {(data) => {
        const willRun = sleepWillRun(data);
        return (
          <div className="agent-page" data-lane="sleep">
            <dl className="agent-kv">
              <div className="agent-kv-row">
                <dt>Next check</dt>
                <dd>{nextCheck(data)}</dd>
              </div>
              <div className="agent-kv-row">
                <dt>Decision</dt>
                <dd>
                  <OutcomeLabel tone={willRun ? "success" : "idle"}>
                    {willRun ? "Expected to run" : "Likely to skip"}
                  </OutcomeLabel>
                  <div
                    className="meter"
                    role="meter"
                    aria-label="New messages"
                    aria-valuemin={0}
                    aria-valuemax={data.threshold + 1}
                    aria-valuenow={Math.min(data.pending_messages, data.threshold + 1)}
                  >
                    <span
                      data-enough={willRun || undefined}
                      style={{ width: `${Math.min(data.pending_messages / (data.threshold + 1), 1) * 100}%` }}
                    />
                  </div>
                  <p className="field-note">
                    {data.pending_messages} new messages so far; it runs when there are more than {data.threshold}.
                  </p>
                </dd>
              </div>
            </dl>
          </div>
        );
      }}
    </QueryState>
  );
}
