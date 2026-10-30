import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { RunList } from "../RunList";
import type { SleepRun } from "../../../shared/api/types";

function makeRun(overrides: Partial<SleepRun>): SleepRun {
  return {
    id: "run-1",
    agent_id: "lyre",
    status: "success",
    trigger: "scheduled",
    started_at: new Date(Date.now() - 3 * 3600_000).toISOString(),
    finished_at: null,
    source_chats_json: "[]",
    source_digest_md: "",
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
    error_message: null,
    session_count: 0,
    ...overrides,
  };
}

/** Stubs global fetch with an apiFetch-compatible response. */
function stubRuns(runs: SleepRun[]) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(() =>
      Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true, runs }) }),
    ),
  );
}

describe("RunList", () => {
  it("requests_the_agent_runs_and_renders_trigger_sessions_and_time", async () => {
    // Arrange
    stubRuns([
      makeRun({ id: "run-1", session_count: 3 }),
      makeRun({ id: "run-2", trigger: "backfill", status: "partial_failure", session_count: 1 }),
    ]);

    // Act
    const { container } = render(
      <RunList agentId="lyre" authToken="t1" selectedRunId={null} onSelectRun={vi.fn()} />,
    );

    // Assert
    await waitFor(() => {
      expect(container.querySelectorAll(".run-row")).toHaveLength(2);
    });
    const requested = String(vi.mocked(fetch).mock.calls[0][0]);
    expect(new URL(requested, "http://localhost").searchParams.get("agent_id")).toBe("lyre");

    const firstRow = container.querySelectorAll(".run-row")[0];
    expect(firstRow.textContent).toContain("Scheduled");
    expect(firstRow.textContent).toContain("3 sessions");
    expect(firstRow.textContent).toContain("3h");
    expect(firstRow.querySelectorAll(".dot-success")).toHaveLength(1);

    const secondRow = container.querySelectorAll(".run-row")[1];
    expect(secondRow.textContent).toContain("Backfill");
    expect(secondRow.textContent).toContain("1 session");
    expect(secondRow.querySelectorAll(".dot-warning")).toHaveLength(1);
  });

  it("marks_the_selected_row_and_reports_selections", async () => {
    stubRuns([makeRun({ id: "run-1" }), makeRun({ id: "run-2" })]);
    const onSelectRun = vi.fn();

    const { container } = render(
      <RunList agentId="lyre" authToken="t2" selectedRunId="run-2" onSelectRun={onSelectRun} />,
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".run-row")).toHaveLength(2);
    });

    const rows = container.querySelectorAll(".run-row");
    expect(rows[1].hasAttribute("data-selected")).toBe(true);
    expect(rows[0].hasAttribute("data-selected")).toBe(false);

    fireEvent.click(rows[0]);
    expect(onSelectRun).toHaveBeenCalledWith("run-1");
  });

  it("shows_empty_state_when_no_runs_exist", async () => {
    stubRuns([]);

    const { container } = render(
      <RunList agentId="lyre" authToken="t3" selectedRunId={null} onSelectRun={vi.fn()} />,
    );

    await waitFor(() => {
      expect(screen.getByText("No sleep batch runs yet")).toBeTruthy();
    });
    expect(container.querySelectorAll(".run-row")).toHaveLength(0);
  });

  it("offers_load_more_when_a_full_page_is_returned", async () => {
    // 25 runs total: the first page fills up, the second page exhausts the list.
    const page = Array.from({ length: 25 }, (_, i) => makeRun({ id: `run-${i}` }));
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        const limit = Number(new URL(url, "http://localhost").searchParams.get("limit") ?? "20");
        return Promise.resolve({
          ok: true,
          json: () => Promise.resolve({ ok: true, runs: page.slice(0, limit) }),
        });
      }),
    );

    const { container } = render(
      <RunList agentId="lyre" authToken="t4" selectedRunId={null} onSelectRun={vi.fn()} />,
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".run-row")).toHaveLength(20);
    });
    expect(screen.getByText("Load more")).toBeTruthy();

    fireEvent.click(screen.getByText("Load more"));

    await waitFor(() => {
      expect(container.querySelectorAll(".run-row")).toHaveLength(25);
    });
    expect(screen.queryByText("Load more")).toBeNull();
  });
});
