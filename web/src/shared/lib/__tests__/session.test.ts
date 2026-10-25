import { describe, expect, it } from "vitest";
import { sessionTime } from "../session";
import type { SessionEntry } from "../../api/types";

function session(lastMessageTime: number): SessionEntry {
  return {
    session_key: "chat:1",
    chat_id: 1,
    name: null,
    channel: "web",
    agent_id: "lyre",
    last_message_preview: "",
    last_message_time: lastMessageTime,
    archived: false,
    pinned: false,
  };
}

describe("sessionTime", () => {
  it("formats_the_last_message_as_a_compact_age", () => {
    // Arrange
    const now = Date.UTC(2026, 6, 4, 12, 0, 0);

    // Act
    const time = sessionTime(session(now - 5 * 60_000), now);

    // Assert
    expect(time).toBe("5m");
  });

  it("omits_sessions_without_a_message_timestamp", () => {
    // Arrange
    const value = session(0);

    // Act
    const time = sessionTime(value, Date.UTC(2026, 6, 4, 12, 0, 0));

    // Assert
    expect(time).toBe("");
  });
});
