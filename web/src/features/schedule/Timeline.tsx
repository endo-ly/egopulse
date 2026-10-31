import { useState } from "react";

import type { ScheduleRun } from "../../app/router";
import { fetchPulseRuns } from "../../shared/api/pulse";
import { fetchSleepRuns } from "../../shared/api/sleep";
import type { AgentPulse, PulseRun, RunPage, SessionEntry, SleepOutlook, SleepRun } from "../../shared/api/types";
import { Spinner } from "../../shared/ui/Spinner";
import { sleepWillRun, upcomingItems, type UpcomingItem } from "./outlook";
import { LaneGlyph, OutcomeLabel } from "./parts";
import { pulseTone, statusLabel, statusTone } from "./runStatus";
import {
  describeDelivery,
  describeSchedule,
  formatClock,
  formatDay,
  formatRunTime,
  markdownPreview,
  pulseOutcomeLabel,
  sleepTitle,
} from "./scheduleFormat";
import { useScheduleQuery } from "./scheduleQuery";
import { groupTimeline, isQuiet, mergeRunPages, type TimelineEntry, type TimelineRun } from "./timeline";

const PAGE_SIZE = 20;

export interface TimelineProps {
  agentId: string;
  authToken: string;
  pulse: AgentPulse | undefined;
  outlook: SleepOutlook | undefined;
  sessions: readonly SessionEntry[];
  openRun: ScheduleRun | null;
  onOpenRun: (run: ScheduleRun) => void;
  /** Opens an intention in the Pulse section, where it is set up. */
  onOpenIntention: (intentionId: string) => void;
  /** Opens the Sleep section, where the check is set up. */
  onOpenSleep: () => void;
}

/** What the agent will do next, then what it did, newest first and grouped by day. */
export function Timeline(props: TimelineProps) {
  return (
    <div className="timeline">
      <Upcoming {...props} />
      <div className="timeline-now" role="separator" aria-label="Now">
        <span>Now</span>
      </div>
      <History {...props} />
    </div>
  );
}

/** What comes next; past runs belong to the history. */
function Upcoming({ pulse, outlook, sessions, onOpenIntention, onOpenSleep }: TimelineProps) {
  if (pulse === undefined || outlook === undefined) return null;
  const items = upcomingItems(pulse, outlook);
  return (
    <section className="timeline-section" aria-label="Upcoming">
      <h2 className="timeline-heading">Upcoming</h2>
      {pulse.enabled === false && <p className="timeline-note">Pulse is off for this agent.</p>}
      {items.length === 0 ? (
        <p className="timeline-note">
          Nothing scheduled. Ask the agent in chat to check in on something at a set time.
        </p>
      ) : (
        <ul className="timeline-rows">
          {items.map((item) => (
            <li key={item.kind === "pulse" ? `pulse:${item.intention.id}` : "sleep"}>
              {/* A row is what will happen; it opens where that is set up. */}
              <button
                type="button"
                className="timeline-row"
                aria-label={
                  item.kind === "pulse"
                    ? `Open intention ${item.intention.id} in Pulse`
                    : "Open Sleep"
                }
                onClick={() =>
                  item.kind === "pulse" ? onOpenIntention(item.intention.id) : onOpenSleep()
                }
              >
                <UpcomingTime at={item.at} />
                <LaneGlyph kind={item.kind} />
                <UpcomingText item={item} pulse={pulse} sessions={sessions} />
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function UpcomingTime({ at }: { at: string | null }) {
  if (at === null) {
    return (
      <span className="timeline-time">
        <span className="timeline-clock">Due</span>
      </span>
    );
  }
  return (
    <time className="timeline-time" dateTime={at}>
      <span className="timeline-day">{formatDay(at)}</span>
      <span className="timeline-clock">{formatClock(at)}</span>
    </time>
  );
}

function UpcomingText({
  item,
  pulse,
  sessions,
}: {
  item: UpcomingItem;
  pulse: AgentPulse;
  sessions: readonly SessionEntry[];
}) {
  if (item.kind === "sleep") {
    const { pending_messages: pending, threshold } = item.outlook;
    return (
      <span className="timeline-text">
        <span className="timeline-title">Sleep</span>
        <span className="timeline-summary">
          <OutcomeLabel tone={sleepWillRun(item.outlook) ? "success" : "idle"}>
            {sleepWillRun(item.outlook) ? "Expected to run" : "Likely to skip"}
          </OutcomeLabel>
          {/* A check runs above the threshold, so threshold + 1 is what it needs. */}
          <span className="timeline-summary-text">
            {pending} of {threshold + 1} new messages
          </span>
        </span>
      </span>
    );
  }
  const delivery = describeDelivery(item.intention, pulse.default_delivery, sessions);
  return (
    <span className="timeline-text">
      <span className="timeline-title">{item.intention.id}</span>
      <span className="timeline-summary">
        {delivery.sessionKey === null && <OutcomeLabel tone="warning">Not deliverable</OutcomeLabel>}
        <span className="timeline-summary-text">
          {describeSchedule(item.intention.schedule)} → {delivery.label}
        </span>
      </span>
    </span>
  );
}

/** The runs fetched so far, per mechanism, once the user pages back. */
interface PagedRuns {
  pulse: RunPage<PulseRun>;
  sleep: RunPage<SleepRun>;
}

/**
 * The cursor to continue a listing from: its oldest run, named by both its
 * `started_at` and its id so the page boundary never splits runs that share a
 * `started_at`.
 */
function pageCursor(page: RunPage<{ id: string; started_at: string }>): string | null {
  const runs = page.runs;
  const last = runs[runs.length - 1];
  return last ? `${last.started_at}|${last.id}` : null;
}

function History({ agentId, authToken, openRun, onOpenRun }: TimelineProps) {
  const [showQuiet, setShowQuiet] = useState(false);
  const [expandedQuiet, setExpandedQuiet] = useState<ReadonlySet<string>>(new Set());
  // Null while the newest page is live. Paging back freezes the list: the page
  // the cursors came from stays as it was, so a poll cannot leave a hole
  // between the live head and the pages fetched below it.
  const [paged, setPaged] = useState<PagedRuns | null>(null);
  const [olderError, setOlderError] = useState<string | null>(null);
  const [loadingOlder, setLoadingOlder] = useState(false);

  const pulseRuns = useScheduleQuery(
    authToken,
    ["pulse-runs", agentId],
    () => fetchPulseRuns(agentId, authToken, { limit: PAGE_SIZE }),
    { poll: paged === null },
  );
  const sleepRuns = useScheduleQuery(
    authToken,
    ["sleep-runs", agentId],
    () => fetchSleepRuns(agentId, authToken, { limit: PAGE_SIZE }),
    { poll: paged === null },
  );

  const loadEarlier = async () => {
    if (loadingOlder || pulseRuns.data === undefined || sleepRuns.data === undefined) return;
    const from: PagedRuns = paged ?? { pulse: pulseRuns.data, sleep: sleepRuns.data };
    const pulseFrom = from.pulse.has_more ? pageCursor(from.pulse) : null;
    const sleepFrom = from.sleep.has_more ? pageCursor(from.sleep) : null;
    setLoadingOlder(true);
    setOlderError(null);
    try {
      const [nextPulse, nextSleep] = await Promise.all([
        pulseFrom === null
          ? null
          : fetchPulseRuns(agentId, authToken, { limit: PAGE_SIZE, before: pulseFrom }),
        sleepFrom === null
          ? null
          : fetchSleepRuns(agentId, authToken, { limit: PAGE_SIZE, before: sleepFrom }),
      ]);
      setPaged({
        pulse: {
          runs: [...from.pulse.runs, ...(nextPulse?.runs ?? [])],
          has_more: nextPulse?.has_more ?? false,
        },
        sleep: {
          runs: [...from.sleep.runs, ...(nextSleep?.runs ?? [])],
          has_more: nextSleep?.has_more ?? false,
        },
      });
    } catch (error) {
      setOlderError(error instanceof Error ? error.message : String(error));
    } finally {
      setLoadingOlder(false);
    }
  };

  const error = pulseRuns.error ?? sleepRuns.error;
  let body;
  if (pulseRuns.data === undefined || sleepRuns.data === undefined) {
    body = error ? (
      <p className="timeline-note timeline-note-error">{error.message}</p>
    ) : (
      <div className="timeline-loading">
        <Spinner size="sm" />
      </div>
    );
  } else {
    const { runs, hasMore } = mergeRunPages(paged?.pulse ?? pulseRuns.data, paged?.sleep ?? sleepRuns.data);
    const days = groupTimeline(runs, showQuiet);
    const expand = (key: string) => setExpandedQuiet((keys) => new Set(keys).add(key));
    body =
      days.length === 0 ? (
        <p className="timeline-note">Nothing has run yet.</p>
      ) : (
        <>
          {days.map((day) => (
            <div key={day.key} className="timeline-day-group">
              <h3 className="timeline-day-heading">{formatDay(day.at)}</h3>
              <ul className="timeline-rows">
                {day.entries.flatMap((entry) =>
                  entryRows(entry, expandedQuiet).map((row) =>
                    row.kind === "quiet" ? (
                      <li key={`quiet:${quietKey(row.items)}`}>
                        <QuietRow items={row.items} onExpand={() => expand(quietKey(row.items))} />
                      </li>
                    ) : (
                      <li key={row.item.run.id}>
                        <RunRow
                          item={row.item}
                          showTitle
                          open={openRun?.kind === row.item.kind && openRun.id === row.item.run.id}
                          onOpen={() => onOpenRun({ kind: row.item.kind, id: row.item.run.id })}
                        />
                      </li>
                    ),
                  ),
                )}
              </ul>
            </div>
          ))}
          {hasMore && (
            <button type="button" className="timeline-more" onClick={loadEarlier} disabled={loadingOlder}>
              {loadingOlder ? "Loading…" : "Load earlier"}
            </button>
          )}
          {olderError !== null && <p className="timeline-note timeline-note-error">{olderError}</p>}
        </>
      );
  }

  return (
    <section className="timeline-section" aria-label="History">
      <div className="timeline-heading-row">
        <h2 className="timeline-heading">History</h2>
        <label className="timeline-toggle">
          <input type="checkbox" checked={showQuiet} onChange={(event) => setShowQuiet(event.target.checked)} />
          Show quiet runs
        </label>
      </div>
      {body}
    </section>
  );
}

/**
 * Identifies a quiet group by its oldest run, which stays the same while
 * newer quiet runs join the group.
 */
function quietKey(items: readonly TimelineRun[]): string {
  return items[items.length - 1].run.id;
}

/** A quiet group expands in place into its runs once opened. */
function entryRows(entry: TimelineEntry, expanded: ReadonlySet<string>): TimelineEntry[] {
  if (entry.kind === "quiet" && expanded.has(quietKey(entry.items))) {
    return entry.items.map((item) => ({ kind: "run", item }));
  }
  return [entry];
}

function QuietRow({ items, onExpand }: { items: TimelineRun[]; onExpand: () => void }) {
  const counts = new Map<string, number>();
  for (const item of items) {
    const label = runOutcome(item);
    counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  const first = items[0].run.started_at;
  const last = items[items.length - 1].run.started_at;
  return (
    <button type="button" className="timeline-row timeline-row-quiet" onClick={onExpand}>
      <span className="timeline-time">
        <span className="timeline-clock">
          {formatClock(last)}–{formatClock(first)}
        </span>
      </span>
      <span className="lane-glyph lane-glyph-quiet" aria-hidden="true">
        ···
      </span>
      <span className="timeline-text">
        <span className="timeline-title">{items.length} quiet runs</span>
        <span className="timeline-summary">
          <span className="timeline-summary-text">
            {[...counts].map(([label, count]) => `${label} ×${count}`).join(" · ")}
          </span>
        </span>
      </span>
    </button>
  );
}

function runOutcome(item: TimelineRun): string {
  return item.kind === "pulse" ? pulseOutcomeLabel(item.run) : statusLabel(item.run.status);
}

function runTone(item: TimelineRun) {
  return item.kind === "pulse" ? pulseTone(item.run.status) : statusTone(item.run.status);
}

/** The run's title on the timeline: the intention it fired, or the kind of sleep. */
function runTitle(item: TimelineRun): string {
  return item.kind === "sleep" ? sleepTitle(item.run.trigger) : item.run.intention_id;
}

/** One line on what came of a run. */
function runSummary(item: TimelineRun): string | null {
  const { run } = item;
  if (run.status === "failed" || run.status === "skipped") return run.error_message;
  if (run.status === "running") return null;
  if (item.kind === "pulse") {
    return item.run.output_kind === "notify" ? markdownPreview(item.run.output_text) : "Nothing worth reporting";
  }
  const changed = item.run.changed_memory;
  return changed.length === 0 ? "Memory unchanged" : `Updated ${changed.join(", ")}`;
}

/** One run as a timeline row: time, mechanism, title, outcome, and a one-line summary. */
function RunRow({
  item,
  showTitle,
  open,
  onOpen,
}: {
  item: TimelineRun;
  showTitle: boolean;
  open: boolean;
  onOpen: () => void;
}) {
  const summary = runSummary(item);
  return (
    <button
      type="button"
      className="timeline-row"
      data-quiet={isQuiet(item) || undefined}
      aria-current={open || undefined}
      onClick={onOpen}
    >
      <time className="timeline-time" dateTime={item.run.started_at} title={formatRunTime(item.run.started_at)}>
        <span className="timeline-clock">{formatClock(item.run.started_at)}</span>
      </time>
      <LaneGlyph kind={item.kind} />
      <span className="timeline-text">
        {showTitle && <span className="timeline-title">{runTitle(item)}</span>}
        <span className="timeline-summary">
          <OutcomeLabel tone={runTone(item)}>{runOutcome(item)}</OutcomeLabel>
          {summary && <span className="timeline-summary-text">{summary}</span>}
        </span>
      </span>
    </button>
  );
}
