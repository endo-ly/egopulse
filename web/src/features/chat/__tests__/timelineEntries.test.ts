import { describe, expect, it } from "vitest";
import { buildTimelineEntries } from "../timelineEntries";
import type { ChatMessage, PulseRef } from "../../../shared/api/types";

function msg(id: string, pulse?: Omit<PulseRef, "intention_id">): ChatMessage {
  return {
    id,
    sender_id: "lyre",
    sender_kind: "assistant",
    content: id,
    timestamp: "2026-10-30T00:00:00.000Z",
    message_kind: "message",
    pulse: pulse ? { ...pulse, intention_id: "morning_review" } : null,
  };
}

describe("buildTimelineEntries", () => {
  it("folds_each_activation_trace_under_its_output", () => {
    // Arrange
    const messages = [
      msg("before"),
      msg("in-1", { run_id: "r1", role: "input" }),
      msg("step-1", { run_id: "r1", role: "step" }),
      msg("tool-1", { run_id: "r1", role: "step" }),
      msg("out-1", { run_id: "r1", role: "output" }),
      msg("between"),
      msg("in-2", { run_id: "r2", role: "input" }),
      msg("out-2", { run_id: "r2", role: "output" }),
    ];

    // Act
    const entries = buildTimelineEntries(messages);

    // Assert
    expect(entries.map((entry) => entry.message.id)).toEqual([
      "before",
      "out-1",
      "between",
      "out-2",
    ]);
    expect(entries[1].pulseTrace).toEqual({
      intentionId: "morning_review",
      entries: [messages[1], messages[2], messages[3]],
    });
    expect(entries[3].pulseTrace?.entries).toEqual([messages[6]]);
    expect(entries[0].pulseTrace).toBeUndefined();
  });

  it("keeps_trace_rows_whose_output_is_not_loaded", () => {
    // Arrange
    const messages = [
      msg("in-1", { run_id: "r1", role: "input" }),
      msg("step-1", { run_id: "r1", role: "step" }),
    ];

    // Act
    const entries = buildTimelineEntries(messages);

    // Assert
    expect(entries.map((entry) => entry.message.id)).toEqual(["in-1", "step-1"]);
    expect(entries.every((entry) => entry.pulseTrace === undefined)).toBe(true);
  });
});
