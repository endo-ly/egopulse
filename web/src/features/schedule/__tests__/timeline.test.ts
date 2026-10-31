import { describe, it, expect } from "vitest";
import { groupTimeline, latestProblems, mergeRunPages, type TimelineRun } from "../timeline";
import type { AgentPulse, PulseIntention, PulseRun, SleepRun } from "../../../shared/api/types";

/** A local time on Oct 2026, as ISO. */
function at(day: number, hour: number): string {
  return new Date(2026, 9, day, hour, 0).toISOString();
}

function pulseRun(id: string, startedAt: string, overrides: Partial<PulseRun> = {}): PulseRun {
  return {
    id,
    agent_id: "lyre",
    intention_id: "morning",
    status: "success",
    started_at: startedAt,
    finished_at: null,
    output_kind: "notify",
    output_text: "Hi",
    error_message: null,
    ...overrides,
  };
}

function sleepRun(id: string, startedAt: string, overrides: Partial<SleepRun> = {}): SleepRun {
  return {
    id,
    agent_id: "lyre",
    status: "success",
    trigger: "scheduled",
    started_at: startedAt,
    finished_at: null,
    source_chats_json: "[]",
    source_digest_md: null,
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
    error_message: null,
    session_count: 0,
    changed_memory: [],
    ...overrides,
  };
}

const ids = (runs: readonly TimelineRun[]) => runs.map((item) => item.run.id);

describe("mergeRunPages", () => {
  it("interleaves_both_sources_newest_first_when_both_are_exhausted", () => {
    // Arrange
    const pulse = { runs: [pulseRun("p2", at(31, 9)), pulseRun("p1", at(29, 9))], has_more: false };
    const sleep = { runs: [sleepRun("s1", at(30, 3))], has_more: false };

    // Act
    const merged = mergeRunPages(pulse, sleep);

    // Assert
    expect(ids(merged.runs)).toEqual(["p2", "s1", "p1"]);
    expect(merged.hasMore).toBe(false);
  });

  it("stops_at_the_boundary_of_a_source_with_more_so_no_gap_is_shown", () => {
    // Arrange
    // Pulse has older runs past Oct 30 09:00, so the sleep run of Oct 29 must
    // wait until more is fetched instead of leaving a gap in the pulse lane.
    const pulse = { runs: [pulseRun("p2", at(31, 9)), pulseRun("p1", at(30, 9))], has_more: true };
    const sleep = { runs: [sleepRun("s2", at(30, 12)), sleepRun("s1", at(29, 3))], has_more: false };

    // Act
    const merged = mergeRunPages(pulse, sleep);

    // Assert
    expect(ids(merged.runs)).toEqual(["p2", "s2", "p1"]);
    expect(merged.hasMore).toBe(true);
  });
});

describe("groupTimeline", () => {
  const runs: TimelineRun[] = [
    { kind: "pulse", run: pulseRun("notify", at(31, 18)) },
    { kind: "pulse", run: pulseRun("silent-2", at(31, 12), { output_kind: "silent" }) },
    { kind: "sleep", run: sleepRun("skip", at(31, 3), { status: "skipped" }) },
    { kind: "pulse", run: pulseRun("undelivered", at(31, 2), { status: "skipped", output_kind: null }) },
    { kind: "pulse", run: pulseRun("silent-1", at(30, 9), { output_kind: "silent" }) },
  ];

  it("groups_by_day_and_collapses_consecutive_quiet_runs", () => {
    // Arrange / Act
    const days = groupTimeline(runs, false);

    // Assert
    expect(days).toHaveLength(2);
    expect(days[0].entries.map((entry) => (entry.kind === "quiet" ? ids(entry.items) : entry.item.run.id))).toEqual([
      "notify",
      ["silent-2", "skip"],
      "undelivered",
    ]);
    // A lone quiet run stays a run row.
    expect(days[1].entries).toEqual([{ kind: "run", item: runs[4] }]);
  });

  it("keeps_every_run_when_quiet_runs_are_shown", () => {
    // Arrange / Act
    const days = groupTimeline(runs, true);

    // Assert
    expect(days.flatMap((day) => day.entries).every((entry) => entry.kind === "run")).toBe(true);
    expect(days[0].entries).toHaveLength(4);
  });
});

describe("latestProblems", () => {
  function intention(id: string, recent: PulseIntention["recent_runs"]): PulseIntention {
    return {
      id,
      enabled: true,
      schedule: { kind: "daily", at: "09:00" },
      attention: "",
      delivery: null,
      target: null,
      next: null,
      recent_runs: recent,
    };
  }

  it("reports_intentions_and_sleep_whose_latest_run_failed_or_could_not_deliver", () => {
    // Arrange
    const pulse: AgentPulse = {
      enabled: true,
      error: null,
      timezone: "UTC",
      default_delivery: null,
      intentions: [
        intention("recovered", [
          { id: "ok", status: "success", output_kind: "notify", started_at: at(31, 9) },
          { id: "old-fail", status: "failed", output_kind: null, started_at: at(30, 9) },
        ]),
        intention("broken", [{ id: "fail", status: "failed", output_kind: null, started_at: at(31, 8) }]),
        intention("undelivered", [{ id: "skip", status: "skipped", output_kind: null, started_at: at(31, 7) }]),
      ],
    };
    const lastSleep = sleepRun("sleep-fail", at(31, 3), { status: "partial_failure" });

    // Act
    const problems = latestProblems(pulse, lastSleep);

    // Assert
    expect(problems.map((problem) => [problem.run.id, problem.status])).toEqual([
      ["fail", "failed"],
      ["skip", "skipped"],
      ["sleep-fail", "partial_failure"],
    ]);
    expect(problems.map((problem) => problem.intentionId)).toEqual(["broken", "undelivered", null]);
  });

  it("reports_nothing_when_every_latest_run_went_well", () => {
    // Arrange / Act / Assert
    expect(latestProblems(undefined, sleepRun("ok", at(31, 3)))).toEqual([]);
  });
});
