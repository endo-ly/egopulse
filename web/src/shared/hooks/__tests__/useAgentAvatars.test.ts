import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { useAgentAvatars } from "../useAgentAvatars";
import type { AgentEntry } from "../../api/types";

function agent(overrides: Partial<AgentEntry>): AgentEntry {
  return {
    id: "lyre",
    label: "Lyre",
    is_default: true,
    ...overrides,
  };
}

describe("useAgentAvatars", () => {
  let urlCounter = 0;

  beforeEach(() => {
    Object.assign(URL, {
      createObjectURL: vi.fn(() => `blob:${(urlCounter += 1)}`),
      revokeObjectURL: vi.fn(),
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    urlCounter = 0;
  });

  it("resolves_authorized_avatar_urls_to_object_urls", async () => {
    const fetchMock = vi.fn(
      async (_path: string, _init?: RequestInit) =>
        new Response(new Blob(["img"], { type: "image/png" })),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() =>
      useAgentAvatars(
        [agent({ avatar_url: "/api/agents/lyre/avatar?v=1" }), agent({ id: "ace", label: "Ace", avatar_url: null })],
        "token",
      ),
    );

    await waitFor(() => expect(result.current["lyre"]).toBe("blob:1"));
    expect(result.current["ace"]).toBeUndefined();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [, init] = fetchMock.mock.calls[0];
    expect((init as RequestInit).headers).toMatchObject({
      Authorization: "Bearer token",
    });
  });

  it("refetches_when_version_changes_and_revokes_when_removed", async () => {
    const fetchMock = vi.fn(
      async () => new Response(new Blob(["img"], { type: "image/png" })),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result, rerender } = renderHook(
      ({ avatar_url }) => useAgentAvatars([agent({ avatar_url })], "token"),
      { initialProps: { avatar_url: "/api/agents/lyre/avatar?v=1" as string | null } },
    );

    await waitFor(() => expect(result.current["lyre"]).toBe("blob:1"));

    rerender({ avatar_url: "/api/agents/lyre/avatar?v=2" });
    await waitFor(() => expect(result.current["lyre"]).toBe("blob:2"));
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:1");

    rerender({ avatar_url: null });
    await waitFor(() => expect(result.current["lyre"]).toBeUndefined());
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:2");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
