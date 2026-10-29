import { describe, it, expect, vi, beforeEach } from "vitest";

const mocks = vi.hoisted(() => ({ apiFetch: vi.fn() }));

vi.mock("../client", () => ({ apiFetch: mocks.apiFetch }));

import { fetchHistory, HISTORY_PAGE_SIZE } from "../history";

describe("fetchHistory", () => {
  beforeEach(() => {
    mocks.apiFetch.mockReset();
  });

  it("requests the configured page size and maps the cursor fields", async () => {
    // Arrange
    mocks.apiFetch.mockResolvedValue({
      ok: true,
      messages: [],
      next_before_seq: 42,
      newest_seq: 80,
    });

    // Act
    const page = await fetchHistory("token", "chat:7");

    // Assert
    const path = mocks.apiFetch.mock.calls[0][0] as string;
    expect(path).toContain("session_key=chat%3A7");
    expect(path).toContain(`limit=${HISTORY_PAGE_SIZE}`);
    expect(path).not.toContain("before_seq=");
    expect(page).toEqual({ messages: [], nextBeforeSeq: 42, newestSeq: 80 });
  });

  it("passes the older cursor when paging backwards", async () => {
    // Arrange
    mocks.apiFetch.mockResolvedValue({
      ok: true,
      messages: [],
      next_before_seq: null,
      newest_seq: 40,
    });

    // Act
    await fetchHistory("token", "chat:7", 42);

    // Assert
    const path = mocks.apiFetch.mock.calls[0][0] as string;
    expect(path).toContain("before_seq=42");
  });

  it("treats missing page metadata as the beginning of history", async () => {
    // Arrange
    mocks.apiFetch.mockResolvedValue({ ok: true, messages: [] });

    // Act
    const page = await fetchHistory("token", "chat:7");

    // Assert
    expect(page).toEqual({ messages: [], nextBeforeSeq: null, newestSeq: null });
  });
});
