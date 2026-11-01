import { describe, expect, it } from "vitest";

import type { RuntimeStatus } from "../../../shared/api/metrics";
import { healthIssues } from "../health";
import { formatChange, formatCount, formatLatency, formatPercent } from "../metricsFormat";

function status(overrides: Partial<RuntimeStatus> = {}): RuntimeStatus {
  return {
    ok: true,
    version: "1",
    uptime_secs: 10,
    db: { ok: true },
    accepting_inputs: true,
    shutdown_started: false,
    channels: { web: { state: "running", last_error: null, last_activity: null } },
    mcp: { connected: [], failed: [] },
    bash_sandbox: { enabled: true },
    active_turns: 0,
    ...overrides,
  };
}

describe("healthIssues", () => {
  it("reports_nothing_for_a_healthy_runtime", () => {
    expect(healthIssues(status())).toEqual([]);
  });

  it("lists_failures_as_danger_and_degraded_states_as_warning", () => {
    // Arrange
    const degraded = status({
      db: { ok: false },
      channels: { telegram: { state: "failed", last_error: "Unauthorized", last_activity: null } },
      mcp: { connected: [], failed: [{ name: "calendar", error: "refused" }] },
      bash_sandbox: { enabled: false, reason: "bwrap missing" },
    });

    // Act
    const issues = healthIssues(degraded);

    // Assert
    expect(issues.map((issue) => [issue.key, issue.severity])).toEqual([
      ["db", "danger"],
      ["channel-telegram", "danger"],
      ["mcp-calendar", "danger"],
      ["sandbox", "warning"],
    ]);
    expect(issues[1].detail).toBe("Unauthorized");
  });
});

describe("metricsFormat", () => {
  it("formats_counts_compactly_without_dropping_integer_zeros", () => {
    expect(formatCount(950)).toBe("950");
    expect(formatCount(12_300)).toBe("12.3k");
    expect(formatCount(120_000)).toBe("120k");
    expect(formatCount(4_560_000)).toBe("4.56M");
    expect(formatCount(1_000_000)).toBe("1M");
  });

  it("returns_null_percent_without_a_denominator", () => {
    expect(formatPercent(1, 0)).toBeNull();
    expect(formatPercent(41, 100)).toBe("41%");
    expect(formatPercent(1, 400)).toBe("0.3%");
  });

  it("describes_change_against_the_previous_period", () => {
    expect(formatChange(10, 0)).toBeNull();
    expect(formatChange(112, 100)).toEqual({ text: "+12%", direction: "up" });
    expect(formatChange(95, 100)).toEqual({ text: "-5%", direction: "down" });
  });

  it("formats_latency", () => {
    expect(formatLatency(null)).toBe("—");
    expect(formatLatency(850)).toBe("850ms");
    expect(formatLatency(8_400)).toBe("8.4s");
    expect(formatLatency(72_000)).toBe("1m 12s");
  });
});
