import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import {
  reduceChatEvent,
  initialChatState,
  reduceToolStart,
  reduceToolResult,
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
    state = reduceChatEvent(state, delta1);

    const draft = state.messages.find((m) => m.id === "draft:run-1");
    expect(draft).toBeTruthy();
    expect(draft?.content).toBe("Hello");

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
    state = reduceChatEvent(state, delta2);

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
    state = reduceChatEvent(state, done);

    const finalized = state.messages.find((m) => m.id === "draft:run-1:done");
    expect(finalized).toBeTruthy();
    expect(finalized?.content).toBe("Hello world");
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
