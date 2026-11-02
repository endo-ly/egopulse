import { describe, it, expect } from "vitest";
import {
  describeDelivery,
  describeNextDue,
  describeNextDueInList,
  describeSchedule,
  formatDay,
  formatUpcomingTime,
  markdownPreview,
  pulseOutcomeLabel,
} from "../scheduleFormat";
import { upcomingItems } from "../outlook";
import type { AgentPulse, PulseIntention, SessionEntry, SleepOutlook } from "../../../shared/api/types";

function intention(id: string, next: PulseIntention["next"]): PulseIntention {
  return {
    id,
    enabled: true,
    schedule: { kind: "daily", at: "09:00" },
    attention: "",
    delivery: null,
    target: null,
    next,
    recent_runs: [],
  };
}

function session(session_key: string, name: string | null, preview = ""): SessionEntry {
  return {
    session_key,
    chat_id: 1,
    name,
    channel: "discord",
    agent_id: "lyre",
    last_message_preview: preview,
    last_message_time: 0,
    archived: false,
    pinned: false,
  };
}

const OUTLOOK: SleepOutlook = {
  scheduler_enabled: true,
  targeted: true,
  next_check_at: "2026-10-31T03:00:00+00:00",
  pending_messages: 0,
  threshold: 64,
};

describe("scheduleFormat", () => {
  it("describes_each_schedule_kind_in_words", () => {
    // Arrange / Act / Assert
    expect(describeSchedule({ kind: "daily", at: "09:00" })).toBe("Every day 09:00");
    expect(describeSchedule({ kind: "weekly", day: "sun", at: "21:00" })).toBe("Every Sunday 21:00");
    expect(describeSchedule({ kind: "interval", interval_days: 3, at: "08:30" })).toBe("Every 3 days 08:30");
    expect(describeSchedule({ kind: "once", date: "2026-11-03", at: "10:00" })).toBe("Once on 2026-11-03 10:00");
  });

  it("names_upcoming_times_relative_to_today", () => {
    // Arrange
    const now = new Date(2026, 9, 31, 8, 0).getTime();

    // Act / Assert
    expect(formatUpcomingTime(new Date(2026, 9, 31, 21, 0).toISOString(), now)).toBe("Today 21:00");
    expect(formatUpcomingTime(new Date(2026, 10, 1, 9, 0).toISOString(), now)).toBe("Tomorrow 09:00");
    expect(formatUpcomingTime(new Date(2026, 10, 3, 10, 0).toISOString(), now)).toMatch(/Nov.*3.*10:00$/);
  });

  it("labels_days_relative_to_today", () => {
    // Arrange
    const now = new Date(2026, 9, 31, 8, 0).getTime();

    // Act / Assert
    expect(formatDay(new Date(2026, 9, 31, 23, 59).toISOString(), now)).toBe("Today");
    expect(formatDay(new Date(2026, 9, 30, 0, 0).toISOString(), now)).toBe("Yesterday");
    expect(formatDay(new Date(2026, 10, 1, 0, 0).toISOString(), now)).toBe("Tomorrow");
    expect(formatDay(new Date(2026, 9, 28, 9, 0).toISOString(), now)).toMatch(/Oct.*28/);
  });

  it("names_a_resolved_conversation_without_using_its_last_message", () => {
    // Arrange
    const sessions = [
      session("chat:7", "Team chat"),
      session("chat:8", null, "another intention's notification"),
    ];
    const fallback = { channel: "discord", external_chat_id: "123" };
    const own = { channel: "telegram", external_chat_id: "987" };

    // Act
    const named = describeDelivery(
      { delivery: null, target: { channel: "discord", session_key: "chat:7" } },
      fallback,
      sessions,
    );
    const unnamed = describeDelivery(
      { delivery: null, target: { channel: "discord", session_key: "chat:8" } },
      null,
      sessions,
    );
    const unnamedConfigured = describeDelivery(
      { delivery: null, target: { channel: "discord", session_key: "chat:8" } },
      fallback,
      sessions,
    );

    // Assert
    expect(named).toEqual({ label: "Discord · Team chat", origin: "default", sessionKey: "chat:7" });
    expect(unnamed).toEqual({ label: "Discord", origin: "latest", sessionKey: "chat:8" });
    expect(unnamedConfigured).toEqual({
      label: "Discord · 123",
      origin: "default",
      sessionKey: "chat:8",
    });
    expect(
      describeDelivery({ delivery: own, target: { channel: "telegram", session_key: "chat:99" } }, null, sessions),
    ).toEqual({
      label: "Telegram · 987",
      origin: "intention",
      sessionKey: "chat:99",
    });
    expect(describeDelivery({ delivery: own, target: null }, fallback, sessions)).toEqual({
      label: "Telegram · 987",
      origin: "intention",
      sessionKey: null,
    });
    expect(describeDelivery({ delivery: null, target: null }, null, sessions)).toEqual({
      label: "Latest conversation",
      origin: "latest",
      sessionKey: null,
    });
  });

  it("names_a_resolved_web_destination_with_the_web_label", () => {
    // Arrange
    const sessions = [session("chat:9", "Web room")];
    const delivery = { channel: "web", external_chat_id: "chat:9" };

    // Act
    const result = describeDelivery(
      { delivery, target: { channel: "web", session_key: "chat:9" } },
      null,
      sessions,
    );

    // Assert
    expect(result).toEqual({
      label: "Web · Web room",
      origin: "intention",
      sessionKey: "chat:9",
    });
  });

  it("says_a_skipped_firing_was_not_delivered", () => {
    // Arrange / Act / Assert
    expect(pulseOutcomeLabel({ status: "success", output_kind: "notify" })).toBe("Notified");
    expect(pulseOutcomeLabel({ status: "success", output_kind: "silent" })).toBe("Silent");
    expect(pulseOutcomeLabel({ status: "skipped", output_kind: null })).toBe("Not delivered");
    expect(pulseOutcomeLabel({ status: "failed", output_kind: null })).toBe("Failed");
  });

  it("explains_every_next_due_state", () => {
    // Arrange / Act / Assert
    expect(describeNextDue(null)).toBe("Off");
    expect(describeNextDue({ state: "due_now", at: null })).toBe("Due now");
    expect(describeNextDue({ state: "attempted_this_period", at: null })).toBe("Done for this period");
    expect(describeNextDue({ state: "attempted_once", at: null })).toBe("Already fired");
    expect(describeNextDue({ state: "no_upcoming", at: null })).toBe("No upcoming");
  });

  it("prefixes_only_a_scheduled_firing_with_next_in_a_row", () => {
    // Arrange
    const now = new Date(2026, 9, 31, 8, 0).getTime();

    // Act / Assert
    expect(describeNextDueInList(null, now)).toBe("off");
    expect(describeNextDueInList({ state: "due_now", at: null }, now)).toBe("due now");
    expect(describeNextDueInList({ state: "scheduled", at: new Date(2026, 9, 31, 21, 0).toISOString() }, now)).toBe(
      "next Today 21:00",
    );
    expect(describeNextDueInList({ state: "attempted_this_period", at: null }, now)).toBe("done for this period");
    expect(describeNextDueInList({ state: "attempted_once", at: null }, now)).toBe("already fired");
    expect(describeNextDueInList({ state: "no_upcoming", at: null }, now)).toBe("no upcoming");
  });

  it("previews_the_first_markdown_line_as_plain_text", () => {
    // Arrange / Act / Assert
    expect(markdownPreview("\n## **10:00** review with [ace](https://x)\nmore")).toBe("10:00 review with ace");
    expect(markdownPreview("- run `cargo test` first")).toBe("run cargo test first");
    expect(markdownPreview("   \n")).toBeNull();
    expect(markdownPreview(null)).toBeNull();
  });
});

describe("upcomingItems", () => {
  it("orders_due_now_then_firings_and_the_sleep_check_by_time", () => {
    // Arrange
    const pulse: AgentPulse = {
      enabled: true,
      error: null,
      timezone: "UTC",
      default_delivery: null,
      intentions: [
        intention("evening", { state: "scheduled", at: "2026-10-31T21:00:00+00:00" }),
        intention("overdue", { state: "due_now", at: null }),
        intention("also-overdue", { state: "due_now", at: null }),
        intention("done", { state: "attempted_this_period", at: null }),
        intention("paused", null),
      ],
    };

    // Act
    const items = upcomingItems(pulse, OUTLOOK);

    // Assert
    expect(items.map((item) => (item.kind === "pulse" ? item.intention.id : "sleep"))).toEqual([
      "overdue",
      "also-overdue",
      "sleep",
      "evening",
    ]);
  });
});
