import { describe, expect, it } from "vitest";
import {
  DEFAULT_SESSION_KEY,
  resolveAutoSessionKey,
} from "../sessionSelection";
import type { SessionEntry } from "../../shared/api/types";

function session(
  key: string,
  agentId: string,
  overrides: Partial<SessionEntry> = {},
): SessionEntry {
  return {
    session_key: key,
    chat_id: Number(key.replace("chat:", "")),
    name: null,
    channel: "web",
    agent_id: agentId,
    last_message_preview: "",
    last_message_time: 0,
    archived: false,
    pinned: false,
    ...overrides,
  };
}

describe("resolveAutoSessionKey", () => {
  it("keeps_the_current_active_session", () => {
    // Arrange
    const sessions = [session("chat:1", "lyre"), session("chat:2", "lyre")];

    // Act
    const key = resolveAutoSessionKey(sessions, "lyre", "chat:2");

    // Assert
    expect(key).toBe("chat:2");
  });

  it("replaces_an_archived_session_with_the_first_active_session", () => {
    // Arrange
    const sessions = [
      session("chat:1", "lyre", { archived: true }),
      session("chat:2", "lyre"),
    ];

    // Act
    const key = resolveAutoSessionKey(sessions, "lyre", "chat:1");

    // Assert
    expect(key).toBe("chat:2");
  });

  it("falls_back_to_the_new_conversation_state_when_all_sessions_are_archived", () => {
    // Arrange
    const sessions = [session("chat:1", "lyre", { archived: true })];

    // Act
    const key = resolveAutoSessionKey(sessions, "lyre", "chat:1");

    // Assert
    expect(key).toBe(DEFAULT_SESSION_KEY);
  });

  it("falls_back_to_the_new_conversation_state_when_the_agent_has_no_sessions", () => {
    // Arrange
    const sessions = [session("chat:1", "ace")];

    // Act
    const key = resolveAutoSessionKey(sessions, "lyre", "chat:1");

    // Assert
    expect(key).toBe(DEFAULT_SESSION_KEY);
  });

  it("ignores_sessions_belonging_to_other_agents", () => {
    // Arrange
    const sessions = [session("chat:1", "ace"), session("chat:2", "lyre")];

    // Act
    const key = resolveAutoSessionKey(sessions, "lyre", DEFAULT_SESSION_KEY);

    // Assert
    expect(key).toBe("chat:2");
  });
});
