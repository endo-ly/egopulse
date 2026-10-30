import { describe, it, expect, vi, afterEach } from "vitest";
import { renderHook, waitFor, cleanup } from "@testing-library/react";
import { useServerState, invalidateQueries } from "../useServerState";

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
