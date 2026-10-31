import { afterEach, describe, it, expect, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/react";
import { SchedulePanel } from "../SchedulePanel";
import { ScheduleSectionNav } from "../ScheduleSectionNav";
import { SCHEDULE_ACTIVITY, type ScheduleView } from "../../../app/router";
import type {
  AgentPulse,
  PulseIntention,
  PulseRun,
  PulseRunDetail,
  SessionEntry,
  SleepOutlook,
  SleepRun,
} from "../../../shared/api/types";

/** An instant `hours` before now, as ISO. */
function hoursAgo(hours: number): string {
  return new Date(Date.now() - hours * 3_600_000).toISOString();
}

function pulseRun(id: string, intentionId: string, hours: number, overrides: Partial<PulseRun> = {}): PulseRun {
  return {
    id,
    agent_id: "lyre",
    intention_id: intentionId,
    status: "success",
    started_at: hoursAgo(hours),
    finished_at: null,
    output_kind: "notify",
    output_text: "**Two** meetings today.",
    error_message: null,
    ...overrides,
  };
}

function sleepRun(id: string, hours: number, overrides: Partial<SleepRun> = {}): SleepRun {
  return {
    id,
    agent_id: "lyre",
    status: "success",
    trigger: "scheduled",
    started_at: hoursAgo(hours),
    finished_at: null,
    source_chats_json: "[]",
    source_digest_md: null,
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
    error_message: null,
    session_count: 0,
    changed_memory: ["episodic", "semantic"],
    ...overrides,
  };
}

function intention(overrides: Partial<PulseIntention>): PulseIntention {
  return {
    id: "morning",
    enabled: true,
    schedule: { kind: "daily", at: "09:00" },
    attention: "Check the calendar.",
    delivery: null,
    target: { channel: "discord", session_key: "chat:7" },
    next: { state: "scheduled", at: new Date(Date.now() + 2 * 3_600_000).toISOString() },
    recent_runs: [],
    ...overrides,
  };
}

const PULSE: AgentPulse = {
  enabled: true,
  error: null,
  timezone: "Asia/Tokyo",
  default_delivery: { channel: "discord", external_chat_id: "123" },
  intentions: [
    intention({}),
    intention({
      id: "evening",
      attention: "Wrap up the day.",
      delivery: { channel: "telegram", external_chat_id: "987" },
      target: null,
      next: { state: "scheduled", at: new Date(Date.now() + 5 * 3_600_000).toISOString() },
      recent_runs: [{ id: "evening-fail", status: "failed", output_kind: null, started_at: hoursAgo(1) }],
    }),
  ],
};

const OUTLOOK: SleepOutlook = {
  scheduler_enabled: true,
  targeted: true,
  next_check_at: new Date(Date.now() + 3 * 3_600_000).toISOString(),
  pending_messages: 41,
  threshold: 64,
};

const SESSIONS: SessionEntry[] = [
  {
    session_key: "chat:7",
    chat_id: 7,
    name: "Team chat",
    channel: "discord",
    agent_id: "lyre",
    last_message_preview: "",
    last_message_time: 0,
    archived: false,
    pinned: false,
  },
];

interface Api {
  pulseRuns?: PulseRun[];
  sleepRuns?: SleepRun[];
  pulseDetails?: Record<string, PulseRunDetail>;
}

/** One page of runs, as the listings serve it. */
function pageOf<T extends { status: string; started_at: string }>(runs: T[], url: URL) {
  const statuses = url.searchParams.get("status")?.split(",") ?? null;
  const before = url.searchParams.get("before");
  const limit = Number(url.searchParams.get("limit") ?? "20");
  const matching = runs
    .filter((run) => statuses === null || statuses.includes(run.status))
    .filter((run) => before === null || run.started_at < before);
  return { runs: matching.slice(0, limit), has_more: matching.length > limit };
}

/** Serves the Schedule APIs from fixtures and records requested URLs. */
function stubApi({ pulseRuns = [], sleepRuns = [], pulseDetails = {} }: Api) {
  const requested: URL[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input), "http://localhost");
      requested.push(url);
      let body: unknown;
      if (url.pathname.endsWith("/pulse")) body = { ok: true, pulse: PULSE };
      else if (url.pathname.endsWith("/sleep/next")) body = { ok: true, outlook: OUTLOOK };
      else if (url.pathname === "/api/pulse/runs") body = { ok: true, ...pageOf(pulseRuns, url) };
      else if (url.pathname === "/api/sleep/runs") body = { ok: true, ...pageOf(sleepRuns, url) };
      else if (url.pathname.startsWith("/api/pulse/runs/")) {
        body = { ok: true, run: pulseDetails[url.pathname.split("/").pop()!] };
      }
      return Promise.resolve({ ok: true, json: () => Promise.resolve(body) });
    }),
  );
  return requested;
}

function renderPanel(authToken: string, view: ScheduleView = SCHEDULE_ACTIVITY) {
  const onChangeView = vi.fn();
  const onOpenSession = vi.fn();
  const utils = render(
    <SchedulePanel
      agentId="lyre"
      authToken={authToken}
      view={view}
      sessions={SESSIONS}
      onChangeView={onChangeView}
      onOpenSession={onOpenSession}
    />,
  );
  return { ...utils, onChangeView, onOpenSession };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("SchedulePanel", () => {
  it("lists_upcoming_firings_soonest_first_with_where_they_go", async () => {
    // Arrange
    stubApi({});

    // Act
    const { findByRole } = renderPanel("t-upcoming");

    // Assert
    const upcoming = await findByRole("region", { name: "Upcoming" });
    await waitFor(() => {
      expect(upcoming.querySelectorAll(".timeline-row")).toHaveLength(3);
    });
    const rows = Array.from(upcoming.querySelectorAll(".timeline-row"));
    // Intentions are named by id; the attention is read in the Pulse section.
    expect(rows.map((row) => row.querySelector(".timeline-title")!.textContent)).toEqual([
      "morning",
      "Sleep",
      "evening",
    ]);
    expect(rows[0].textContent).toContain("Every day 09:00 → Discord · Team chat");
    expect(rows[1].textContent).toContain("Likely to skip");
    expect(rows[1].textContent).toContain("41 of 65 new messages");
    expect(rows[2].textContent).toContain("Not deliverable");
  });

  it("merges_both_histories_by_day_and_collapses_quiet_runs_until_asked", async () => {
    // Arrange
    stubApi({
      pulseRuns: [
        pulseRun("notify", "morning", 0.2),
        pulseRun("silent", "morning", 0.4, { output_kind: "silent", output_text: "PULSE_OK" }),
      ],
      sleepRuns: [sleepRun("skip", 0.6, { status: "skipped", changed_memory: [], error_message: "only 3" })],
    });
    const { getByRole } = renderPanel("t-history");
    const history = getByRole("region", { name: "History" });
    await waitFor(() => {
      expect(history.querySelector(".timeline-row-quiet")).toBeTruthy();
    });

    // Act
    fireEvent.click(history.querySelector(".timeline-row-quiet")!);

    // Assert
    const rows = Array.from(history.querySelectorAll(".timeline-row"));
    expect(rows.map((row) => row.querySelector(".outcome")!.textContent)).toEqual([
      "Notified",
      "Silent",
      "Skipped",
    ]);
    expect(rows[0].textContent).toContain("Two meetings today.");
    expect(rows[2].textContent).toContain("only 3");
  });

  it("pages_back_through_the_history_without_gaps_or_duplicates", async () => {
    // Arrange: 45 runs, so the newest page of 20 leaves two older pages.
    const runs = Array.from({ length: 45 }, (_, index) =>
      pulseRun(`run-${String(index).padStart(2, "0")}`, "morning", index + 1),
    );
    stubApi({ pulseRuns: runs });
    const { getByRole, findByRole, queryByRole } = renderPanel("t-paging");
    const history = getByRole("region", { name: "History" });
    const stamps = () =>
      Array.from(history.querySelectorAll(".timeline-row time")).map((el) => el.getAttribute("datetime"));
    await waitFor(() => {
      expect(stamps()).toHaveLength(20);
    });

    // Act: page back twice, until the history runs out.
    fireEvent.click(await findByRole("button", { name: "Load earlier" }));
    await waitFor(() => {
      expect(stamps()).toHaveLength(40);
    });
    fireEvent.click(await findByRole("button", { name: "Load earlier" }));

    // Assert: every run shows up exactly once and the button goes away.
    await waitFor(() => {
      expect(stamps()).toHaveLength(45);
    });
    expect(new Set(stamps()).size).toBe(45);
    expect(queryByRole("button", { name: "Load earlier" })).toBeNull();
  });

  it("opens_a_run_beside_the_timeline_and_closes_it_again", async () => {
    // Arrange
    const run = pulseRun("notify", "morning", 0.2);
    const detail: PulseRunDetail = {
      ...run,
      attention_snapshot: "Check the calendar.",
      delivery: { channel: "discord", external_chat_id: "discord:1", session_key: "chat:7" },
    };
    stubApi({ pulseRuns: [run], pulseDetails: { notify: detail } });
    const view: ScheduleView = { section: "activity", run: { kind: "pulse", id: "notify" } };
    const { getByRole, onChangeView, onOpenSession } = renderPanel("t-detail", view);
    const pane = getByRole("complementary", { name: "Pulse run" });
    await waitFor(() => {
      expect(pane.querySelector(".message-card strong")?.textContent).toBe("Two");
    });

    // Act
    fireEvent.click(within(pane).getByRole("button", { name: "Open in Chat" }));
    fireEvent.click(within(pane).getByRole("button", { name: "Close" }));

    // Assert
    // The title is a way back to the intention, so it carries a glyph too.
    expect(pane.querySelector(".title-link")!.textContent).toContain("morning");
    expect(pane.textContent).toContain("Sent to Discord · Team chat");
    expect(onOpenSession).toHaveBeenCalledWith("chat:7");
    expect(onChangeView).toHaveBeenLastCalledWith({ section: "activity", run: null });
    const openRow = getByRole("region", { name: "History" }).querySelector("[aria-current]");
    expect(openRow).toBeTruthy();
  });

  it("flags_routines_whose_latest_run_went_wrong", async () => {
    // Arrange
    stubApi({ sleepRuns: [sleepRun("sleep-fail", 2, { status: "failed", changed_memory: [] })] });
    const { findByRole, onChangeView } = renderPanel("t-problems");
    const alert = await findByRole("alert");
    await waitFor(() => {
      expect(alert.querySelectorAll("li")).toHaveLength(2);
    });

    // Act
    fireEvent.click(within(alert.querySelectorAll("li")[0] as HTMLElement).getByRole("button", { name: "View" }));

    // Assert
    const items = Array.from(alert.querySelectorAll("li")).map((item) => item.textContent);
    expect(items[0]).toContain("evening failed on its last firing");
    expect(items[1]).toContain("Sleep failed on its last run");
    expect(onChangeView).toHaveBeenCalledWith({ section: "activity", run: { kind: "pulse", id: "evening-fail" } });
  });

  it("keeps_upcoming_to_what_comes_next", async () => {
    // Arrange
    stubApi({});

    // Act
    const { findByRole } = renderPanel("t-upcoming-static");

    // Assert
    const upcoming = await findByRole("region", { name: "Upcoming" });
    await waitFor(() => {
      expect(upcoming.querySelectorAll(".timeline-row")).toHaveLength(3);
    });
    // Upcoming is a plan, not history: its rows carry no run outcome.
    expect(upcoming.querySelector(".outcome")?.textContent).not.toBe("Notified");
  });

  it("opens_the_intention_or_the_sleep_setup_from_an_upcoming_row", async () => {
    // Arrange
    stubApi({});
    const { findByRole, onChangeView } = renderPanel("t-upcoming-nav");
    const upcoming = await findByRole("region", { name: "Upcoming" });
    await waitFor(() => {
      expect(upcoming.querySelectorAll(".timeline-row")).toHaveLength(3);
    });

    // Act
    fireEvent.click(within(upcoming).getByRole("button", { name: "Open intention morning in Pulse" }));
    fireEvent.click(within(upcoming).getByRole("button", { name: "Open Sleep" }));

    // Assert
    expect(onChangeView).toHaveBeenNthCalledWith(1, { section: "pulse", intentionId: "morning" });
    expect(onChangeView).toHaveBeenNthCalledWith(2, { section: "sleep" });
  });

  it("opens_the_intention_a_pulse_run_fired", async () => {
    // Arrange
    const run = pulseRun("notify", "morning", 0.2);
    const detail: PulseRunDetail = { ...run, attention_snapshot: "Check the calendar.", delivery: null };
    stubApi({ pulseRuns: [run], pulseDetails: { notify: detail } });
    const view: ScheduleView = { section: "activity", run: { kind: "pulse", id: "notify" } };
    const { getByRole, onChangeView } = renderPanel("t-intention-nav", view);
    const pane = getByRole("complementary", { name: "Pulse run" });
    await waitFor(() => {
      expect(pane.querySelector(".title-link")).toBeTruthy();
    });

    // Act
    fireEvent.click(within(pane).getByRole("button", { name: "Open intention" }));

    // Assert
    expect(onChangeView).toHaveBeenCalledWith({ section: "pulse", intentionId: "morning" });
  });

  it("shows_each_intention_definition_in_the_pulse_section", async () => {
    // Arrange
    stubApi({});
    const onChangeView = vi.fn();
    const { findByRole, container } = render(
      <SchedulePanel
        agentId="lyre"
        authToken="t-pulse"
        view={{ section: "pulse", intentionId: "evening" }}
        sessions={SESSIONS}
        onChangeView={onChangeView}
        onOpenSession={vi.fn()}
      />,
    );

    // Act
    const detail = await findByRole("region", { name: "evening" });
    fireEvent.click(container.querySelectorAll(".skill-row")[0]);

    // Assert
    const rows = Array.from(container.querySelectorAll(".skill-row"));
    expect(rows.map((row) => row.getAttribute("aria-current"))).toEqual([null, "true"]);
    expect(detail.textContent).toContain("Every day 09:00 (Asia/Tokyo)");
    expect(detail.querySelector(".run-detail-title")!.textContent).toBe("evening");
    expect(detail.textContent).toContain("Telegram · 987");
    expect(detail.querySelector(".field-warning")!.textContent).toContain("firings are skipped");
    expect(detail.querySelector(".doc-card")!.textContent).toContain("Wrap up the day.");
    expect(container.textContent).not.toContain("History");
    expect(onChangeView).toHaveBeenCalledWith({ section: "pulse", intentionId: "morning" });
  });

  it("shows_the_sleep_outlook_in_the_sleep_section", async () => {
    // Arrange
    stubApi({});

    // Act
    const { findByRole, container } = render(
      <SchedulePanel
        agentId="lyre"
        authToken="t-sleep"
        view={{ section: "sleep" }}
        sessions={SESSIONS}
        onChangeView={vi.fn()}
        onOpenSession={vi.fn()}
      />,
    );

    // Assert
    const meter = await findByRole("meter", { name: "New messages" });
    expect(meter.getAttribute("aria-valuenow")).toBe("41");
    expect(container.textContent).toContain("Likely to skip");
    expect(container.textContent).toContain("it runs when there are more than 64");
  });
});

describe("ScheduleSectionNav", () => {
  it("lists_the_sections_with_summaries_and_switches_between_them", async () => {
    // Arrange
    stubApi({});
    const onChangeView = vi.fn();
    const { container, findByText } = render(
      <ScheduleSectionNav agentId="lyre" authToken="t-nav" section="pulse" onChangeView={onChangeView} />,
    );
    await findByText("2 of 2 active");

    // Act
    fireEvent.click(container.querySelectorAll(".context-row")[2]);

    // Assert
    const rows = Array.from(container.querySelectorAll(".context-row"));
    expect(rows.map((row) => row.querySelector(".context-title")!.textContent)).toEqual(["Activity", "Pulse", "Sleep"]);
    expect(rows[0].querySelector(".context-sub")!.textContent).toBe("1 needs attention");
    expect(rows[1].getAttribute("aria-current")).toBe("page");
    expect(onChangeView).toHaveBeenCalledWith({ section: "sleep" });
  });
});
