import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { ChatMessage } from "../../../shared/api/types";
import {
  mergeChatMessages,
  isWaitingForAssistant,
  reduceAssistantDiscarded,
  reduceChatEvent,
  initialChatState,
  reduceDiscardUserMessage,
  reduceDropRunMessages,
  reduceOptimisticUserMessage,
  reduceRunAccepted,
  reduceRunMissing,
  reduceTagMessageRun,
  reduceToolStart,
  reduceToolResult,
  reduceUserInput,
  type AssistantDiscardedPayload,
  type ChatEventMessage,
  type ChatEventPayload,
  type ToolStartPayload,
  type UserInputPayload,
} from "../chatReducer";

const USER_ID = "web:11111111-1111-1111-1111-111111111111";
const FOLLOW_UP_ID = "web:22222222-2222-2222-2222-222222222222";
const ASSISTANT_1 = "turn:turn-1:assistant:1";
const ASSISTANT_2 = "turn:turn-1:assistant:2";

function textMessage(id: string, text: string): ChatEventMessage {
  return { id, role: "assistant", content: [{ type: "text", text }] };
}

function delta(
  runId: string,
  seq: number,
  id: string,
  text: string,
): ChatEventPayload {
  return {
    runId,
    sessionKey: "main",
    seq,
    state: "delta",
    message: textMessage(id, text),
  };
}

function done(
  runId: string,
  seq: number,
  id: string,
  text: string,
  terminal = true,
): ChatEventPayload {
  return {
    runId,
    sessionKey: "main",
    seq,
    state: "done",
    terminal,
    message: textMessage(id, text),
  };
}

function userInput(
  runId: string,
  messageId: string,
  text: string,
): UserInputPayload {
  return {
    runId,
    messageId,
    senderId: "web-user",
    text,
    timestamp: "2026-09-12T00:00:00Z",
  };
}

describe("chatReducer user identity", () => {
  it("optimistic_message_uses_the_canonical_id_from_the_start", () => {
    let state = initialChatState();

    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "hi" });

    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: USER_ID,
      sender_kind: "user",
      content: "hi",
    });
  });

  it("committed_follow_up_upserts_the_same_id", () => {
    // The server commit names the canonical id the optimistic bubble
    // already carries, so delivery converges without any rename.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: FOLLOW_UP_ID, text: "go on" });

    state = reduceUserInput(state, userInput("run-1", FOLLOW_UP_ID, "go on"));
    state = reduceUserInput(state, userInput("run-1", FOLLOW_UP_ID, "go on"));

    expect(state.messages.map((m) => m.id)).toEqual([FOLLOW_UP_ID]);
    expect(state.messages[0].runId).toBe("run-1");
  });

  it("identical_texts_keep_distinct_ids", () => {
    // Two sends of the same text are two messages; neither consumes the other.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "確認して" });
    state = reduceOptimisticUserMessage(state, { messageId: FOLLOW_UP_ID, text: "確認して" });

    state = reduceUserInput(state, userInput("run-1", FOLLOW_UP_ID, "確認して"));

    expect(state.messages.map((m) => m.id)).toEqual([USER_ID, FOLLOW_UP_ID]);
  });

  it("discarded_send_removes_only_its_own_bubble", () => {
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "hi" });
    state = reduceOptimisticUserMessage(state, { messageId: FOLLOW_UP_ID, text: "hi" });

    state = reduceDiscardUserMessage(state, USER_ID);

    expect(state.messages.map((m) => m.id)).toEqual([FOLLOW_UP_ID]);
  });
});

describe("chatReducer assistant identity", () => {
  it("first_delta_creates_the_message_and_later_deltas_append", () => {
    let state = initialChatState();

    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "Hello"), "lyre");
    state = reduceChatEvent(state, delta("run-1", 2, ASSISTANT_1, " world"), "lyre");

    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: ASSISTANT_1,
      sender_id: "lyre",
      content: "Hello world",
      runId: "run-1",
    });
    expect(isWaitingForAssistant(state)).toBe(false);
  });

  it("done_upserts_the_same_id_with_authoritative_content", () => {
    let state = initialChatState();
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "Hello"), "lyre");

    // A replayed done replaces the streamed text instead of duplicating it.
    state = reduceChatEvent(state, done("run-1", 2, ASSISTANT_1, "Hello world"), "lyre");
    state = reduceChatEvent(state, done("run-1", 2, ASSISTANT_1, "Hello world"), "lyre");

    expect(state.messages.map((m) => m.id)).toEqual([ASSISTANT_1]);
    expect(state.messages[0].content).toBe("Hello world");
  });

  it("done_without_prior_delta_inserts_by_id", () => {
    let state = initialChatState();

    state = reduceChatEvent(state, done("run-1", 1, ASSISTANT_1, "final"), "lyre");

    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      id: ASSISTANT_1,
      sender_id: "lyre",
      content: "final",
    });
  });

  it("tool_narration_keeps_its_id_and_cards_use_call_ids", () => {
    // The narration streamed under ASSISTANT_1 keeps that id across the
    // tool phase; each tool call gets its own card by call id.
    let state = initialChatState();
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "checking"), "lyre");
    const toolStart = (callId: string): ToolStartPayload => ({
      runId: "run-1",
      callId,
      name: "read",
      input: { path: "a.txt" },
    });
    state = reduceToolStart(state, toolStart("call-1"));
    state = reduceToolStart(state, toolStart("call-2"));

    expect(state.messages.find((m) => m.id === ASSISTANT_1)?.content).toBe("checking");
    expect(state.messages.map((m) => m.id)).toEqual([
      ASSISTANT_1,
      "tool:call-1",
      "tool:call-2",
    ]);
    expect(isWaitingForAssistant(state)).toBe(false);
  });

  it("no_narration_tool_call_creates_no_empty_bubble", () => {
    // The model called a tool before emitting any narration: only the tool
    // card appears, never an empty assistant bubble.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "read" });
    state = reduceRunAccepted(state, "run-1");

    state = reduceToolStart(state, {
      runId: "run-1",
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
    });

    expect(state.messages.map((m) => m.id)).toEqual([USER_ID, "tool:call-1"]);

    // The final answer streams after the tool result under its own id.
    state = reduceChatEvent(state, delta("run-1", 2, ASSISTANT_2, "answer"), "lyre");
    state = reduceChatEvent(state, done("run-1", 3, ASSISTANT_2, "answer"), "lyre");

    expect(state.messages.map((m) => m.id)).toEqual([USER_ID, "tool:call-1", ASSISTANT_2]);

    // Merging with the persisted history (which hides the bare tool
    // preview) converges with no leftover bubble.
    const history: ChatMessage[] = [
      {
        id: USER_ID,
        sender_id: "user",
        sender_kind: "user",
        content: "read",
        timestamp: "2026-09-12T00:00:00Z",
        message_kind: "message",
      },
      {
        id: "tool:call-1",
        sender_id: "lyre",
        sender_kind: "tool",
        content: "{}",
        timestamp: "2026-09-12T00:00:01Z",
        message_kind: "tool_call",
      },
      {
        id: ASSISTANT_2,
        sender_id: "lyre",
        sender_kind: "assistant",
        content: "answer",
        timestamp: "2026-09-12T00:00:02Z",
        message_kind: "message",
      },
    ];
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      USER_ID,
      "tool:call-1",
      ASSISTANT_2,
    ]);
  });

  it("next_iteration_streams_under_a_new_id", () => {
    let state = initialChatState();
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "checking"), "lyre");
    state = reduceToolStart(state, { runId: "run-1", callId: "call-1", name: "read" });
    state = reduceChatEvent(state, delta("run-1", 3, ASSISTANT_2, "done"), "lyre");
    state = reduceChatEvent(state, done("run-1", 4, ASSISTANT_2, "done"), "lyre");

    expect(state.messages.map((m) => m.id)).toEqual([
      ASSISTANT_1,
      "tool:call-1",
      ASSISTANT_2,
    ]);
  });

  it("tool_result_upserts_the_card_and_keeps_ownership", () => {
    let state = initialChatState();
    state = reduceToolStart(state, {
      runId: "run-1",
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
    });

    state = reduceToolResult(state, {
      runId: "run-1",
      callId: "call-1",
      name: "read",
      isError: false,
      preview: "done",
      durationMs: 120,
    });

    const card = state.messages.find((m) => m.id === "tool:call-1");
    expect(JSON.parse(card?.content ?? "{}")).toMatchObject({
      tool: "read",
      status: "success",
      result: "done",
      duration_ms: 120,
      input: { path: "a.txt" },
    });
    expect(card?.runId).toBe("run-1");
  });
});

describe("chatReducer retry and error", () => {
  it("discard_removes_only_the_superseded_message", () => {
    let state = initialChatState();
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "I will check"), "lyre");

    const payload: AssistantDiscardedPayload = {
      runId: "run-1",
      messageId: ASSISTANT_1,
    };
    state = reduceAssistantDiscarded(state, payload);
    state = reduceAssistantDiscarded(state, payload);

    expect(state.messages).toHaveLength(0);

    // The retry streams under a new id; the discard cannot touch it.
    state = reduceChatEvent(state, delta("run-1", 3, ASSISTANT_2, "done"), "lyre");
    state = reduceAssistantDiscarded(state, payload);
    expect(state.messages.map((m) => m.id)).toEqual([ASSISTANT_2]);
  });

  it("discard_keeps_waiting_for_the_retry", () => {
    let state = initialChatState();
    state = reduceRunAccepted(state, "run-1");
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "I will check"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);

    // The streamed bubble is dropped, but the retry is still on its way.
    state = reduceAssistantDiscarded(state, { runId: "run-1", messageId: ASSISTANT_1 });
    expect(state.messages).toHaveLength(0);
    expect(isWaitingForAssistant(state)).toBe(true);

    state = reduceChatEvent(state, delta("run-1", 3, ASSISTANT_2, "done"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);
  });

  it("partial_delta_survives_a_terminal_error_unchanged", () => {
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "hi" });
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "途中まで生成"), "lyre");

    state = reduceChatEvent(
      state,
      {
        runId: "run-1",
        sessionKey: "main",
        seq: 2,
        state: "error",
        terminal: true,
        errorMessage: "boom",
      },
      "lyre",
    );

    // The partial message keeps its streamed id; nothing is renamed.
    expect(state.messages.map((m) => m.id)).toEqual([USER_ID, ASSISTANT_1]);
    expect(state.messages.find((m) => m.id === ASSISTANT_1)?.content).toBe("途中まで生成");
    expect(state.error).toBe("boom");
    expect(isWaitingForAssistant(state)).toBe(false);

    // A history refetch carrying only the persisted input keeps both rows
    // with no duplication.
    const history: ChatMessage[] = [
      {
        id: USER_ID,
        sender_id: "user",
        sender_kind: "user",
        content: "hi",
        timestamp: "2026-09-12T00:00:00Z",
        message_kind: "message",
      },
    ];
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      USER_ID,
      ASSISTANT_1,
    ]);
  });

  it("nonterminal_done_and_error_keep_waiting", () => {
    let state = initialChatState();
    state = reduceRunAccepted(state, "run-1");

    state = reduceChatEvent(state, done("run-1", 1, ASSISTANT_1, "part", false), "lyre");
    expect(isWaitingForAssistant(state)).toBe(true);

    state = reduceChatEvent(
      state,
      {
        runId: "run-1",
        sessionKey: "main",
        seq: 2,
        state: "error",
        terminal: false,
        errorMessage: "parent failed",
      },
      "lyre",
    );
    expect(isWaitingForAssistant(state)).toBe(true);
    expect(state.messages.map((m) => m.id)).toEqual([ASSISTANT_1]);
  });

  it("terminal_done_clears_waiting", () => {
    let state = initialChatState();
    state = reduceRunAccepted(state, "run-1");

    state = reduceChatEvent(state, done("run-1", 1, ASSISTANT_1, "done"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);
  });

  it("waiting_returns_once_content_lands_before_a_nonterminal_done", () => {
    // The missed case: a delta clears progress, then the parent turn ends
    // non-terminally while the staged child is still on its way. The run
    // must await progress again instead of going dark.
    let state = initialChatState();
    state = reduceRunAccepted(state, "run-1");
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "part"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);

    state = reduceChatEvent(state, done("run-1", 2, ASSISTANT_1, "part", false), "lyre");
    expect(isWaitingForAssistant(state)).toBe(true);

    // The child turn streams under a new message id on the same run.
    state = reduceChatEvent(state, delta("run-1", 3, ASSISTANT_2, "more"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);
    state = reduceChatEvent(state, done("run-1", 4, ASSISTANT_2, "more"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);
  });

  it("waiting_tracks_runs_independently", () => {
    // Two accepted runs share the session: progress on one must not clear
    // the other's wait.
    let state = initialChatState();
    state = reduceRunAccepted(state, "run-a");
    state = reduceRunAccepted(state, "run-b");
    expect(isWaitingForAssistant(state)).toBe(true);

    state = reduceChatEvent(state, delta("run-a", 1, ASSISTANT_1, "a"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(true);

    state = reduceChatEvent(state, done("run-a", 2, ASSISTANT_1, "a"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(true);

    state = reduceChatEvent(state, delta("run-b", 1, ASSISTANT_2, "b"), "lyre");
    expect(isWaitingForAssistant(state)).toBe(false);
  });
});

describe("chatReducer staged follow-ups", () => {
  it("promoted_child_keeps_the_original_user_id", () => {
    // The follow-up send, its staged commit, and the child's persisted
    // input all share the canonical id: the initial event upserts it.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: FOLLOW_UP_ID, text: "follow-up" });
    state = reduceTagMessageRun(state, { messageId: FOLLOW_UP_ID, runId: "run-1" });

    state = reduceUserInput(state, userInput("run-1", FOLLOW_UP_ID, "follow-up"));

    expect(state.messages.map((m) => m.id)).toEqual([FOLLOW_UP_ID]);
  });

  it("per_turn_dones_upsert_only_their_own_ids", () => {
    // Parent turn A answers, then child turn B answers on the same run.
    // Each done names its own final id; neither touches the other's entries.
    const parentFinal = "turn:turn-a:assistant:2";
    const childFinal = "turn:turn-b:assistant:1";
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "first" });
    state = reduceChatEvent(state, delta("run-1", 1, "turn:turn-a:assistant:1", "a1"), "lyre");
    state = reduceChatEvent(state, done("run-1", 2, parentFinal, "answer-a", false), "lyre");
    state = reduceOptimisticUserMessage(state, { messageId: FOLLOW_UP_ID, text: "follow-up" });
    state = reduceUserInput(state, userInput("run-1", FOLLOW_UP_ID, "follow-up"));
    state = reduceChatEvent(state, delta("run-1", 4, childFinal, "answer-b"), "lyre");
    state = reduceChatEvent(state, done("run-1", 5, childFinal, "answer-b"), "lyre");

    expect(state.messages.map((m) => m.id)).toEqual([
      USER_ID,
      "turn:turn-a:assistant:1",
      parentFinal,
      FOLLOW_UP_ID,
      childFinal,
    ]);

    // Merging with the persisted history converges with no dup.
    const history: ChatMessage[] = state.messages.map((m) => ({ ...m }));
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      USER_ID,
      "turn:turn-a:assistant:1",
      parentFinal,
      FOLLOW_UP_ID,
      childFinal,
    ]);
  });

  it("run_ownership_never_decides_identity", () => {
    // Entries of another run are invisible to this run's events: a done
    // upserts only its own id even when foreign entries exist.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "later" });
    state = reduceTagMessageRun(state, { messageId: USER_ID, runId: "run-9" });

    state = reduceChatEvent(state, done("run-1", 1, ASSISTANT_1, "done"), "lyre");

    expect(state.messages.map((m) => m.id)).toEqual([USER_ID, ASSISTANT_1]);
  });
});

describe("chatReducer reconnect", () => {
  it("truncated_replay_drops_only_the_run_owned_entries", () => {
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "hi" });
    state = reduceTagMessageRun(state, { messageId: USER_ID, runId: "run-1" });
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "part"), "lyre");
    state = reduceToolStart(state, { runId: "run-1", callId: "call-1", name: "read" });
    const otherId = "web:33333333-3333-3333-3333-333333333333";
    state = reduceOptimisticUserMessage(state, { messageId: otherId, text: "other run" });
    state = reduceTagMessageRun(state, { messageId: otherId, runId: "run-9" });

    state = reduceDropRunMessages(state, "run-1");

    // Only run-1's entries are gone; the other run is untouched. The live
    // subscription fills forward and history converges by id.
    expect(state.messages.map((m) => m.id)).toEqual([otherId]);
  });

  it("missing_run_drops_entries_and_clears_progress", () => {
    let state = initialChatState();
    state = reduceRunAccepted(state, "run-1");
    state = reduceChatEvent(state, delta("run-1", 1, ASSISTANT_1, "part"), "lyre");
    state = reduceRunAccepted(state, "run-1");

    state = reduceRunMissing(state, "run-1");

    expect(state.messages).toHaveLength(0);
    expect(isWaitingForAssistant(state)).toBe(false);
  });

  it("slash_done_upserts_its_stable_id", () => {
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { messageId: USER_ID, text: "/status" });
    state = reduceRunAccepted(state, "run-7");

    const slashId = "web:slash:run-7";
    state = reduceChatEvent(state, done("run-7", 1, slashId, "status ok"), "lyre");
    state = reduceChatEvent(state, done("run-7", 1, slashId, "status ok"), "lyre");

    expect(state.messages.map((m) => m.id)).toEqual([USER_ID, slashId]);
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
    // Live and history share ids from the start, so convergence is exact.
    const history = [
      msg({ id: USER_ID, sender_kind: "user", content: "hi" }),
      msg({ id: ASSISTANT_1, content: "answer" }),
      msg({ id: "tool:call-1", message_kind: "tool_call", content: "{}" }),
    ];
    const live = [
      msg({ id: USER_ID, sender_kind: "user", content: "hi" }),
      msg({ id: ASSISTANT_1, content: "answer" }),
      msg({ id: "tool:call-1", message_kind: "tool_call", content: "{}" }),
    ];

    // Act
    const merged = mergeChatMessages(history, live);

    // Assert
    expect(merged.map((m) => m.id)).toEqual([USER_ID, ASSISTANT_1, "tool:call-1"]);
  });

  it("keeps_live_entries_missing_from_history", () => {
    // Arrange
    const history = [msg({ id: "db-1", content: "old" })];
    const live = [
      msg({ id: ASSISTANT_1, content: "old and more" }),
      msg({ id: USER_ID, sender_kind: "user", content: "hi" }),
    ];

    // Act
    const merged = mergeChatMessages(history, live);

    // Assert
    expect(merged.map((m) => m.id)).toEqual(["db-1", ASSISTANT_1, USER_ID]);
  });
});
