import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useSessionRefresh } from "../useSessionRefresh";

describe("useSessionRefresh", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("refreshes sessions and the event's history cache", () => {
    vi.useFakeTimers();
    const invalidate = vi.fn();
    const { result } = renderHook(() => useSessionRefresh(invalidate));

    act(() => result.current({ sessionKey: "chat:1" }));
    expect(invalidate).not.toHaveBeenCalled();

    act(() => vi.advanceTimersByTime(100));

    expect(invalidate).toHaveBeenCalledWith("sessions");
    expect(invalidate).toHaveBeenCalledWith("history:chat:1");
  });

  it("refreshes history for a session other than the open one", () => {
    vi.useFakeTimers();
    const invalidate = vi.fn();
    const { result } = renderHook(() => useSessionRefresh(invalidate));

    act(() => result.current({ sessionKey: "chat:2" }));
    act(() => vi.advanceTimersByTime(100));

    expect(invalidate).toHaveBeenCalledWith("sessions");
    expect(invalidate).toHaveBeenCalledWith("history:chat:2");
  });

  it("treats a resync request as a full history refresh", () => {
    vi.useFakeTimers();
    const invalidate = vi.fn();
    const { result } = renderHook(() => useSessionRefresh(invalidate));

    act(() => result.current({ resync: true }));
    act(() => vi.advanceTimersByTime(100));

    expect(invalidate).toHaveBeenCalledWith("sessions");
    expect(invalidate).toHaveBeenCalledWith("history");
  });

  it("coalesces a burst into one refresh", () => {
    vi.useFakeTimers();
    const invalidate = vi.fn();
    const { result } = renderHook(() => useSessionRefresh(invalidate));

    act(() => {
      result.current({ sessionKey: "chat:1" });
      result.current({ sessionKey: "chat:1" });
      result.current({ sessionKey: "chat:1" });
    });
    act(() => vi.advanceTimersByTime(100));

    expect(invalidate).toHaveBeenCalledTimes(2);
  });

  it("coalesces events for several sessions", () => {
    vi.useFakeTimers();
    const invalidate = vi.fn();
    const { result } = renderHook(() => useSessionRefresh(invalidate));

    act(() => {
      result.current({ sessionKey: "chat:1" });
      result.current({ sessionKey: "chat:2" });
    });
    act(() => vi.advanceTimersByTime(100));

    expect(invalidate).toHaveBeenCalledWith("sessions");
    expect(invalidate).toHaveBeenCalledWith("history:chat:1");
    expect(invalidate).toHaveBeenCalledWith("history:chat:2");
    expect(invalidate).toHaveBeenCalledTimes(3);
  });
});
