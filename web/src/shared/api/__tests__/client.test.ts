import { afterEach, describe, expect, it, vi } from "vitest";
import { apiFetch } from "../client";
import { AuthRequiredError } from "../auth";
import { createSessionKey } from "../sessions";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("api client", () => {
  it("api_fetch_sends_auth_header", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ ok: true }), { status: 200 }),
    );

    await apiFetch("/api/agents", "secret-token");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/agents",
      expect.objectContaining({
        headers: expect.objectContaining({
          Authorization: "Bearer secret-token",
          "Content-Type": "application/json",
        }),
      }),
    );
  });

  it("api_fetch_maps_unauthorized_to_auth_error", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "token required" }), { status: 401 }),
    );

    await expect(apiFetch("/api/agents", "")).rejects.toBeInstanceOf(AuthRequiredError);
  });

  it("create_session_key_keeps_web_session_format", () => {
    const key = createSessionKey(
      new Date(2026, 6, 3, 12, 34, 56),
      "11111111-1111-4111-8111-111111111111",
    );

    expect(key).toBe(
      "session-20260703123456-11111111-1111-4111-8111-111111111111",
    );
  });

  it("create_session_key_separates_sessions_created_in_the_same_second", () => {
    // Arrange
    const now = new Date(2026, 6, 3, 12, 34, 56);

    // Act
    const first = createSessionKey(now, "first");
    const second = createSessionKey(now, "second");

    // Assert
    expect(first).not.toBe(second);
  });

  it("create_session_key_appends_a_random_suffix", () => {
    // Act
    const key = createSessionKey(new Date(2026, 6, 3, 12, 34, 56));

    // Assert
    expect(key).toMatch(
      /-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
  });
});
