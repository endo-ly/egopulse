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
    const key = createSessionKey(new Date(2026, 6, 3, 12, 34, 56));

    expect(key).toBe("session-20260703123456");
  });
});
