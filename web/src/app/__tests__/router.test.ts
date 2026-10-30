import { describe, it, expect } from "vitest";
import { parseRoute, buildRoutePath, type AppRoute } from "../router";

describe("router", () => {
  it("router_parses_chat_and_global_paths", () => {
    // Arrange / Act / Assert
    expect(parseRoute("/")).toEqual({ tab: "chat", agentId: "", sessionKey: null });
    expect(parseRoute("/agents/lyre/chat")).toEqual({
      tab: "chat",
      agentId: "lyre",
      sessionKey: null,
    });
    expect(parseRoute("/agents/lyre/chat/s/web-chat")).toEqual({
      tab: "chat",
      agentId: "lyre",
      sessionKey: "web-chat",
    });
    expect(parseRoute("/metrics")).toEqual({ tab: "metrics" });
    expect(parseRoute("/config")).toEqual({ tab: "config" });
  });

  it("router_parses_agent_sections", () => {
    // Arrange / Act / Assert
    expect(parseRoute("/agents/lyre")).toEqual({ tab: "agent", agentId: "lyre", section: "overview" });
    for (const section of ["soul", "rules", "secret", "memory", "skills"] as const) {
      expect(parseRoute(`/agents/lyre/${section}`)).toEqual({ tab: "agent", agentId: "lyre", section });
    }
  });

  it("router_parses_schedule_run_selection", () => {
    // Arrange / Act / Assert
    expect(parseRoute("/agents/lyre/schedule")).toEqual({
      tab: "schedule",
      agentId: "lyre",
      runId: null,
    });
    expect(parseRoute("/agents/lyre/schedule/sleep/run-123")).toEqual({
      tab: "schedule",
      agentId: "lyre",
      runId: "run-123",
    });
  });

  it("router_rejects_paths_that_do_not_map_to_a_view", () => {
    // Arrange
    const invalid = [
      "/agents",
      "/agents//chat",
      "/agents/lyre/chat/s/",
      "/agents/lyre/chat/extra",
      "/agents/lyre/config",
      "/agents/lyre/sleep",
      "/agents/lyre/overview",
      "/agents/lyre/memory/semantic",
      "/agents/lyre/soul/extra",
      "/agents/lyre/schedule/sleep",
      "/agents/lyre/schedule/pulse/run-1",
      "/metrics/extra",
      "/nope",
    ];

    // Act / Assert
    for (const path of invalid) {
      expect(parseRoute(path), path).toBeNull();
    }
  });

  it("router_builds_paths_round_trip", () => {
    // Arrange
    const paths = [
      "/",
      "/agents/lyre/chat",
      "/agents/lyre/chat/s/web-chat",
      "/agents/lyre",
      "/agents/lyre/soul",
      "/agents/lyre/rules",
      "/agents/lyre/secret",
      "/agents/lyre/skills",
      "/agents/lyre/memory",
      "/agents/lyre/schedule",
      "/agents/lyre/schedule/sleep/r1",
      "/metrics",
      "/config",
    ];

    // Act / Assert
    for (const path of paths) {
      const route = parseRoute(path);
      expect(route, path).not.toBeNull();
      expect(buildRoutePath(route!)).toBe(path);
    }
  });

  it("router_cannot_represent_agent_scoped_routes_without_an_agent", () => {
    // Arrange
    const routes: AppRoute[] = [
      { tab: "chat", agentId: "", sessionKey: "s1" },
      { tab: "agent", agentId: "", section: "overview" },
      { tab: "schedule", agentId: "", runId: null },
    ];

    // Act / Assert
    // Without an agent, chat collapses to the boot view ("/") only when no
    // session is selected; other agent-scoped routes have no representation.
    expect(buildRoutePath({ tab: "chat", agentId: "", sessionKey: null })).toBe("/");
    for (const route of routes) {
      expect(buildRoutePath(route)).toBeNull();
    }
  });
});
