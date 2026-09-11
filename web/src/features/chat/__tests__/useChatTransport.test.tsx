import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import { useChatTransport } from "../useChatTransport";
import { invalidateQueries } from "../../../shared/hooks/useServerState";

vi.mock("../../../shared/hooks/useServerState", async (importOriginal) => {
  const mod =
    await importOriginal<typeof import("../../../shared/hooks/useServerState")>();
  return { ...mod, invalidateQueries: vi.fn() };
});

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  readyState = FakeWebSocket.CONNECTING;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    if (this.readyState === FakeWebSocket.CLOSED) return;
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }

  simulateOpen(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  receive(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
}

function setup(
  options: {
    sessionKey?: string;
    onSessionResolved?: (sessionKey: string) => void;
  } = {},
) {
  const initialProps = {
    sessionKey: options.sessionKey ?? "s1",
    agentId: "default",
    authToken: "token",
    onAuthRequired: vi.fn(),
    onError: vi.fn(),
    onSessionResolved: options.onSessionResolved,
  };
  return renderHook((props) => useChatTransport(props ?? initialProps), {
    initialProps,
  });
}

describe("useChatTransport reconnect", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeWebSocket.instances = [];
    vi.stubGlobal("WebSocket", FakeWebSocket as unknown as typeof WebSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
    cleanup();
  });

  it("chat_transport_reconnects_with_backoff_after_unexpected_close", () => {
    const onError = vi.fn();
    const { result } = renderHook(() =>
      useChatTransport({
        sessionKey: "s1",
        agentId: "default",
        authToken: "token",
        onAuthRequired: vi.fn(),
        onError,
      }),
    );
    expect(result.current.connectionState).toBe("closed");

    act(() => {
      void result.current.connect();
    });
    const first = FakeWebSocket.instances[0];
    act(() => first.simulateOpen());
    expect(result.current.connectionState).toBe("open");

    act(() => first.close());
    expect(result.current.connectionState).toBe("closed");
    // The drop is reported once ("Retrying…" is not spammy per retry).
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError).toHaveBeenCalledWith("Connection lost. Retrying…");

    // First retry is scheduled after the base delay.
    act(() => vi.advanceTimersByTime(999));
    expect(FakeWebSocket.instances).toHaveLength(1);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(2);

    // Failing again before opening grows the delay (1s -> 2s).
    act(() => FakeWebSocket.instances[1].close());
    act(() => vi.advanceTimersByTime(1999));
    expect(FakeWebSocket.instances).toHaveLength(2);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(3);

    // A successful open resets the backoff.
    act(() => FakeWebSocket.instances[2].simulateOpen());
    act(() => FakeWebSocket.instances[2].close());
    act(() => vi.advanceTimersByTime(999));
    expect(FakeWebSocket.instances).toHaveLength(3);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(4);
  });

  it("chat_transport_does_not_reconnect_after_auth_rejection", () => {
    const onAuthRequired = vi.fn();
    const { result } = renderHook(() =>
      useChatTransport({
        sessionKey: "s1",
        agentId: "default",
        authToken: "bad",
        onAuthRequired,
        onError: vi.fn(),
      }),
    );

    act(() => {
      result.current.connect().catch(() => {});
    });
    const ws = FakeWebSocket.instances[0];
    act(() => ws.simulateOpen());
    act(() => {
      ws.receive({ type: "event", event: "connect.challenge" });
    });
    expect(ws.sent).toHaveLength(1);

    act(() => {
      ws.receive({
        type: "res",
        id: "connect",
        ok: false,
        error: { code: "unauthorized", message: "bad token" },
      });
    });

    expect(onAuthRequired).toHaveBeenCalledWith("bad token");
    expect(result.current.connectionState).toBe("closed");

    act(() => vi.advanceTimersByTime(60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  async function connectOpen(
    options?: Parameters<typeof setup>[0],
  ) {
    const { result, rerender } = setup(options);
    act(() => {
      void result.current.connect();
    });
    const ws = FakeWebSocket.instances[0];
    act(() => ws.simulateOpen());
    act(() => ws.receive({ type: "event", event: "connect.challenge" }));
    act(() => ws.receive({ type: "res", id: "connect", ok: true }));
    expect(result.current.connectionState).toBe("open");
    return { result, ws, rerender };
  }

  function lastSentChat(ws: FakeWebSocket): {
    id: string;
    params: { requestId: string };
  } {
    const chatFrames = ws.sent.filter((sent) => sent.includes('"chat.send"'));
    return JSON.parse(chatFrames[chatFrames.length - 1]) as {
      id: string;
      params: { requestId: string };
    };
  }

  it("chat_transport_resolves_send_on_ack_and_shows_text_immediately", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const chatFrame = lastSentChat(ws);
    const rpcId = chatFrame.id;
    const durableRequestId = chatFrame.params.requestId;
    // Optimistic message is visible before the ack lands.
    expect(
      result.current.state.messages.find((m) => m.id === `local:${durableRequestId}`),
    ).toMatchObject({ sender_kind: "user", content: "hello" });
    const sentChat = JSON.parse(
      ws.sent.find((frame) => frame.includes('"chat.send"'))!,
    ) as { params: { agentId: string; requestId: string } };
    expect(sentChat.params.agentId).toBe("default");
    expect(sentChat.params.requestId).toBe(durableRequestId);

    let resolved: string | null = null;
    await act(async () => {
      ws.receive({ type: "res", id: rpcId, ok: true });
      resolved = await pending;
    });
    expect(resolved).toBe(durableRequestId);
    expect(
      result.current.state.messages.some((m) => m.id === `local:${durableRequestId}`),
    ).toBe(true);
  });

  it("chat_transport_tags_optimistic_message_with_run_on_ack", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const chatFrame = lastSentChat(ws);
    const durableRequestId = chatFrame.params.requestId;

    await act(async () => {
      ws.receive({
        type: "res",
        id: chatFrame.id,
        ok: true,
        payload: { runId: "run-tag", sessionKey: "s1" },
      });
      await pending;
    });

    expect(
      result.current.state.messages.find((m) => m.id === `local:${durableRequestId}`),
    ).toMatchObject({ runId: "run-tag" });
  });

  it("chat_transport_routes_events_to_the_current_session", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const rpcId = lastSentChat(ws).id;

    await act(async () => {
      ws.receive({
        type: "res",
        id: rpcId,
        ok: true,
        payload: { runId: "run-a", sessionKey: "chat:42" },
      });
      await pending;
    });

    act(() => {
      ws.receive({
        type: "event",
        event: "tool_start",
        payload: {
          runId: "run-a",
          sessionKey: "chat:42",
          callId: "call-a",
          name: "read",
          input: { path: "a.txt" },
        },
      });
      ws.receive({
        type: "event",
        event: "tool_start",
        payload: {
          runId: "run-b",
          sessionKey: "session-b",
          callId: "call-b",
          name: "write",
          input: { path: "b.txt" },
        },
      });
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-b",
          sessionKey: "session-b",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "wrong session" }] },
        },
      });
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "chat:42",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "right session" }] },
        },
      });
    });

    expect(result.current.state.messages.some((message) => message.id === "tool:call-a")).toBe(true);
    expect(result.current.state.messages.some((message) => message.id === "tool:call-b")).toBe(false);
    expect(result.current.state.messages.some((message) => message.content === "wrong session")).toBe(false);
    expect(result.current.state.messages.some((message) => message.content === "right session")).toBe(true);
  });

  it("chat_transport_rejects_send_and_withdraws_text_on_busy", async () => {
    const onError = vi.fn();
    const { result } = renderHook(() =>
      useChatTransport({
        sessionKey: "s1",
        agentId: "default",
        authToken: "token",
        onAuthRequired: vi.fn(),
        onError,
      }),
    );
    act(() => {
      void result.current.connect();
    });
    const ws = FakeWebSocket.instances[0];
    act(() => ws.simulateOpen());
    act(() => ws.receive({ type: "event", event: "connect.challenge" }));
    act(() => ws.receive({ type: "res", id: "connect", ok: true }));

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
      pending.catch(() => {});
    });
    const durableRequestId = lastSentChat(ws).params.requestId;

    await act(async () => {
      ws.receive({
        type: "res",
        id: lastSentChat(ws).id,
        ok: false,
        error: { code: "busy", message: "busy" },
      });
      await expect(pending).rejects.toThrow("busy");
    });

    expect(
      result.current.state.messages.some((m) => m.id === `local:${durableRequestId}`),
    ).toBe(false);
    // The caller surfaces the failure (composer keeps the text); the
    // transport itself stays quiet.
    expect(onError).not.toHaveBeenCalled();
  });

  it("chat_transport_rejects_send_on_ack_timeout", async () => {
    const { result } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
      pending.catch(() => {});
    });
    const ws = FakeWebSocket.instances[0];
    const durableRequestId = lastSentChat(ws).params.requestId;

    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    await expect(pending).rejects.toThrow("timed out");
    expect(
      result.current.state.messages.some((m) => m.id === `local:${durableRequestId}`),
    ).toBe(false);
  });

  it("chat_transport_reuses_the_request_id_when_retrying_after_ack_timeout", async () => {
    const { result, ws } = await connectOpen();

    let first!: Promise<string | null>;
    await act(async () => {
      first = result.current.sendMessage("hello", "draft-1");
      first.catch(() => {});
    });
    const firstFrame = lastSentChat(ws);
    const durableRequestId = firstFrame.params.requestId;

    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    await expect(first).rejects.toThrow("timed out");

    let retry!: Promise<string | null>;
    await act(async () => {
      retry = result.current.sendMessage("hello", "draft-1");
    });
    const retryFrame = lastSentChat(ws);
    expect(retryFrame.id).not.toBe(firstFrame.id);
    expect(retryFrame.params.requestId).toBe(durableRequestId);

    let retrySettled = false;
    retry.then(() => {
      retrySettled = true;
    });
    await act(async () => {
      ws.receive({ type: "res", id: firstFrame.id, ok: true });
      await Promise.resolve();
    });
    expect(retrySettled).toBe(false);

    await act(async () => {
      ws.receive({
        type: "res",
        id: retryFrame.id,
        ok: true,
        payload: { runId: "run-retried", sessionKey: "s1" },
      });
      await retry;
    });
  });

  it("chat_transport_uses_a_new_durable_id_after_the_draft_changes", async () => {
    const { result, ws } = await connectOpen();

    let first!: Promise<string | null>;
    await act(async () => {
      first = result.current.sendMessage("hello", "draft-1");
      first.catch(() => {});
    });
    const firstFrame = lastSentChat(ws);
    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    await expect(first).rejects.toThrow("timed out");

    let replacement!: Promise<string | null>;
    await act(async () => {
      replacement = result.current.sendMessage("hello", "draft-2");
    });
    const replacementFrame = lastSentChat(ws);
    expect(replacementFrame.params.requestId).not.toBe(firstFrame.params.requestId);

    await act(async () => {
      ws.receive({ type: "res", id: replacementFrame.id, ok: true });
      await replacement;
    });
  });

  it("chat_transport_reuses_the_durable_id_after_connection_loss", async () => {
    const { result, ws } = await connectOpen();

    let first!: Promise<string | null>;
    await act(async () => {
      first = result.current.sendMessage("hello", "draft-1");
      first.catch(() => {});
    });
    const firstFrame = lastSentChat(ws);

    act(() => ws.close());
    await expect(first).rejects.toThrow("Connection lost");
    await act(async () => {
      vi.advanceTimersByTime(1_000);
    });
    const retrySocket = FakeWebSocket.instances[1];
    act(() => retrySocket.simulateOpen());
    act(() => retrySocket.receive({ type: "event", event: "connect.challenge" }));
    act(() => retrySocket.receive({ type: "res", id: "connect", ok: true }));

    let retry!: Promise<string | null>;
    await act(async () => {
      retry = result.current.sendMessage("hello", "draft-1");
    });
    const retryFrame = lastSentChat(retrySocket);
    expect(retryFrame.id).not.toBe(firstFrame.id);
    expect(retryFrame.params.requestId).toBe(firstFrame.params.requestId);

    await act(async () => {
      retrySocket.receive({ type: "res", id: retryFrame.id, ok: true });
      await retry;
    });
  });

  it("chat_transport_does_not_reuse_uncertain_identity_after_session_change", async () => {
    const hook = renderHook(
      ({ sessionKey }: { sessionKey: string }) =>
        useChatTransport({
          sessionKey,
          agentId: "default",
          authToken: "token",
          onAuthRequired: vi.fn(),
          onError: vi.fn(),
        }),
      { initialProps: { sessionKey: "s1" } },
    );
    act(() => {
      void hook.result.current.connect();
    });
    const ws = FakeWebSocket.instances[0];
    act(() => ws.simulateOpen());
    act(() => ws.receive({ type: "event", event: "connect.challenge" }));
    act(() => ws.receive({ type: "res", id: "connect", ok: true }));

    let first!: Promise<string | null>;
    await act(async () => {
      first = hook.result.current.sendMessage("hello", "draft-1");
      first.catch(() => {});
    });
    const firstFrame = lastSentChat(ws);
    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    await expect(first).rejects.toThrow("timed out");

    hook.rerender({ sessionKey: "s2" });
    let second!: Promise<string | null>;
    await act(async () => {
      second = hook.result.current.sendMessage("hello", "draft-1");
    });
    const secondFrame = lastSentChat(ws);
    expect(secondFrame.params.requestId).not.toBe(firstFrame.params.requestId);
    await act(async () => {
      ws.receive({ type: "res", id: secondFrame.id, ok: true });
      await second;
    });
    hook.unmount();
  });

  it("chat_transport_resyncs_after_unexpected_drop", async () => {
    const mockInvalidate = vi.mocked(invalidateQueries);
    mockInvalidate.mockClear();
    const { result } = await connectOpen();

    act(() => {
      FakeWebSocket.instances[0].close();
    });
    expect(result.current.connectionState).toBe("closed");

    await act(async () => {
      vi.advanceTimersByTime(1_000);
    });
    expect(FakeWebSocket.instances).toHaveLength(2);
    act(() => FakeWebSocket.instances[1].simulateOpen());

    expect(mockInvalidate).toHaveBeenCalledWith("sessions");
    expect(mockInvalidate).toHaveBeenCalledWith("history");
  });

  it("chat_transport_reconnects_immediately_on_visibility_regain", () => {
    const setVisibility = (state: "visible" | "hidden") => {
      Object.defineProperty(document, "visibilityState", {
        value: state,
        configurable: true,
      });
    };
    const { result } = setup();
    act(() => {
      void result.current.connect();
    });
    act(() => FakeWebSocket.instances[0].simulateOpen());
    act(() => FakeWebSocket.instances[0].close());
    // A retry is now pending behind the backoff timer.
    expect(FakeWebSocket.instances).toHaveLength(1);

    // Regaining visibility skips the remaining backoff and reconnects now.
    act(() => {
      setVisibility("visible");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(FakeWebSocket.instances).toHaveLength(2);

    // The superseded backoff timer must not open another socket.
    act(() => vi.advanceTimersByTime(30_000));
    expect(FakeWebSocket.instances).toHaveLength(2);

    // An open socket is left alone — visibility alone does not reconnect.
    act(() => FakeWebSocket.instances[1].simulateOpen());
    act(() => {
      setVisibility("visible");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(FakeWebSocket.instances).toHaveLength(2);
  });

  it("chat_transport_shows_thinking_draft_on_ack", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const rpcId = lastSentChat(ws).id;

    // Act: the server accepts the run.
    await act(async () => {
      ws.receive({
        type: "res",
        id: rpcId,
        ok: true,
        payload: { runId: "run-a", sessionKey: "s1" },
      });
      await pending;
    });

    // Assert: an empty assistant draft (the typing indicator) is visible
    // before any delta arrives.
    expect(
      result.current.state.messages.find((m) => m.id === "draft:run-a"),
    ).toMatchObject({ sender_kind: "assistant", sender_id: "default", content: "" });

    // The first delta fills the placeholder instead of adding a bubble.
    act(() => {
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "s1",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "Hi" }] },
        },
      });
    });
    expect(
      result.current.state.messages.filter((m) =>
        m.id.startsWith("draft:run-a"),
      ),
    ).toHaveLength(1);
    expect(
      result.current.state.messages.find((m) => m.id === "draft:run-a"),
    ).toMatchObject({ content: "Hi" });
  });

  it("chat_transport_ack_from_a_left_session_does_not_pollute_the_active_one", async () => {
    const onSessionResolved = vi.fn();
    const { result, ws, rerender } = await connectOpen({
      onSessionResolved,
    });

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const rpcId = lastSentChat(ws).id;

    // The user leaves for session s2 before the ack arrives; the displayed
    // transcript resets.
    act(() => {
      rerender({
        sessionKey: "s2",
        agentId: "default",
        authToken: "token",
        onAuthRequired: vi.fn(),
        onError: vi.fn(),
        onSessionResolved,
      });
    });
    expect(result.current.state.messages).toHaveLength(0);

    // Act: session s1's ack and events arrive while s2 is displayed.
    await act(async () => {
      ws.receive({
        type: "res",
        id: rpcId,
        ok: true,
        payload: { runId: "run-a" },
      });
      await pending;
    });
    act(() => {
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "chat:42",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "Hi" }] },
        },
      });
    });
    act(() => {
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "chat:42",
          seq: 2,
          state: "done",
          terminal: true,
          message: { role: "assistant", content: [{ type: "text", text: "Hi" }] },
        },
      });
    });

    // Assert: session s2 shows nothing from s1's send, and the canonical
    // session of s1's run never switches the displayed session.
    expect(result.current.state.messages).toHaveLength(0);
    expect(onSessionResolved).not.toHaveBeenCalled();
  });

  it("chat_transport_resolves_the_canonical_session_on_done_after_reconnect", async () => {
    const onSessionResolved = vi.fn();
    const { result, ws } = await connectOpen({
      // A send from a fresh (not yet persisted) session; the server will
      // canonicalize it to chat:42 once the run is accepted.
      sessionKey: "web:new-chat",
      onSessionResolved,
    });

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    await act(async () => {
      ws.receive({
        type: "res",
        id: lastSentChat(ws).id,
        ok: true,
        payload: { runId: "run-a" },
      });
      await pending;
    });

    // The socket drops and reconnects; the run is resubscribed.
    act(() => ws.close());
    await act(async () => {
      vi.advanceTimersByTime(1_000);
    });
    const retry = FakeWebSocket.instances[1];
    act(() => retry.simulateOpen());
    act(() => retry.receive({ type: "event", event: "connect.challenge" }));
    act(() => retry.receive({ type: "res", id: "connect", ok: true }));
    const subscription = retry.sent
      .map((sent) => JSON.parse(sent) as {
        method?: string;
        id: string;
      })
      .find((frame) => frame.method === "run.subscribe");
    expect(subscription).toBeTruthy();
    act(() => {
      retry.receive({
        type: "res",
        id: subscription!.id,
        ok: true,
        payload: { runId: "run-a", replayed: 0, replayTruncated: false, done: false },
      });
    });

    // Act: the replayed done carries the canonical session key.
    act(() => {
      retry.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "chat:42",
          seq: 2,
          state: "done",
          terminal: true,
          message: { role: "assistant", content: [{ type: "text", text: "Hi" }] },
        },
      });
    });

    // Assert: the displayed session follows the run's canonical session.
    expect(onSessionResolved).toHaveBeenCalledWith("chat:42");
  });

  it("chat_transport_rebuilds_from_history_when_replay_is_truncated", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    await act(async () => {
      ws.receive({
        type: "res",
        id: lastSentChat(ws).id,
        ok: true,
        payload: { runId: "run-a" },
      });
      await pending;
    });
    expect(
      result.current.state.messages.find((m) => m.id === "draft:run-a"),
    ).toBeTruthy();

    // The socket drops and reconnects; the run is resubscribed.
    act(() => ws.close());
    await act(async () => {
      vi.advanceTimersByTime(1_000);
    });
    const retry = FakeWebSocket.instances[1];
    act(() => retry.simulateOpen());
    act(() => retry.receive({ type: "event", event: "connect.challenge" }));
    act(() => retry.receive({ type: "res", id: "connect", ok: true }));
    const subscription = retry.sent
      .map((sent) => JSON.parse(sent) as { method?: string; id: string })
      .find((frame) => frame.method === "run.subscribe");
    expect(subscription).toBeTruthy();

    // Act: the ack reports that the replay buffer cannot bridge the gap.
    act(() => {
      retry.receive({
        type: "res",
        id: subscription!.id,
        ok: true,
        payload: { runId: "run-a", replayed: 3, replayTruncated: true, done: false },
      });
    });

    // Assert: the frozen draft is dropped and history is refetched instead
    // of trusting a stream that starts mid-run.
    expect(
      result.current.state.messages.find((m) => m.id === "draft:run-a"),
    ).toBeUndefined();
    expect(invalidateQueries).toHaveBeenCalledWith("sessions");
    expect(invalidateQueries).toHaveBeenCalledWith("history");
  });

  it("chat_transport_stamps_streamed_messages_with_the_agent_of_the_send", async () => {
    const { result, ws, rerender } = await connectOpen({ sessionKey: "s1" });

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    await act(async () => {
      ws.receive({
        type: "res",
        id: lastSentChat(ws).id,
        ok: true,
        payload: { runId: "run-a" },
      });
      await pending;
    });

    // The user switches the selected agent while the run is streaming.
    act(() => {
      rerender({
        sessionKey: "s1",
        agentId: "lyre",
        authToken: "token",
        onAuthRequired: vi.fn(),
        onError: vi.fn(),
        onSessionResolved: undefined,
      });
    });

    // Act: the run's delta arrives after the switch.
    act(() => {
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "s1",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "Hi" }] },
        },
      });
    });

    // Assert: the streamed message is attributed to the sending agent, not
    // the one selected at event time.
    expect(
      result.current.state.messages.find((m) => m.id === "draft:run-a"),
    ).toMatchObject({ sender_id: "default" });
  });

  it("chat_transport_resubscribes_to_inflight_run_after_reconnect", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const rpcId = lastSentChat(ws).id;
    await act(async () => {
      ws.receive({
        type: "res",
        id: rpcId,
        ok: true,
        payload: { runId: "run-a", sessionKey: "s1" },
      });
      await pending;
    });
    act(() => {
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "s1",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "Hello" }] },
        },
      });
    });

    // Act: the socket drops and reconnects.
    act(() => ws.close());
    await act(async () => {
      vi.advanceTimersByTime(1_000);
    });
    const retry = FakeWebSocket.instances[1];
    act(() => retry.simulateOpen());
    act(() => retry.receive({ type: "event", event: "connect.challenge" }));
    act(() => retry.receive({ type: "res", id: "connect", ok: true }));

    // Assert: the in-flight run is resubscribed with the last applied seq.
    const frames = retry.sent.map((sent) => JSON.parse(sent) as {
      method?: string;
      id: string;
      params?: { runId: string; lastSeq: number };
    });
    const subscription = frames.find((frame) => frame.method === "run.subscribe");
    expect(subscription).toBeTruthy();
    expect(subscription?.params).toEqual({
      runId: "run-a",
      lastSeq: 1,
    });

    // The replayed remainder completes the transcript.
    act(() => {
      retry.receive({
        type: "res",
        id: subscription!.id,
        ok: true,
        payload: { runId: "run-a", replayed: 1, done: false },
      });
      retry.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "s1",
          seq: 2,
          state: "done",
          message: {
            role: "assistant",
            content: [{ type: "text", text: "Hello world" }],
          },
        },
      });
    });
    expect(
      result.current.state.messages.find((m) => m.id === "draft:run-a:done"),
    ).toMatchObject({ content: "Hello world" });
  });

  it("chat_transport_drops_replayed_frames_at_or_below_last_seq", async () => {
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const rpcId = lastSentChat(ws).id;
    await act(async () => {
      ws.receive({
        type: "res",
        id: rpcId,
        ok: true,
        payload: { runId: "run-a", sessionKey: "s1" },
      });
      await pending;
    });

    const delta = (text: string) => ({
      type: "event" as const,
      event: "chat",
      payload: {
        runId: "run-a",
        sessionKey: "s1",
        seq: 1,
        state: "delta",
        message: { role: "assistant", content: [{ type: "text", text }] },
      },
    });

    // Act: the same seq arrives twice (e.g. overlapping replay).
    act(() => {
      ws.receive(delta("Hello"));
      ws.receive(delta("Hello"));
    });

    // Assert: the duplicate is not appended twice.
    const draft = result.current.state.messages.find((m) => m.id === "draft:run-a");
    expect(draft?.content).toBe("Hello");
  });

  it("chat_transport_forgets_run_after_run_not_found", async () => {
    const mockInvalidate = vi.mocked(invalidateQueries);
    const { result, ws } = await connectOpen();

    let pending!: Promise<string | null>;
    await act(async () => {
      pending = result.current.sendMessage("hello", "draft-1");
    });
    const rpcId = lastSentChat(ws).id;
    await act(async () => {
      ws.receive({
        type: "res",
        id: rpcId,
        ok: true,
        payload: { runId: "run-a", sessionKey: "s1" },
      });
      await pending;
    });
    act(() => {
      ws.receive({
        type: "event",
        event: "chat",
        payload: {
          runId: "run-a",
          sessionKey: "s1",
          seq: 1,
          state: "delta",
          message: { role: "assistant", content: [{ type: "text", text: "Hel" }] },
        },
      });
    });

    act(() => ws.close());
    await act(async () => {
      vi.advanceTimersByTime(1_000);
    });
    const retry = FakeWebSocket.instances[1];
    act(() => retry.simulateOpen());
    act(() => retry.receive({ type: "event", event: "connect.challenge" }));
    act(() => retry.receive({ type: "res", id: "connect", ok: true }));
    const frames = retry.sent.map((sent) => JSON.parse(sent) as {
      method?: string;
      id: string;
    });
    const subscription = frames.find((frame) => frame.method === "run.subscribe");
    expect(subscription).toBeTruthy();

    // Act: the run is no longer replayable (TTL expiry / restart).
    mockInvalidate.mockClear();
    act(() => {
      retry.receive({
        type: "res",
        id: subscription!.id,
        ok: false,
        error: { code: "run_not_found", message: "run is gone" },
      });
    });

    // Assert: the transport falls back to persisted history...
    expect(mockInvalidate).toHaveBeenCalledWith("history");

    // ...and discards the frozen draft its events can never complete.
    expect(
      result.current.state.messages.some((m) => m.id.startsWith("draft:run-a")),
      "a forgotten run's frozen draft must not linger",
    ).toBe(false);

    // ...and stops resubscribing to the forgotten run.
    act(() => retry.close());
    await act(async () => {
      vi.advanceTimersByTime(2_000);
    });
    const third = FakeWebSocket.instances[2];
    act(() => third.simulateOpen());
    act(() => third.receive({ type: "event", event: "connect.challenge" }));
    act(() => third.receive({ type: "res", id: "connect", ok: true }));
    expect(
      third.sent.some((sent) => sent.includes("run.subscribe")),
      "a forgotten run must not be resubscribed",
    ).toBe(false);
  });

  it("chat_transport_disconnect_suppresses_reconnect", () => {
    const { result } = setup();

    act(() => {
      void result.current.connect();
    });
    act(() => FakeWebSocket.instances[0].simulateOpen());

    act(() => result.current.disconnect());
    expect(result.current.connectionState).toBe("closed");

    act(() => vi.advanceTimersByTime(60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);
  });
});
