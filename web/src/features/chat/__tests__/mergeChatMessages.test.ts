import { describe, it, expect } from "vitest";
import { renderHook } from "@testing-library/react";
import { mergeChatMessages, useMergedChatMessages } from "../mergeChatMessages";
import type { ChatMessage } from "../../../shared/api/types";

function msg(overrides: Partial<ChatMessage>): ChatMessage {
  return {
    id: "db-1",
    sender_id: "lyre",
    sender_kind: "assistant",
    content: "hello",
    timestamp: "2026-01-01T12:00:00Z",
    message_kind: "message",
    ...overrides,
  };
}

const ALL_FRESH = (history: ChatMessage[]) =>
  new Set(history.map((m) => m.id));

describe("mergeChatMessages", () => {
  it("returns_history_unchanged_without_live_messages", () => {
    const history = [msg({})];
    expect(mergeChatMessages(history, [], new Set())).toEqual(history);
  });

  it("drops_live_message_with_persisted_id", () => {
    // Arrange: server echo already persisted under the same id.
    const history = [msg({ id: "web:follow-up", sender_kind: "user", content: "hi" })];
    const live = [msg({ id: "web:follow-up", sender_kind: "user", content: "hi" })];

    // Act
    const merged = mergeChatMessages(history, live, ALL_FRESH(history));

    // Assert
    expect(merged).toHaveLength(1);
    expect(merged[0].id).toBe("web:follow-up");
  });

  it("drops_sealed_draft_once_fresh_history_holds_the_same_content", () => {
    // Arrange: done sealed the draft, refetch brought the persisted copy.
    const history = [msg({ id: "db-9", content: "Hello world" })];
    const live = [msg({ id: "draft:run-1:done", content: "Hello world" })];

    // Act
    const merged = mergeChatMessages(history, live, ALL_FRESH(history));

    // Assert
    expect(merged).toHaveLength(1);
    expect(merged[0].id).toBe("db-9");
  });

  it("keeps_sealed_draft_when_match_only_exists_in_stale_history", () => {
    // Arrange: an older identical answer is already displayed; the fresh
    // sealed draft has no persisted copy yet.
    const history = [msg({ id: "db-1", content: "OK" })];
    const live = [msg({ id: "draft:run-2:done", content: "OK" })];

    // Act
    const merged = mergeChatMessages(history, live, new Set());

    // Assert
    expect(merged).toHaveLength(2);
    expect(merged[1].id).toBe("draft:run-2:done");
  });

  it("keeps_streaming_draft_while_content_grows", () => {
    // Arrange: history cannot hold the partial text yet.
    const history = [msg({ id: "db-1", content: "old" })];
    const live = [msg({ id: "draft:run-2", content: "old and more" })];

    // Act
    const merged = mergeChatMessages(history, live, ALL_FRESH(history));

    // Assert
    expect(merged).toHaveLength(2);
    expect(merged[1].id).toBe("draft:run-2");
  });

  it("keeps_optimistic_locals_until_fresh_history_covers_them", () => {
    // Arrange: resending the same text must not hide the fresh message,
    // even though history already holds an older identical one.
    const history = [msg({ id: "db-1", sender_kind: "user", content: "hi" })];
    const live = [msg({ id: "local:req-1", sender_kind: "user", content: "hi" })];

    // Act
    const stale = mergeChatMessages(history, live, new Set());
    const covered = mergeChatMessages(
      [...history, msg({ id: "db-2", sender_kind: "user", content: "hi" })],
      live,
      new Set(["db-2"]),
    );

    // Assert
    expect(stale).toHaveLength(2);
    expect(covered).toHaveLength(2);
    expect(covered.map((m) => m.id).sort()).toEqual(["db-1", "db-2"]);
  });

  it("drops_persisted_tool_card_by_id", () => {
    // Arrange: transport and history share the tool call id space.
    const tool = (id: string) =>
      msg({
        id,
        sender_kind: "assistant",
        content: JSON.stringify({ tool: "read", status: "success" }),
        message_kind: "tool_call",
      });
    const history = [tool("tool:call-1")];
    const live = [tool("tool:call-1")];

    // Act
    const merged = mergeChatMessages(history, live, ALL_FRESH(history));

    // Assert
    expect(merged).toHaveLength(1);
  });
});

describe("useMergedChatMessages", () => {
  it("reconciles_locals_only_against_newly_arrived_history", () => {
    // Arrange: identical text already displayed, fresh local pending.
    const history = [msg({ id: "db-1", sender_kind: "user", content: "hi" })];
    const live = [msg({ id: "local:req-1", sender_kind: "user", content: "hi" })];
    const { result, rerender } = renderHook(
      ({ h, l }: { h: ChatMessage[]; l: ChatMessage[] }) =>
        useMergedChatMessages("s1", h, l),
      { initialProps: { h: history, l: live } },
    );
    expect(result.current.map((m) => m.id).sort()).toEqual([
      "db-1",
      "local:req-1",
    ]);

    // Act: refetch delivers the persisted copy.
    const historyWithDb2 = [
      ...history,
      msg({ id: "db-2", sender_kind: "user", content: "hi" }),
    ];
    rerender({
      h: historyWithDb2,
      l: live,
    });

    // Assert
    expect(result.current.map((m) => m.id).sort()).toEqual(["db-1", "db-2"]);

    // A later live-only render must not reuse db-2 as a fresh reconciliation
    // candidate for the next identical optimistic message.
    rerender({
      h: historyWithDb2,
      l: [msg({ id: "local:req-2", sender_kind: "user", content: "hi" })],
    });
    expect(result.current.map((m) => m.id).sort()).toEqual([
      "db-1",
      "db-2",
      "local:req-2",
    ]);
  });

  it("resets_seen_history_on_session_switch", () => {
    // Arrange
    const { result, rerender } = renderHook(
      ({ s, h }: { s: string; h: ChatMessage[] }) =>
        useMergedChatMessages(s, h, []),
      { initialProps: { s: "s1", h: [msg({ id: "db-1" })] } },
    );
    expect(result.current).toHaveLength(1);

    // Act: switch session with the same message id present.
    rerender({ s: "s2", h: [msg({ id: "db-1" })] });

    // Assert: no crash, history passes through.
    expect(result.current.map((m) => m.id)).toEqual(["db-1"]);
  });
});
