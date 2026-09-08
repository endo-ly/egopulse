import { describe, it, expect, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useUnreadSessions } from "../useUnreadSessions";
import type { SessionEntry } from "../../api/types";

function session(overrides: Partial<SessionEntry>): SessionEntry {
  return {
    session_key: "s1",
    label: "Web Chat",
    channel: "web",
    agent_id: "lyre",
    last_message_preview: "hi",
    last_message_time: 100,
    ...overrides,
  };
}

describe("useUnreadSessions", () => {
  afterEach(() => {
    globalThis.localStorage?.clear();
  });

  it("starts_with_no_unread_on_first_sight", () => {
    // Arrange + Act
    const { result } = renderHook(() =>
      useUnreadSessions([session({}), session({ session_key: "s2" })], "s1"),
    );

    // Assert: first load initializes everything as read, no flood.
    expect(result.current.size).toBe(0);
  });

  it("marks_session_unread_when_messages_advance_while_unselected", () => {
    // Arrange
    const { result, rerender } = renderHook(
      ({ sessions, selected }: { sessions: SessionEntry[]; selected: string }) =>
        useUnreadSessions(sessions, selected),
      {
        initialProps: {
          sessions: [session({}), session({ session_key: "s2" })],
          selected: "s1",
        },
      },
    );
    expect(result.current.size).toBe(0);

    // Act: a new message lands in the unselected s2.
    rerender({
      sessions: [session({}), session({ session_key: "s2", last_message_time: 200 })],
      selected: "s1",
    });

    // Assert
    expect(result.current.has("s2")).toBe(true);
    expect(result.current.has("s1")).toBe(false);
  });

  it("marks_selected_session_read_and_persists_across_reloads", () => {
    // Arrange: s2 became unread while s1 was selected.
    const { result, rerender } = renderHook(
      ({ sessions, selected }: { sessions: SessionEntry[]; selected: string }) =>
        useUnreadSessions(sessions, selected),
      {
        initialProps: {
          sessions: [session({}), session({ session_key: "s2" })],
          selected: "s1",
        },
      },
    );
    rerender({
      sessions: [session({}), session({ session_key: "s2", last_message_time: 200 })],
      selected: "s1",
    });
    expect(result.current.has("s2")).toBe(true);

    // Act: open s2.
    rerender({
      sessions: [session({}), session({ session_key: "s2", last_message_time: 200 })],
      selected: "s2",
    });

    // Assert
    expect(result.current.size).toBe(0);

    // Arrange + Act: reload with the persisted read state.
    const { result: reloaded } = renderHook(() =>
      useUnreadSessions(
        [session({}), session({ session_key: "s2", last_message_time: 200 })],
        "s1",
      ),
    );

    // Assert
    expect(reloaded.current.size).toBe(0);
  });

  it("marks_new_messages_in_selected_session_read", () => {
    // Arrange
    const { result, rerender } = renderHook(
      ({ sessions, selected }: { sessions: SessionEntry[]; selected: string }) =>
        useUnreadSessions(sessions, selected),
      { initialProps: { sessions: [session({})], selected: "s1" } },
    );

    // Act: the open session receives a reply (visible on next poll).
    rerender({ sessions: [session({ last_message_time: 500 })], selected: "s1" });

    // Assert: the open chat never flags itself unread.
    expect(result.current.size).toBe(0);
  });
});
