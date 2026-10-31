import { describe, it, expect, vi, afterEach } from "vitest";
import { renderHook, waitFor, cleanup, act } from "@testing-library/react";
import { useServerState, invalidateQueries, invalidateQuery } from "../useServerState";

describe("useServerState", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    invalidateQueries("test-");
  });

  it("fetches initial data", async () => {
    const fetcher = vi.fn().mockResolvedValue("hello");
    const { result } = renderHook(() => useServerState("test-init", fetcher));

    await waitFor(() => expect(result.current.data).toBe("hello"));
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("polls at the given interval while the tab is visible", async () => {
    const fetcher = vi.fn().mockResolvedValue("polled");
    renderHook(() =>
      useServerState("test-poll", fetcher, { pollIntervalMs: 50 }),
    );

    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(3), {
      timeout: 2000,
    });
  });

  it("does not poll without pollIntervalMs", async () => {
    const fetcher = vi.fn().mockResolvedValue("no-poll");
    renderHook(() => useServerState("test-nopoll", fetcher));

    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("pauses polling while hidden and refetches immediately on regain", async () => {
    const fetcher = vi.fn().mockResolvedValue("vis");
    renderHook(() =>
      useServerState("test-vis", fetcher, { pollIntervalMs: 50 }),
    );

    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));

    Object.defineProperty(document, "visibilityState", {
      value: "hidden",
      configurable: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));

    const countAfterHide = fetcher.mock.calls.length;
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(fetcher.mock.calls.length).toBe(countAfterHide);

    Object.defineProperty(document, "visibilityState", {
      value: "visible",
      configurable: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));

    await waitFor(
      () => expect(fetcher.mock.calls.length).toBeGreaterThan(countAfterHide),
      { timeout: 2000 },
    );
  });

  it("keeps polling when the caller passes a new fetcher on every render", async () => {
    // Arrange
    const fetcher = vi.fn().mockResolvedValue("stable");
    const { rerender } = renderHook(
      ({ tick }: { tick: number }) =>
        useServerState("test-stable-poll", () => fetcher(tick), { pollIntervalMs: 40 }),
      { initialProps: { tick: 0 } },
    );
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));

    // Act: re-render more often than the poll interval, as a parent that
    // animates or polls on its own does.
    for (let tick = 1; tick <= 12; tick += 1) {
      await new Promise((resolve) => setTimeout(resolve, 15));
      rerender({ tick });
    }

    // Assert: those renders did not restart the timer, so it kept firing
    expect(fetcher.mock.calls.length).toBeGreaterThanOrEqual(3);
  });

  it("ignores a response that arrives after a newer request", async () => {
    // Arrange: two requests whose answers arrive on demand
    const pending: Array<(value: string) => void> = [];
    const fetcher = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          pending.push(resolve);
        }),
    );
    const { result } = renderHook(() => useServerState("test-race", fetcher));
    await waitFor(() => expect(pending).toHaveLength(1));

    // Act: a second request starts before the first one answers, and the newer
    // one answers first
    act(() => result.current.invalidate());
    await waitFor(() => expect(pending).toHaveLength(2));
    await act(async () => pending[1]("newer"));
    await waitFor(() => expect(result.current.data).toBe("newer"));
    await act(async () => pending[0]("stale"));

    // Assert: the late answer from the older request is dropped
    expect(result.current.data).toBe("newer");
  });

  it("drops a response that was in flight when the cache was invalidated", async () => {
    // Arrange
    const pending: Array<(value: string) => void> = [];
    const fetcher = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          pending.push(resolve);
        }),
    );
    const { result } = renderHook(() => useServerState("test-invalidate-race", fetcher));
    await waitFor(() => expect(pending).toHaveLength(1));

    // Act: the cache is dropped while the first request is still running
    act(() => invalidateQuery("test-invalidate-race"));
    await waitFor(() => expect(pending.length).toBeGreaterThanOrEqual(2));
    await act(async () => pending[0]("stale"));

    // Assert: the dropped request must not resurrect the cache, and the request
    // that replaced it still lands
    expect(result.current.data).toBeUndefined();
    await act(async () => pending[1]("fresh"));
    await waitFor(() => expect(result.current.data).toBe("fresh"));
  });

  it("revalidates cached data on mount while keeping it visible", async () => {
    // Arrange: a first mount fills the cache
    const first = renderHook(() => useServerState("test-revalidate", () => Promise.resolve("old")));
    await waitFor(() => expect(first.result.current.data).toBe("old"));
    first.unmount();
    const fetcher = vi.fn().mockResolvedValue("new");

    // Act
    const { result } = renderHook(() =>
      useServerState("test-revalidate", fetcher, { revalidateOnMount: true }),
    );

    // Assert: cached data is shown at once, then replaced in the background
    expect(result.current.data).toBe("old");
    await waitFor(() => expect(result.current.data).toBe("new"));
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("does not revalidate cached data without revalidateOnMount", async () => {
    // Arrange
    const first = renderHook(() => useServerState("test-cached", () => Promise.resolve("old")));
    await waitFor(() => expect(first.result.current.data).toBe("old"));
    first.unmount();
    const fetcher = vi.fn().mockResolvedValue("new");

    // Act
    const { result } = renderHook(() => useServerState("test-cached", fetcher));
    await new Promise((resolve) => setTimeout(resolve, 50));

    // Assert
    expect(result.current.data).toBe("old");
    expect(fetcher).not.toHaveBeenCalled();
  });
});
