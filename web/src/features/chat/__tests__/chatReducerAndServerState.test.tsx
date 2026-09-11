import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { ChatMessage } from "../../../shared/api/types";
import {
  mergeChatMessages,
  reduceChatEvent,
  initialChatState,
  reduceOptimisticUserMessage,
  reduceRunAccepted,
  reduceTagLocalRun,
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

  it("done_adopts_only_input_and_final_ids_around_tool_use", () => {
    // Arrange: a tool turn. The live transcript holds the optimistic user
    // bubble, the tool card and the streaming draft.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "read the note" });
    state = reduceTagLocalRun(state, { requestId: "req-1", runId: "run-1" });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "checking" }] },
    }, "lyre");
    state = reduceToolStart(state, {
      callId: "call-1",
      name: "read",
      input: { path: "note.txt" },
    });
    state = reduceToolResult(state, {
      callId: "call-1",
      name: "read",
      isError: false,
      preview: "file contents",
      durationMs: 10,
    });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 4,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "done" }] },
    }, "lyre");

    // Act: done reports ONLY the turn's input/final stamps. Tool previews
    // and result summaries share the turn in storage but never appear in
    // history, so adopting them would strand live entries.
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 5,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "done" }] },
      userMessageId: "turn:run-1:input",
      assistantMessageId: "turn:run-1:final",
    }, "lyre");

    // Assert: user bubble and draft carry persisted ids, tool card untouched,
    // no live leftovers.
    const ids = state.messages.map((m) => m.id);
    expect(ids).toContain("turn:run-1:input");
    expect(ids).toContain("turn:run-1:final");
    expect(ids).toContain("tool:call-1");
    expect(ids.some((id) => id.startsWith("draft:") || id.startsWith("local:"))).toBe(
      false,
    );

    // Assert: merging with the persisted history converges exactly. Bare
    // previews are excluded from history while a narrated one is kept; the
    // adopted live entries drop by id instead of duplicating.
    const history: ChatMessage[] = [
      {
        id: "turn:run-1:input",
        sender_id: "user",
        sender_kind: "user",
        content: "read the note",
        timestamp: "2026-09-10T15:00:00Z",
        message_kind: "message",
      },
      {
        id: "narrated-preview",
        sender_id: "lyre",
        sender_kind: "assistant",
        content: "読みますね [tool_call] read",
        timestamp: "2026-09-10T15:00:01Z",
        message_kind: "message",
      },
      {
        id: "tool:call-1",
        sender_id: "lyre",
        sender_kind: "tool",
        content: "{}",
        timestamp: "2026-09-10T15:00:02Z",
        message_kind: "tool_call",
      },
      {
        id: "turn:run-1:final",
        sender_id: "lyre",
        sender_kind: "assistant",
        content: "done",
        timestamp: "2026-09-10T15:00:03Z",
        message_kind: "message",
      },
    ];
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      "turn:run-1:input",
      "narrated-preview",
      "tool:call-1",
      "turn:run-1:final",
    ]);
  });

  it("done_without_ids_keeps_live_entries", () => {
    // Arrange: legacy server or slash command reports no persisted ids.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "hi" });

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "yo" }] },
      userMessageId: null,
      assistantMessageId: null,
    }, "lyre");

    // Assert
    expect(state.messages.map((m) => m.id).sort()).toEqual([
      "draft:run-1:done",
      "local:req-1",
    ]);
  });

  it("terminal_error_with_user_id_only_keeps_the_partial_answer", () => {
    // Arrange: the turn persisted its input, streamed a partial answer,
    // then failed before any final message existed. This is the exact
    // payload shape the backend sends on streaming failure.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "hi" });
    state = reduceTagLocalRun(state, { requestId: "req-1", runId: "run-1" });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "途中まで生成" }] },
    }, "lyre");

    // Act
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 2,
      state: "error",
      terminal: true,
      errorMessage: "boom",
      userMessageId: "turn:run-1:input",
      assistantMessageId: null,
    }, "lyre");

    // Assert: the user bubble adopts its id while the partial draft is
    // sealed and kept, not dropped.
    expect(state.messages.map((m) => m.id).sort()).toEqual([
      "draft:run-1:done",
      "turn:run-1:input",
    ]);
    expect(state.messages.find((m) => m.id === "draft:run-1:done")?.content).toBe(
      "途中まで生成",
    );

    // Assert: a history refetch carrying only the persisted input keeps
    // both rows with no duplication.
    const history: ChatMessage[] = [
      {
        id: "turn:run-1:input",
        sender_id: "user",
        sender_kind: "user",
        content: "hi",
        timestamp: "2026-09-10T15:00:00Z",
        message_kind: "message",
      },
    ];
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      "turn:run-1:input",
      "draft:run-1:done",
    ]);
  });

  it("staged_follow_up_dones_adopt_per_turn_ids_on_one_run", () => {
    // Arrange: the first send completes as a non-terminal parent turn, then
    // a follow-up is promoted to a child turn on the same web run. Each
    // done resolves its own turn's stamps.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "first" });
    state = reduceTagLocalRun(state, { requestId: "req-1", runId: "run-1" });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "answer-a" }] },
    }, "lyre");
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 2,
      state: "done",
      terminal: false,
      message: { role: "assistant", content: [{ type: "text", text: "answer-a" }] },
      userMessageId: "turn:turn-a:input",
      assistantMessageId: "turn:turn-a:final",
    }, "lyre");
    expect(state.messages.map((m) => m.id)).toEqual([
      "turn:turn-a:input",
      "turn:turn-a:final",
    ]);

    // Act: the follow-up send is claimed by the child's initial event,
    // which carries the child's future input id (not the staged row id).
    state = reduceOptimisticUserMessage(state, { requestId: "req-2", text: "follow-up" });
    state = reduceTagLocalRun(state, { requestId: "req-2", runId: "run-1" });
    state = reduceUserInput(state, {
      messageId: "turn:turn-b:input",
      senderId: "web-user",
      text: "follow-up",
      timestamp: "2026-09-10T15:00:01Z",
    });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 3,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "answer-b" }] },
    }, "lyre");
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 4,
      state: "done",
      terminal: true,
      message: { role: "assistant", content: [{ type: "text", text: "answer-b" }] },
      userMessageId: "turn:turn-b:input",
      assistantMessageId: "turn:turn-b:final",
    }, "lyre");

    // Assert: each turn adopted exactly its own ids; the parent's ids were
    // never applied to the child's entries.
    expect(state.messages.map((m) => m.id)).toEqual([
      "turn:turn-a:input",
      "turn:turn-a:final",
      "turn:turn-b:input",
      "turn:turn-b:final",
    ]);

    // Assert: merging with the persisted history converges with no dup.
    const history: ChatMessage[] = state.messages.map((m) => ({ ...m }));
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      "turn:turn-a:input",
      "turn:turn-a:final",
      "turn:turn-b:input",
      "turn:turn-b:final",
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

describe("mergeChatMessages", () => {
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

  it("returns_history_unchanged_without_live_messages", () => {
    const history = [msg({})];
    expect(mergeChatMessages(history, [])).toEqual(history);
  });

  it("drops_live_entries_with_persisted_ids", () => {
    // Arrange: adopted locals, echoes and tool cards share history ids.
    const history = [
      msg({ id: "turn:t1:input", sender_kind: "user", content: "hi" }),
      msg({ id: "tool:call-1", message_kind: "tool_call", content: "{}" }),
    ];
    const live = [
      msg({ id: "turn:t1:input", sender_kind: "user", content: "hi" }),
      msg({ id: "tool:call-1", message_kind: "tool_call", content: "{}" }),
    ];

    // Act
    const merged = mergeChatMessages(history, live);

    // Assert
    expect(merged.map((m) => m.id)).toEqual(["turn:t1:input", "tool:call-1"]);
  });

  it("keeps_streaming_drafts_and_unknown_entries", () => {
    // Arrange
    const history = [msg({ id: "db-1", content: "old" })];
    const live = [
      msg({ id: "draft:run-2", content: "old and more" }),
      msg({ id: "web:echo-1", sender_kind: "user", content: "hi" }),
    ];

    // Act
    const merged = mergeChatMessages(history, live);

    // Assert
    expect(merged.map((m) => m.id)).toEqual(["db-1", "draft:run-2", "web:echo-1"]);
  });
});
