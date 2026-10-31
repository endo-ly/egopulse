import type { ScheduleRun } from "../../app/router";
import type { AgentPulse, PulseRun, SleepRun } from "../../shared/api/types";
import { dayKey } from "./scheduleFormat";

/** One run on the timeline, of either mechanism. */
export type TimelineRun = { kind: "pulse"; run: PulseRun } | { kind: "sleep"; run: SleepRun };

/** A source's newest runs, fetched up to `limit`. */
interface RunPage<T> {
  runs: T[];
  limit: number;
}

function startedAt(item: TimelineRun): number {
  return Date.parse(item.run.started_at);
}

/**
 * Merges each source's newest runs into one newest-first timeline. A source
 * that filled its page may have older runs not fetched yet, so the merged
 * list stops at the latest such page boundary; nothing older is shown until
 * more is fetched.
 */
export function mergeRunPages(
  pulse: RunPage<PulseRun>,
  sleep: RunPage<SleepRun>,
): { runs: TimelineRun[]; hasMore: boolean } {
  const items: TimelineRun[] = [
    ...pulse.runs.map((run): TimelineRun => ({ kind: "pulse", run })),
    ...sleep.runs.map((run): TimelineRun => ({ kind: "sleep", run })),
  ].sort((a, b) => startedAt(b) - startedAt(a));

  const boundaries = [pulse, sleep]
    .filter((page) => page.runs.length >= page.limit && page.runs.length > 0)
    .map((page) => Date.parse(page.runs[page.runs.length - 1].started_at));
  if (boundaries.length === 0) return { runs: items, hasMore: false };
  const cutoff = Math.max(...boundaries);
  return { runs: items.filter((item) => startedAt(item) >= cutoff), hasMore: true };
}

/**
 * A run that changed nothing and needs no attention: a Pulse firing with
 * nothing to report, or a sleep check that decided not to run. A skipped
 * Pulse firing is not quiet; it means a notification could not be delivered.
 */
export function isQuiet(item: TimelineRun): boolean {
  if (item.kind === "pulse") return item.run.status === "success" && item.run.output_kind === "silent";
  return item.run.status === "skipped";
}

export type TimelineEntry =
  | { kind: "run"; item: TimelineRun }
  /** Consecutive quiet runs shown as one row, newest first. */
  | { kind: "quiet"; items: TimelineRun[] };

interface TimelineDay {
  key: number;
  /** Start of the day's newest run, for labelling the day. */
  at: string;
  entries: TimelineEntry[];
}

/**
 * Groups newest-first runs by local day. Unless `showQuiet`, two or more
 * consecutive quiet runs within a day become one `quiet` entry.
 */
export function groupTimeline(runs: readonly TimelineRun[], showQuiet: boolean): TimelineDay[] {
  const days: TimelineDay[] = [];
  for (const item of runs) {
    const key = dayKey(item.run.started_at);
    let day = days[days.length - 1];
    if (day?.key !== key) {
      day = { key, at: item.run.started_at, entries: [] };
      days.push(day);
    }
    const last = day.entries[day.entries.length - 1];
    if (showQuiet || !isQuiet(item)) {
      day.entries.push({ kind: "run", item });
    } else if (last?.kind === "quiet") {
      last.items.push(item);
    } else if (last?.kind === "run" && isQuiet(last.item)) {
      day.entries[day.entries.length - 1] = { kind: "quiet", items: [last.item, item] };
    } else {
      day.entries.push({ kind: "run", item });
    }
  }
  return days;
}

/** A Pulse intention or Sleep whose latest run went wrong. */
export interface Problem {
  /** The intention for a Pulse run; null for Sleep. */
  intentionId: string | null;
  run: ScheduleRun;
  status: string;
  startedAt: string;
}

/**
 * Routines whose latest run failed or could not deliver: each Pulse
 * intention's latest firing, and the latest sleep run that did work. A later
 * successful run clears the problem.
 */
export function latestProblems(pulse: AgentPulse | undefined, lastSleep: SleepRun | undefined): Problem[] {
  const problems: Problem[] = [];
  for (const intention of pulse?.intentions ?? []) {
    const latest = intention.recent_runs[0];
    if (latest && (latest.status === "failed" || latest.status === "skipped")) {
      problems.push({
        intentionId: intention.id,
        run: { kind: "pulse", id: latest.id },
        status: latest.status,
        startedAt: latest.started_at,
      });
    }
  }
  if (lastSleep && (lastSleep.status === "failed" || lastSleep.status === "partial_failure")) {
    problems.push({
      intentionId: null,
      run: { kind: "sleep", id: lastSleep.id },
      status: lastSleep.status,
      startedAt: lastSleep.started_at,
    });
  }
  return problems.sort((a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt));
}
