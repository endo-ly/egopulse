import { describe, it, expect } from "vitest";
import { parseRoute, buildRoutePath } from "../router";

describe("router", () => {
  it("router_parses_root_and_agent_scoped_paths", () => {
    expect(parseRoute("/")).toEqual({
      tab: "chat",
      agentId: "",
      sessionKey: null,
      sleepView: "runs",
      runId: null,
    });
    expect(parseRoute("/agents/lyre/chat")).toEqual({
      tab: "chat",
      agentId: "lyre",
      sessionKey: null,
      sleepView: "runs",
      runId: null,
    });
    expect(parseRoute("/agents/lyre/chat/s/web-chat")).toEqual({
      tab: "chat",
      agentId: "lyre",
      sessionKey: "web-chat",
      sleepView: "runs",
      runId: null,
    });
    expect(parseRoute("/agents/lyre/sleep")).toEqual({
      tab: "sleep",
      agentId: "lyre",
      sessionKey: null,
      sleepView: "runs",
      runId: null,
    });
    expect(parseRoute("/metrics")).toEqual({
      tab: "metrics",
      agentId: "",
      sessionKey: null,
      sleepView: "runs",
      runId: null,
    });
  });

  it("router_parses_sleep_run_selection_and_memory_view", () => {
    expect(parseRoute("/agents/lyre/sleep/runs/run-123")).toEqual({
      tab: "sleep",
      agentId: "lyre",
      sessionKey: null,
      sleepView: "runs",
      runId: "run-123",
    });
    expect(parseRoute("/agents/lyre/sleep/memory")).toEqual({
      tab: "sleep",
      agentId: "lyre",
      sessionKey: null,
      sleepView: "memory",
      runId: null,
    });
  });

  it("router_rejects_paths_that_do_not_map_to_a_view", () => {
    expect(parseRoute("/agents/lyre")).toBeNull();
    expect(parseRoute("/agents//chat")).toBeNull();
    expect(parseRoute("/agents/lyre/chat/s/")).toBeNull();
    expect(parseRoute("/agents/lyre/config")).toBeNull();
    expect(parseRoute("/agents/lyre/chat/extra")).toBeNull();
    expect(parseRoute("/agents/lyre/sleep/runs/")).toBeNull();
    expect(parseRoute("/agents/lyre/sleep/memory/extra")).toBeNull();
    expect(parseRoute("/nope")).toBeNull();
  });

  it("router_builds_paths_round_trip", () => {
    expect(
      buildRoutePath({ tab: "chat", agentId: "lyre", sessionKey: null, sleepView: "runs", runId: null }),
    ).toBe("/agents/lyre/chat");
    expect(
      buildRoutePath({ tab: "chat", agentId: "lyre", sessionKey: "web-chat", sleepView: "runs", runId: null }),
    ).toBe("/agents/lyre/chat/s/web-chat");
    expect(
      buildRoutePath({ tab: "sleep", agentId: "lyre", sessionKey: null, sleepView: "runs", runId: null }),
    ).toBe("/agents/lyre/sleep");
    expect(
      buildRoutePath({ tab: "sleep", agentId: "lyre", sessionKey: null, sleepView: "runs", runId: "r1" }),
    ).toBe("/agents/lyre/sleep/runs/r1");
    expect(
      buildRoutePath({ tab: "sleep", agentId: "lyre", sessionKey: null, sleepView: "memory", runId: null }),
    ).toBe("/agents/lyre/sleep/memory");
    expect(
      buildRoutePath({ tab: "metrics", agentId: "", sessionKey: null, sleepView: "runs", runId: null }),
    ).toBe("/metrics");

    // Without an agent, chat collapses to the boot view ("/") and a session
    // cannot be represented; sleep has no representation at all.
    expect(buildRoutePath({ tab: "chat", agentId: "", sessionKey: null, sleepView: "runs", runId: null })).toBe("/");
    expect(buildRoutePath({ tab: "chat", agentId: "", sessionKey: "s1", sleepView: "runs", runId: null })).toBeNull();
    expect(buildRoutePath({ tab: "sleep", agentId: "", sessionKey: null, sleepView: "runs", runId: null })).toBeNull();

    for (const path of [
      "/",
      "/agents/lyre/chat",
      "/agents/lyre/chat/s/web-chat",
      "/agents/lyre/sleep",
      "/agents/lyre/sleep/runs/r1",
      "/agents/lyre/sleep/memory",
      "/metrics",
    ]) {
      const route = parseRoute(path);
      expect(route).not.toBeNull();
      expect(buildRoutePath(route!)).toBe(path);
    }
  });
});
