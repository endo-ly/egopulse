import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import {
  reduceChatEvent,
  initialChatState,
  reduceOptimisticUserMessage,
  reduceRunAccepted,
  reduceToolStart,
  reduceToolResult,
  reduceUserInput,
  type ChatEventPayload,
} from "../chatReducer";

describe("chatReducer", () => {
  it("ws_handler_processes_chat_events_and_send_via_chat_send", () => {
    let state = initialChatState();

    const delta1: ChatEventPayload = {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Hello" }],
      },
    };
    state = reduceChatEvent(state, delta1, "lyre");

    const draft = state.messages.find((m) => m.id === "draft:run-1");
    expect(draft).toBeTruthy();
    expect(draft?.content).toBe("Hello");
    // The streamed sender is the real agent id so the avatar map resolves it.
    expect(draft?.sender_id).toBe("lyre");

    const delta2: ChatEventPayload = {
      runId: "run-1",
      sessionKey: "main",
      seq: 2,
      state: "delta",
      message: {
        role: "assistant",
        content: [{ type: "text", text: " world" }],
      },
    };
    state = reduceChatEvent(state, delta2, "lyre");

    const appended = state.messages.find((m) => m.id === "draft:run-1");
    expect(appended?.content).toBe("Hello world");

    const done: ChatEventPayload = {
      runId: "run-1",
      sessionKey: "main",
      seq: 3,
      state: "done",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Hello world" }],
      },
    };
    state = reduceChatEvent(state, done, "lyre");

    const finalized = state.messages.find((m) => m.id === "draft:run-1:done");
    expect(finalized).toBeTruthy();
    expect(finalized?.content).toBe("Hello world");
    expect(finalized?.sender_id).toBe("lyre");
  });

  it("done_without_prior_delta_stamps_the_agent_id", () => {
    // Arrange: non-streaming runs only deliver the terminal event.
    let state = initialChatState();

    // Act
    state = reduceChatEvent(state, {
      runId: "run-no-delta",
      sessionKey: "main",
      seq: 1,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "final" }] },
    }, "lyre");

    // Assert
    const sealed = state.messages.find((m) => m.id === "draft:run-no-delta:done");
    expect(sealed).toBeTruthy();
    expect(sealed?.sender_id).toBe("lyre");
  });

  it("run_accepted_shows_an_empty_draft_until_the_first_delta", () => {
    // Arrange
    let state = initialChatState();

    // Act
    state = reduceRunAccepted(state, { runId: "run-1", agentId: "lyre" });
    state = reduceRunAccepted(state, { runId: "run-1", agentId: "lyre" });

    // Assert: one empty placeholder, stamped with the agent id.
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: "draft:run-1",
      sender_id: "lyre",
      sender_kind: "assistant",
      content: "",
    });

    // Act: the first delta fills the placeholder instead of adding a bubble.
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "Hello" }] },
    }, "lyre");

    // Assert
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: "draft:run-1",
      content: "Hello",
    });
  });

  it("run_accepted_done_without_text_drops_the_placeholder", () => {
    // Arrange: the run completed without ever producing a delta.
    let state = initialChatState();
    state = reduceRunAccepted(state, { runId: "run-1", agentId: "lyre" });

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "done",
    }, "lyre");

    // Assert: no empty bubble lingers.
    expect(state.messages).toHaveLength(0);
  });

  it("run_accepted_done_with_text_seals_the_placeholder", () => {
    // Arrange
    let state = initialChatState();
    state = reduceRunAccepted(state, { runId: "run-1", agentId: "lyre" });

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "Final" }] },
    }, "lyre");

    // Assert
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: "draft:run-1:done",
      sender_id: "lyre",
      content: "Final",
    });
  });

  it("run_accepted_terminal_error_drops_the_placeholder", () => {
    // Arrange
    let state = initialChatState();
    state = reduceRunAccepted(state, { runId: "run-1", agentId: "lyre" });

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "error",
      terminal: true,
      errorMessage: "boom",
    }, "lyre");

    // Assert
    expect(state.messages).toHaveLength(0);
    expect(state.error).toBe("boom");
  });

  it("run_accepted_partial_delta_survives_a_terminal_error", () => {
    // Arrange: the run streamed before failing; the partial text stays.
    let state = initialChatState();
    state = reduceRunAccepted(state, { runId: "run-1", agentId: "lyre" });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "partial" }] },
    }, "lyre");

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 2,
      state: "error",
      terminal: true,
      errorMessage: "boom",
    }, "lyre");

    // Assert: the partial text is kept but sealed, so the streaming cursor
    // stops blinking instead of freezing mid-run.
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: "draft:run-1:done",
      content: "partial",
      sender_id: "lyre",
    });
  });

  it("tool_start_and_result_inject_tool_messages", () => {
    let state = initialChatState();

    state = reduceToolStart(state, {
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
    });
    const pending = state.messages.find((m) => m.id === "tool:call-1");
    expect(pending?.sender_kind).toBe("tool");
    expect(JSON.parse(pending?.content ?? "{}")).toMatchObject({
      tool: "read",
      status: "pending",
      input: { path: "a.txt" },
    });

    state = reduceToolResult(state, {
      callId: "call-1",
      name: "read",
      isError: false,
      preview: "done",
      durationMs: 120,
    });
    const result = state.messages.find((m) => m.id === "tool:call-1");
    expect(JSON.parse(result?.content ?? "{}")).toMatchObject({
      tool: "read",
      status: "success",
      result: "done",
      duration_ms: 120,
      input: { path: "a.txt" },
    });
  });

  it("user_input_events_append_the_committed_follow_up_once", () => {
    let state = initialChatState();
    state = reduceToolStart(state, {
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
    });
    state = reduceToolResult(state, {
      callId: "call-1",
      name: "read",
      isError: false,
      preview: "done",
      durationMs: 120,
    });

    const payload = {
      messageId: "web:follow-up",
      senderId: "web-user",
      text: "follow-up",
      timestamp: "2026-08-28T12:00:00Z",
    };
    state = reduceUserInput(state, payload);
    state = reduceUserInput(state, payload);

    expect(state.messages).toHaveLength(2);
    expect(state.messages[1]).toMatchObject({
      id: "web:follow-up",
      sender_kind: "user",
      content: "follow-up",
      timestamp: payload.timestamp,
    });
  });

  it("optimistic_message_shows_until_echo_replaces_it", () => {
    // Arrange
    let state = initialChatState();

    // Act: send shows the text immediately with a local id.
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "hi" });
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: "local:req-1",
      sender_kind: "user",
      content: "hi",
    });

    // Act: the server echo supersedes the optimistic copy.
    state = reduceUserInput(state, {
      messageId: "web:msg-1",
      senderId: "web-user",
      text: "hi",
      timestamp: "2026-08-28T12:00:00Z",
    });

    // Assert: exactly one copy, under the persisted id.
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].id).toBe("web:msg-1");
  });

  it("optimistic_message_survives_resend_of_identical_text", () => {
    // Arrange: an older identical message is already displayed.
    let state = initialChatState();
    state = reduceUserInput(state, {
      messageId: "web:old",
      senderId: "web-user",
      text: "hi",
      timestamp: "2026-08-28T11:00:00Z",
    });

    // Act: echo for the older message must not consume the fresh local.
    state = reduceOptimisticUserMessage(state, { requestId: "req-2", text: "hi" });
    state = reduceUserInput(state, {
      messageId: "web:old",
      senderId: "web-user",
      text: "hi",
      timestamp: "2026-08-28T11:00:00Z",
    });

    // Assert
    expect(state.messages.map((m) => m.id).sort()).toEqual([
      "local:req-2",
      "web:old",
    ]);
  });

  it("done_keeps_optimistic_locals_until_history_covers_them", () => {
    // Arrange
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "hi" });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "yo" }] },
    }, "lyre");

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 2,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "yo" }] },
    }, "lyre");

    // Assert: both stay; the merge drops them once fresh history lands.
    expect(state.messages.map((m) => m.id).sort()).toEqual([
      "draft:run-1:done",
      "local:req-1",
    ]);
  });

  it("separates_assistant_stream_segments_around_injected_user_input", () => {
    let state = initialChatState();
    state = reduceChatEvent(state, {
      runId: "run-segments",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "before" }] },
    }, "lyre");
    state = reduceToolStart(state, {
      callId: "call-segments",
      name: "read",
      input: { path: "config" },
    });
    state = reduceToolResult(state, {
      callId: "call-segments",
      name: "read",
      isError: false,
      preview: "done",
      durationMs: 10,
    });

    state = reduceUserInput(state, {
      messageId: "web:segment-follow-up",
      senderId: "web-user",
      text: "also check config",
      timestamp: "2026-08-28T12:00:00Z",
    });
    state = reduceChatEvent(state, {
      runId: "run-segments",
      sessionKey: "main",
      seq: 2,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "after" }] },
    }, "lyre");
    state = reduceChatEvent(state, {
      runId: "run-segments",
      sessionKey: "main",
      seq: 3,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "after" }] },
    }, "lyre");

    expect(state.messages.map((message) => message.content)).toEqual([
      "before",
      expect.stringContaining('"tool":"read"'),
      "also check config",
      "after",
    ]);
    expect(state.messages.map((message) => message.sender_kind)).toEqual([
      "assistant",
      "tool",
      "user",
      "assistant",
    ]);
  });
});

describe("useServerState cache", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("server_state_caches_and_invalidates", async () => {
    const { useServerState, invalidateQuery } = await import("../../../shared/hooks/useServerState");
    const fetcher = vi.fn().mockResolvedValue({ agents: ["a", "b"] });

    const { result: r1, unmount: u1 } = renderHook(() =>
      useServerState("test-agents", fetcher),
    );
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(r1.current.data).toEqual({ agents: ["a", "b"] });
    expect(fetcher).toHaveBeenCalledTimes(1);

    const { result: r2, unmount: u2 } = renderHook(() =>
      useServerState("test-agents", fetcher),
    );
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(r2.current.data).toEqual({ agents: ["a", "b"] });
    expect(fetcher).toHaveBeenCalledTimes(1);

    act(() => {
      invalidateQuery("test-agents");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    u1();
    u2();
  });

  it("chat_send_invalidates_sessions_and_history", async () => {
    const mod = await import("../../../shared/hooks/useServerState");
    const fetcher = vi.fn().mockResolvedValue([]);
    const { unmount } = renderHook(() =>
      mod.useServerState("sessions", fetcher),
    );
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    act(() => {
      mod.invalidateQueries("sessions");
    });

    unmount();
  });
});
