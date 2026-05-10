import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  fetchAgents,
  fetchSleepRuns,
  fetchRunDetail,
  formatTokens,
} from "../api";

describe("formatTokens", () => {
  it("format_tokens_below_1000", () => {
    // Act
    const result = formatTokens(999);

    // Assert
    expect(result).toBe("999");
  });

  it("format_tokens_above_1000", () => {
    // Act
    const result = formatTokens(1247);

    // Assert
    expect(result).toBe("1.2k");
  });

  it("format_tokens_exact_1000", () => {
    // Act
    const result = formatTokens(1000);

    // Assert
    expect(result).toBe("1.0k");
  });

  it("format_tokens_zero", () => {
    // Act
    const result = formatTokens(0);

    // Assert
    expect(result).toBe("0");
  });
});

describe("api sleep functions", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("api_sleep_functions_call_correct_paths", async () => {
    // Arrange
    const mockFetch = vi.spyOn(globalThis, "fetch").mockImplementation(
      (input) => {
        const url =
          typeof input === "string"
            ? input
            : input instanceof URL
              ? input.pathname
              : "";
        if (url.includes("/api/sleep/runs/")) {
          return Promise.resolve(
            new Response(
              JSON.stringify({ ok: true, run: {}, snapshots: [] }),
              { status: 200, headers: { "Content-Type": "application/json" } },
            ),
          );
        }
        if (url.includes("/api/sleep/runs")) {
          return Promise.resolve(
            new Response(JSON.stringify({ ok: true, runs: [] }), {
              status: 200,
              headers: { "Content-Type": "application/json" },
            }),
          );
        }
        return Promise.resolve(
          new Response(JSON.stringify({ ok: true, agents: [] }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        );
      },
    );

    const authToken = "test-token";

    // Act
    await fetchAgents(authToken);
    await fetchSleepRuns(authToken, "agent-1", 10);
    await fetchRunDetail(authToken, "run-1");

    // Assert — verify correct paths
    expect(mockFetch).toHaveBeenCalledWith(
      "/api/agents",
      expect.objectContaining({
        headers: expect.objectContaining({
          Authorization: "Bearer test-token",
        }),
      }),
    );

    expect(mockFetch).toHaveBeenCalledWith(
      "/api/sleep/runs?agent_id=agent-1&limit=10",
      expect.objectContaining({
        headers: expect.objectContaining({
          Authorization: "Bearer test-token",
        }),
      }),
    );

    expect(mockFetch).toHaveBeenCalledWith(
      "/api/sleep/runs/run-1",
      expect.objectContaining({
        headers: expect.objectContaining({
          Authorization: "Bearer test-token",
        }),
      }),
    );

    // Assert — total call count
    expect(mockFetch).toHaveBeenCalledTimes(3);
  });
});
