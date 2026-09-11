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
      userMessageId: null,
      assistantMessageId: null,
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
      userMessageId: null,
      assistantMessageId: null,
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
      userMessageId: null,
      assistantMessageId: null,
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
      userMessageId: null,
      assistantMessageId: null,
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
      userMessageId: null,
      assistantMessageId: null,
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
      userMessageId: null,
      assistantMessageId: null,
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
      runId: "run-1",
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
      assistantMessageId: "preview-1",
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
      runId: "run-1",
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
      assistantMessageId: "preview-1",
    });
    state = reduceToolResult(state, {
      callId: "call-1",
      name: "read",
      isError: false,
      preview: "done",
      durationMs: 120,
    });

    const payload = {
      runId: "run-1",
      requestId: null,
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
      runId: "run-1",
      requestId: "req-1",
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
      runId: "run-1",
      requestId: null,
      messageId: "web:old",
      senderId: "web-user",
      text: "hi",
      timestamp: "2026-08-28T11:00:00Z",
    });

    // Act: echo for the older message must not consume the fresh local.
    state = reduceOptimisticUserMessage(state, { requestId: "req-2", text: "hi" });
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: null,
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
      userMessageId: null,
      assistantMessageId: null,
    }, "lyre");

    // Assert: both stay; the merge drops them once fresh history lands.
    expect(state.messages.map((m) => m.id).sort()).toEqual([
      "draft:run-1:done",
      "local:req-1",
    ]);
  });

  it("done_adopts_only_input_and_final_ids_around_tool_use", () => {
    // Arrange: a tool turn with a follow-up injected mid-run. The live
    // transcript holds the optimistic user bubble, the streaming narration,
    // the tool card, the follow-up bubble and the final draft.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "read the note" });
    state = reduceTagLocalRun(state, { requestId: "req-1", runId: "run-1" });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "確認します" }] },
    }, "lyre");
    // The tool phase persists the narration first: the draft adopts the
    // issuing assistant message id immediately, so later segments cannot
    // steal the final id.
    state = reduceToolStart(state, {
      runId: "run-1",
      callId: "call-1",
      name: "read",
      input: { path: "note.txt" },
      assistantMessageId: "preview-1",
    });
    expect(state.messages.find((m) => m.id === "preview-1")?.content).toBe(
      "確認します",
    );
    state = reduceToolResult(state, {
      callId: "call-1",
      name: "read",
      isError: false,
      preview: "file contents",
      durationMs: 10,
    });
    state = reduceOptimisticUserMessage(state, { requestId: "req-2", text: "これも見て" });
    state = reduceTagLocalRun(state, { requestId: "req-2", runId: "run-1" });
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: "req-2",
      messageId: "web:followup-1",
      senderId: "web-user",
      text: "これも見て",
      timestamp: "2026-09-10T15:00:01Z",
    });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 4,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "確認できました" }] },
    }, "lyre");

    // Act: done reports ONLY the turn's input/final stamps.
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 5,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "確認できました" }] },
      userMessageId: "turn:run-1:input",
      assistantMessageId: "turn:run-1:final",
    }, "lyre");

    // Assert: every live entry carries a persisted id; the final id sits on
    // the last segment, never on the earlier narration.
    expect(state.messages.map((m) => m.id)).toEqual([
      "turn:run-1:input",
      "preview-1",
      "tool:call-1",
      "web:followup-1",
      "turn:run-1:final",
    ]);

    // Assert: merging with the persisted history converges exactly — the
    // final answer appears once, not twice.
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
        id: "preview-1",
        sender_id: "lyre",
        sender_kind: "assistant",
        content: "確認します [tool_call] read",
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
        id: "web:followup-1",
        sender_id: "web-user",
        sender_kind: "user",
        content: "これも見て",
        timestamp: "2026-09-10T15:00:03Z",
        message_kind: "message",
      },
      {
        id: "turn:run-1:final",
        sender_id: "lyre",
        sender_kind: "assistant",
        content: "確認できました",
        timestamp: "2026-09-10T15:00:04Z",
        message_kind: "message",
      },
    ];
    expect(mergeChatMessages(history, state.messages).map((m) => m.id)).toEqual([
      "turn:run-1:input",
      "preview-1",
      "tool:call-1",
      "web:followup-1",
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

  it("tool_start_adopts_the_streaming_narration_immediately", () => {
    // Arrange: narration streamed, then the tool phase persists it.
    let state = initialChatState();
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "checking" }] },
    }, "lyre");

    // Act: the first tool call adopts the draft onto the persisted preview.
    state = reduceToolStart(state, {
      runId: "run-1",
      callId: "call-1",
      name: "read",
      input: { path: "a.txt" },
      assistantMessageId: "preview-1",
    });

    // Assert: the narration converged; post-tool deltas start a fresh draft.
    expect(state.messages.find((m) => m.id === "preview-1")?.content).toBe(
      "checking",
    );

    // Act: a concurrent second call in the same phase finds no live draft
    // (no deltas streamed between the two calls), so nothing is adopted.
    state = reduceToolStart(state, {
      runId: "run-1",
      callId: "call-2",
      name: "read",
      input: { path: "b.txt" },
      assistantMessageId: "preview-1",
    });

    // Assert: no duplicate adoption, both cards present.
    expect(state.messages.filter((m) => m.id === "preview-1")).toHaveLength(1);
    expect(state.messages.map((m) => m.id)).toContain("tool:call-2");

    // Act: the next phase streams new narration and persists it under a new
    // id; the fresh draft adopts exactly that id.
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 3,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "phase2" }] },
    }, "lyre");
    state = reduceToolStart(state, {
      runId: "run-1",
      callId: "call-3",
      name: "read",
      input: { path: "c.txt" },
      assistantMessageId: "preview-2",
    });

    // Assert
    expect(state.messages.find((m) => m.id === "preview-2")?.content).toBe(
      "phase2",
    );
    expect(state.messages.find((m) => m.id === "draft:run-1")).toBeUndefined();
  });

  it("user_input_links_commits_to_sends_by_request_id", () => {
    // Arrange: two identical follow-ups sent while the tool phase runs.
    // Content alone cannot tell them apart; the linked request id can, even
    // when commits arrive out of send order.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "確認して" });
    state = reduceTagLocalRun(state, { requestId: "req-1", runId: "run-1" });
    state = reduceOptimisticUserMessage(state, { requestId: "req-2", text: "確認して" });
    state = reduceTagLocalRun(state, { requestId: "req-2", runId: "run-1" });

    // Act: the second commit arrives first; exact linkage still claims the
    // right bubble and leaves the earlier send untouched.
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: "req-2",
      messageId: "web:second",
      senderId: "web-user",
      text: "確認して",
      timestamp: "2026-09-10T15:00:02Z",
    });
    expect(state.messages.map((m) => m.id)).toEqual(["local:req-1", "web:second"]);
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: "req-1",
      messageId: "web:first",
      senderId: "web-user",
      text: "確認して",
      timestamp: "2026-09-10T15:00:01Z",
    });

    // Assert: each commit claimed exactly its own bubble.
    expect(state.messages.map((m) => m.id)).toEqual(["web:second", "web:first"]);
  });

  it("user_input_without_link_falls_back_to_run_fifo", () => {
    // Arrange: two identical bubbles, commits without request linkage.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-1", text: "確認して" });
    state = reduceTagLocalRun(state, { requestId: "req-1", runId: "run-1" });
    state = reduceOptimisticUserMessage(state, { requestId: "req-2", text: "確認して" });
    state = reduceTagLocalRun(state, { requestId: "req-2", runId: "run-1" });

    // Act
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: null,
      messageId: "web:first",
      senderId: "web-user",
      text: "確認して",
      timestamp: "2026-09-10T15:00:01Z",
    });

    // Assert: send order decides.
    expect(state.messages.map((m) => m.id)).toEqual(["local:req-2", "web:first"]);
  });

  it("user_input_never_consumes_another_run_bubble", () => {
    // Arrange: a bubble tagged with a newer run while an older run's commit
    // arrives late.
    let state = initialChatState();
    state = reduceOptimisticUserMessage(state, { requestId: "req-9", text: "later" });
    state = reduceTagLocalRun(state, { requestId: "req-9", runId: "run-9" });

    // Act
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: "other-req",
      messageId: "web:stale",
      senderId: "web-user",
      text: "unrelated",
      timestamp: "2026-09-10T15:00:01Z",
    });

    // Assert: the foreign bubble survives; the commit is appended.
    expect(state.messages.map((m) => m.id)).toEqual(["local:req-9", "web:stale"]);
  });

  it("assistant_adoption_prefers_the_last_sealed_segment", () => {
    // Arrange: two sealed segments with no recorded ids (e.g. tool events
    // missed). Only the trailing one can be the final answer.
    let state = initialChatState();
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 1,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "old" }] },
    }, "lyre");
    state = reduceUserInput(state, {
      runId: "run-1",
      requestId: null,
      messageId: "web:follow-up",
      senderId: "web-user",
      text: "go on",
      timestamp: "2026-09-10T15:00:01Z",
    });
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 2,
      state: "delta",
      message: { role: "assistant", content: [{ type: "text", text: "new" }] },
    }, "lyre");
    state = reduceChatEvent(state, {
      runId: "run-1",
      sessionKey: "main",
      seq: 3,
      state: "done",
      message: { role: "assistant", content: [{ type: "text", text: "new" }] },
      userMessageId: null,
      assistantMessageId: "turn:run-1:final",
    }, "lyre");

    // Assert: the final id sits on the last segment; the earlier one stays
    // sealed instead of stealing it.
    const ids = state.messages.map((m) => m.id);
    expect(ids).toContain("turn:run-1:final");
    expect(ids.filter((id) => id.includes(":done"))).toHaveLength(1);
    expect(
      state.messages.find((m) => m.id === "turn:run-1:final")?.content,
    ).toBe("new");
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
      runId: "run-1",
      requestId: "req-2",
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
      runId: "run-segments",
      callId: "call-segments",
      name: "read",
      input: { path: "config" },
      assistantMessageId: "preview-seg",
    });
    state = reduceToolResult(state, {
      callId: "call-segments",
      name: "read",
      isError: false,
      preview: "done",
      durationMs: 10,
    });

    state = reduceUserInput(state, {
      runId: "run-segments",
      requestId: null,
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
      userMessageId: null,
      assistantMessageId: null,
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
