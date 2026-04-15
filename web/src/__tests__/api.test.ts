import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  api,
  AuthRequiredError,
  loadAuthToken,
  persistAuthToken,
  AUTH_TOKEN_STORAGE_KEY,
  sessionKeyNow,
  webSessionKey,
  makeId,
  parseSseFrames,
} from "../api";

describe("api", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("sets Authorization header when token is provided", async () => {
    // Arrange
    const mockJson = { ok: true, data: 42 };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(mockJson), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    // Act
    const result = await api<{ ok: boolean; data: number }>(
      "/api/test",
      "my-secret-token",
    );

    // Assert
    expect(result).toEqual(mockJson);
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "/api/test",
      expect.objectContaining({
        headers: expect.objectContaining({
          Authorization: "Bearer my-secret-token",
        }),
      }),
    );
  });

  it("throws AuthRequiredError on 401", async () => {
    // Arrange
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ message: "Unauthorized" }), {
        status: 401,
      }),
    );

    // Act & Assert
    await expect(api("/api/test", "token")).rejects.toThrow(
      AuthRequiredError,
    );
  });

  it("throws on network error", async () => {
    // Arrange
    vi.spyOn(globalThis, "fetch").mockRejectedValue(
      new TypeError("Failed to fetch"),
    );

    // Act & Assert
    await expect(api("/api/test", "")).rejects.toThrow("Network error");
  });

  it("throws on HTTP error", async () => {
    // Arrange
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "Bad Request" }), {
        status: 400,
      }),
    );

    // Act & Assert
    await expect(api("/api/test", "token")).rejects.toThrow("Bad Request");
  });
});

function makeMockResponse(chunks: string[]): Response {
  const encoder = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(encoder.encode(chunk));
      }
      controller.close();
    },
  });
  return new Response(stream, {
    headers: { "Content-Type": "text/event-stream" },
  });
}

describe("parseSseFrames", () => {
  it("yields events from SSE data", async () => {
    // Arrange
    const response = makeMockResponse([
      'event: foo\ndata: {"key":"val"}\n\n',
    ]);
    const controller = new AbortController();

    // Act
    const events = [];
    for await (const event of parseSseFrames(response, controller.signal)) {
      events.push(event);
    }

    // Assert
    expect(events).toEqual([{ event: "foo", payload: { key: "val" } }]);
  });

  it("handles multiline data by joining with newline", async () => {
    // Arrange
    const response = makeMockResponse([
      'event: multi\ndata: {"part1":\ndata: "value"}\n\n',
    ]);
    const controller = new AbortController();

    // Act
    const events = [];
    for await (const event of parseSseFrames(response, controller.signal)) {
      events.push(event);
    }

    // Assert
    expect(events).toEqual([
      { event: "multi", payload: { part1: "value" } },
    ]);
  });

  it("handles empty lines that flush events", async () => {
    // Arrange
    const response = makeMockResponse([
      'data: {"a":1}\n\ndata: {"b":2}\n\n',
    ]);
    const controller = new AbortController();

    // Act
    const events = [];
    for await (const event of parseSseFrames(response, controller.signal)) {
      events.push(event);
    }

    // Assert
    expect(events).toHaveLength(2);
    expect(events[0]).toEqual({ event: "message", payload: { a: 1 } });
    expect(events[1]).toEqual({ event: "message", payload: { b: 2 } });
  });

  it("ignores comment lines", async () => {
    // Arrange
    const response = makeMockResponse([
      ': this is a comment\ndata: {"ok":true}\n\n',
    ]);
    const controller = new AbortController();

    // Act
    const events = [];
    for await (const event of parseSseFrames(response, controller.signal)) {
      events.push(event);
    }

    // Assert
    expect(events).toEqual([{ event: "message", payload: { ok: true } }]);
  });

  it("stops on abort signal", async () => {
    // Arrange
    const response = makeMockResponse([
      'data: {"first":true}\n\n',
      'data: {"second":true}\n\n',
    ]);
    const controller = new AbortController();
    controller.abort();

    // Act
    const events = [];
    for await (const event of parseSseFrames(response, controller.signal)) {
      events.push(event);
    }

    // Assert
    expect(events).toHaveLength(0);
  });
});

describe("loadAuthToken", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("returns stored value", () => {
    // Arrange
    localStorage.setItem(AUTH_TOKEN_STORAGE_KEY, "stored-token");

    // Act
    const result = loadAuthToken();

    // Assert
    expect(result).toBe("stored-token");
  });

  it("returns empty string when nothing stored", () => {
    // Act
    const result = loadAuthToken();

    // Assert
    expect(result).toBe("");
  });
});

describe("persistAuthToken", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("saves non-empty token", () => {
    // Act
    persistAuthToken("my-token");

    // Assert
    expect(localStorage.getItem(AUTH_TOKEN_STORAGE_KEY)).toBe("my-token");
  });

  it("removes empty string token", () => {
    // Arrange
    localStorage.setItem(AUTH_TOKEN_STORAGE_KEY, "old");
    persistAuthToken("");

    // Assert
    expect(localStorage.getItem(AUTH_TOKEN_STORAGE_KEY)).toBeNull();
  });
});

describe("sessionKeyNow", () => {
  it("formats correctly as session-YYYYMMDDHHmmss", () => {
    // Act
    const result = sessionKeyNow();

    // Assert
    expect(result).toMatch(/^session-\d{14}$/);
  });
});

describe("webSessionKey", () => {
  it.each([
    ["", "main"],
    ["web:foo", "foo"],
    ["  web:bar  ", "bar"],
    ["plain", "plain"],
  ] as const)("normalizes %j → %j", (input, expected) => {
    expect(webSessionKey(input)).toBe(expected);
  });
});

describe("makeId", () => {
  it("generates unique IDs on successive calls", () => {
    // Act
    const id1 = makeId("test");
    const id2 = makeId("test");

    // Assert
    expect(id1).not.toBe(id2);
    expect(id1).toMatch(/^test:/);
    expect(id2).toMatch(/^test:/);
  });
});
