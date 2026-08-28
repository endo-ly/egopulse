import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import { useChatTransport } from "../useChatTransport";

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

function setup() {
  return renderHook(() =>
    useChatTransport({
      sessionKey: "s1",
      authToken: "token",
      onAuthRequired: vi.fn(),
      onError: vi.fn(),
    }),
  );
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
