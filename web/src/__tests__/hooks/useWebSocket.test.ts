import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { UiStatus } from "../../types";
import { useWebSocket } from "../../hooks/useWebSocket";

class MockWebSocket {
  static instances: MockWebSocket[] = [];
  static OPEN = 1;

  readyState = 0;
  close = vi.fn(() => {
    this.readyState = 3;
  });
  send = vi.fn();
  private listeners = new Map<string, Array<(event?: unknown) => void>>();

  constructor(public readonly url: string) {
    MockWebSocket.instances.push(this);
  }

  addEventListener(type: string, listener: (event?: unknown) => void) {
    const current = this.listeners.get(type) || [];
    current.push(listener);
    this.listeners.set(type, current);
  }

  dispatch(type: string, event?: unknown) {
    for (const listener of this.listeners.get(type) || []) {
      listener(event);
    }
  }
}

describe("useWebSocket", () => {
  beforeEach(() => {
    MockWebSocket.instances = [];
    vi.stubGlobal("WebSocket", MockWebSocket as unknown as typeof WebSocket);
  });

  it("initial state", () => {
    // Arrange
    const authTokenRef = { current: "token" };

    // Act
    const { result } = renderHook(() =>
      useWebSocket({
        authTokenRef,
        onAuthRequired: vi.fn(),
        onStatusChange: vi.fn() as unknown as (status: UiStatus) => void,
      }),
    );

    // Assert
    expect(result.current.wsState).toBe("connecting");
  });

  it("sets open on success", async () => {
    // Arrange
    const authTokenRef = { current: "token-123" };
    const onStatusChange = vi.fn();
    const { result } = renderHook(() =>
      useWebSocket({
        authTokenRef,
        onAuthRequired: vi.fn(),
        onStatusChange,
      }),
    );

    // Act
    const connectPromise = result.current.connect();
    const socket = MockWebSocket.instances[0];
    socket.readyState = MockWebSocket.OPEN;
    await act(async () => {
      socket.dispatch("message", {
        data: JSON.stringify({ type: "event", event: "connect.challenge" }),
      });
    });
    await act(async () => {
      socket.dispatch("message", {
        data: JSON.stringify({ type: "res", id: "connect", ok: true }),
      });
    });
    await expect(connectPromise).resolves.toBeUndefined();

    // Assert
    await waitFor(() => {
      expect(result.current.wsState).toBe("open");
    });
    expect(socket.send).toHaveBeenCalledWith(
      JSON.stringify({
        type: "req",
        id: "connect",
        method: "connect",
        params: {
          minProtocol: 1,
          maxProtocol: 1,
          authToken: "token-123",
        },
      }),
    );
    expect(onStatusChange).toHaveBeenCalledWith({
      tone: "ok",
      text: "Gateway connected",
    });
  });

  it("sets closed on error", async () => {
    // Arrange
    const authTokenRef = { current: "token" };
    const onStatusChange = vi.fn();
    const { result } = renderHook(() =>
      useWebSocket({
        authTokenRef,
        onAuthRequired: vi.fn(),
        onStatusChange,
      }),
    );

    // Act
    const connectPromise = result.current.connect();
    const socket = MockWebSocket.instances[0];
    await act(async () => {
      socket.dispatch("error");
      await expect(connectPromise).rejects.toThrow("websocket error");
    });

    // Assert
    await waitFor(() => {
      expect(result.current.wsState).toBe("closed");
    });
    expect(onStatusChange).toHaveBeenCalledWith({
      tone: "error",
      text: "Gateway connection failed",
    });
  });

  it("closes on unmount", () => {
    // Arrange
    const authTokenRef = { current: "token" };
    const { result, unmount } = renderHook(() =>
      useWebSocket({
        authTokenRef,
        onAuthRequired: vi.fn(),
        onStatusChange: vi.fn(),
      }),
    );

    // Act
    act(() => {
      void result.current.connect();
    });
    unmount();

    // Assert
    const socket = MockWebSocket.instances[0];
    expect(socket.close).toHaveBeenCalled();
  });
});
