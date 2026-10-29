import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ChatMessage, HistoryPage } from "../../../shared/api/types";
import { invalidateQueries } from "../../../shared/hooks/useServerState";

const mocks = vi.hoisted(() => ({ fetchHistory: vi.fn() }));

vi.mock("../../../shared/api/history", () => ({
  HISTORY_PAGE_SIZE: 300,
  fetchHistory: mocks.fetchHistory,
}));

import { useChatHistory } from "../useChatHistory";

function msg(id: string): ChatMessage {
  return {
    id,
    sender_id: "lyre",
    sender_kind: "assistant",
    content: id,
    timestamp: "2026-01-01T00:00:00Z",
    message_kind: "message",
  };
}

function page(
  ids: string[],
  nextBeforeSeq: number | null,
  newestSeq: number | null,
): HistoryPage {
  return { messages: ids.map(msg), nextBeforeSeq, newestSeq };
}

describe("useChatHistory", () => {
  beforeEach(() => {
    mocks.fetchHistory.mockReset();
  });

  it("loads the newest page and prepends older pages", async () => {
    // Arrange: newest page has messages 3,4 and an older cursor at 3.
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(page(["3", "4"], 3, 4));
        if (before === 3) return Promise.resolve(page(["1", "2"], null, 2));
        return Promise.resolve(page([], null, null));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-older", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(2));

    // Act
    await act(async () => {
      await result.current.loadOlder();
    });

    // Assert
    expect(result.current.messages.map((m) => m.id)).toEqual([
      "1",
      "2",
      "3",
      "4",
    ]);
    expect(result.current.atHistoryStart).toBe(true);
    expect(result.current.loadSeq).toBe(1);
    expect(result.current.loadingOlder).toBe(false);
    unmount();
  });

  it("walks past preview-only pages before prepending", async () => {
    // Arrange: the first older page has no visible entries but advances.
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(page(["4"], 4, 4));
        if (before === 4) return Promise.resolve(page([], 2, 3));
        if (before === 2) return Promise.resolve(page(["1"], null, 1));
        return Promise.resolve(page([], null, null));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-preview-page", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(1));

    // Act
    await act(async () => {
      await result.current.loadOlder();
    });

    // Assert
    expect(result.current.messages.map((m) => m.id)).toEqual(["1", "4"]);
    expect(result.current.atHistoryStart).toBe(true);
    unmount();
  });

  it("fills a skipped newest-page window before merging", async () => {
    // Arrange: loaded seq 5,6 plus older 3,4; the next newest page starts at 9.
    let newestPage = page(["5", "6"], 5, 6);
    const olderPages = new Map<number, HistoryPage>([
      [5, page(["3", "4"], 3, 4)],
      [9, page(["7", "8"], 7, 8)],
    ]);
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(newestPage);
        return Promise.resolve(olderPages.get(before) ?? page([], null, null));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-gap", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(2));
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(result.current.messages.map((m) => m.id)).toEqual(["3", "4", "5", "6"]);

    // Act: a burst of commits moved the newest page past the loaded rows.
    newestPage = page(["9", "10"], 9, 10);
    act(() => {
      invalidateQueries("history:chat:test-gap");
    });

    // Assert: the gap page is fetched and the full order is preserved.
    await waitFor(() =>
      expect(result.current.messages.map((m) => m.id)).toEqual([
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "10",
      ]),
    );
    expect(
      mocks.fetchHistory.mock.calls.some(
        (call) => call[0] === "token" && call[1] === "chat:test-gap" && call[2] === 9,
      ),
    ).toBe(true);
    unmount();
  });

  it("keeps older rows when the newest page window slides forward", async () => {
    // Arrange: loaded rows 1..4, then a poll brings rows 5..6.
    let newestPage = page(["3", "4"], 3, 4);
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(newestPage);
        if (before === 3) return Promise.resolve(page(["1", "2"], null, 2));
        return Promise.resolve(page([], null, null));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-slide", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(2));
    await act(async () => {
      await result.current.loadOlder();
    });

    // Act
    newestPage = page(["5", "6"], 5, 6);
    act(() => {
      invalidateQueries("history:chat:test-slide");
    });

    // Assert: the older pages survive the newest-page merge.
    await waitFor(() =>
      expect(result.current.messages.map((m) => m.id)).toEqual([
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
      ]),
    );
    unmount();
  });

  it("ignores an out-of-order stale newest page", async () => {
    // Arrange: the initial page is followed by a slower, older response.
    let newestPage = page(["3", "4"], 3, 4);
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) =>
        Promise.resolve(
          before == null ? newestPage : page([], null, null),
        ),
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-stale", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(2));
    const callsBefore = mocks.fetchHistory.mock.calls.length;

    // Act
    newestPage = page(["1", "2"], 1, 2);
    act(() => {
      invalidateQueries("history:chat:test-stale");
    });
    await waitFor(() =>
      expect(mocks.fetchHistory.mock.calls.length).toBeGreaterThan(callsBefore),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // Assert
    expect(result.current.messages.map((m) => m.id)).toEqual(["3", "4"]);
    unmount();
  });

  it("ignores a stale empty newest page", async () => {
    // Arrange
    let newestPage = page(["1"], null, 1);
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) =>
        Promise.resolve(
          before == null ? newestPage : page([], null, null),
        ),
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-stale-empty", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(1));
    const callsBefore = mocks.fetchHistory.mock.calls.length;

    // Act
    newestPage = page([], null, null);
    act(() => {
      invalidateQueries("history:chat:test-stale-empty");
    });
    await waitFor(() =>
      expect(mocks.fetchHistory.mock.calls.length).toBeGreaterThan(callsBefore),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // Assert
    expect(result.current.messages.map((m) => m.id)).toEqual(["1"]);
    unmount();
  });

  it("drops an older page when the newest page rebases the history", async () => {
    // Arrange: an older page is in flight while the newest page replaces the
    // whole loaded slice (e.g. a compacted history).
    let newestPage = page(["3", "4"], 3, 4);
    let resolveOlder!: (value: HistoryPage) => void;
    const olderPage = new Promise<HistoryPage>((resolve) => {
      resolveOlder = resolve;
    });
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(newestPage);
        if (before === 3) return olderPage;
        return Promise.resolve(page([], null, null));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-rebase", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(2));

    let pending!: Promise<void>;
    act(() => {
      pending = result.current.loadOlder();
    });
    await waitFor(() => expect(result.current.loadingOlder).toBe(true));

    // Act: the newest page rebases the loaded history while the fetch runs.
    newestPage = page(["5", "6"], null, 6);
    act(() => {
      invalidateQueries("history:chat:test-rebase");
    });
    await waitFor(() =>
      expect(result.current.messages.map((m) => m.id)).toEqual(["5", "6"]),
    );
    await act(async () => {
      resolveOlder(page(["1", "2"], null, 2));
      await pending;
    });

    // Assert: the superseded older page is discarded.
    expect(result.current.messages.map((m) => m.id)).toEqual(["5", "6"]);
    expect(result.current.loadingOlder).toBe(false);
    unmount();
  });

  it("defers a gap that needs more than one poll and retries it whole", async () => {
    // Arrange: loaded history stops at seq 1, then the newest page starts at
    // seq 100. A single poll may fetch only 10 gap pages, so the partial gap
    // must not be appended out of order.
    let newestPage = page(["1"], null, 1);
    let partial = true;
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(newestPage);
        if (partial) {
          return Promise.resolve(
            page([`g${before - 1}`], before - 1, before - 1),
          );
        }
        return Promise.resolve(page([`g${before - 1}`], 1, before - 1));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-gap-cap", "token"),
    );
    await waitFor(() =>
      expect(result.current.messages.map((m) => m.id)).toEqual(["1"]),
    );
    const callsBefore = mocks.fetchHistory.mock.calls.length;

    // Act: a large jump exhausts the per-poll gap budget.
    newestPage = page(["100"], 100, 100);
    act(() => {
      invalidateQueries("history:chat:test-gap-cap");
    });
    await waitFor(() =>
      expect(mocks.fetchHistory.mock.calls.length).toBeGreaterThanOrEqual(
        callsBefore + 11,
      ),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // Assert: the view stays stable instead of showing a hole.
    expect(result.current.messages.map((m) => m.id)).toEqual(["1"]);

    // Act: the next poll closes the gap in one page.
    partial = false;
    newestPage = page(["100"], 100, 100);
    act(() => {
      invalidateQueries("history:chat:test-gap-cap");
    });

    // Assert
    await waitFor(() =>
      expect(result.current.messages.map((m) => m.id)).toEqual([
        "1",
        "g99",
        "100",
      ]),
    );
    unmount();
  });

  it("keeps the current view and exposes an error when an older page fails", async () => {
    // Arrange
    let failOlder = false;
    mocks.fetchHistory.mockImplementation(
      (_token: string, _session: string, before?: number | null) => {
        if (before == null) return Promise.resolve(page(["2"], 2, 2));
        if (failOlder) return Promise.reject(new Error("network down"));
        return Promise.resolve(page(["1"], null, 1));
      },
    );
    const { result, unmount } = renderHook(() =>
      useChatHistory("chat:test-older-error", "token"),
    );
    await waitFor(() => expect(result.current.messages).toHaveLength(1));

    // Act: fail, then retry successfully.
    failOlder = true;
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(result.current.messages.map((m) => m.id)).toEqual(["2"]);
    expect(result.current.olderError).toContain("network down");
    expect(result.current.loadingOlder).toBe(false);

    // Scroll/fill triggers must not hammer the failed request.
    const callsAfterFailure = mocks.fetchHistory.mock.calls.length;
    await act(async () => {
      await result.current.loadOlder();
    });
    expect(mocks.fetchHistory.mock.calls.length).toBe(callsAfterFailure);

    // An explicit retry resumes paging.
    failOlder = false;
    await act(async () => {
      await result.current.retryOlder();
    });

    // Assert
    expect(result.current.messages.map((m) => m.id)).toEqual(["1", "2"]);
    expect(result.current.olderError).toBeNull();
    unmount();
  });

  it("drops the previous session when the key changes", async () => {
    // Arrange
    mocks.fetchHistory.mockImplementation(
      (_token: string, session: string) =>
        Promise.resolve(
          session === "chat:test-session-a"
            ? page(["a"], null, 1)
            : page(["b"], null, 1),
        ),
    );
    const { result, rerender, unmount } = renderHook(
      ({ session }: { session: string }) => useChatHistory(session, "token"),
      { initialProps: { session: "chat:test-session-a" } },
    );
    await waitFor(() => expect(result.current.messages.map((m) => m.id)).toEqual(["a"]));

    // Act
    rerender({ session: "chat:test-session-b" });

    // Assert
    await waitFor(() => expect(result.current.messages.map((m) => m.id)).toEqual(["b"]));
    unmount();
  });
});
