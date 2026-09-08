import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { RunList } from "../RunList";
import type { AgentEntry, SleepRun } from "../../../shared/api/types";

const AGENTS: AgentEntry[] = [
  { id: "lyre", label: "Lyre", is_default: true },
  { id: "ace", label: "Ace", is_default: false },
];

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
  it("renders_one_row_per_run_with_agent_label_trigger_and_relative_time", async () => {
    stubRuns([
      makeRun({ id: "run-1" }),
      makeRun({ id: "run-2", agent_id: "ace", trigger: "backfill", status: "partial_failure" }),
    ]);

    const { container } = render(
      <RunList agents={AGENTS} agentFilter="" authToken="t1" selectedRunId={null} onSelectRun={vi.fn()} />,
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".run-row")).toHaveLength(2);
    });

    const firstRow = container.querySelectorAll(".run-row")[0];
    expect(firstRow.textContent).toContain("Lyre");
    expect(firstRow.textContent).toContain("Scheduled");
    expect(firstRow.textContent).toContain("3h");
    expect(firstRow.querySelectorAll(".dot-success")).toHaveLength(1);

    const secondRow = container.querySelectorAll(".run-row")[1];
    expect(secondRow.textContent).toContain("Ace");
    expect(secondRow.textContent).toContain("Backfill");
    expect(secondRow.querySelectorAll(".dot-warning")).toHaveLength(1);
  });

  it("marks_the_selected_row_and_reports_selections", async () => {
    stubRuns([makeRun({ id: "run-1" }), makeRun({ id: "run-2" })]);
    const onSelectRun = vi.fn();

    const { container } = render(
      <RunList agents={AGENTS} agentFilter="" authToken="t2" selectedRunId="run-2" onSelectRun={onSelectRun} />,
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
      <RunList agents={AGENTS} agentFilter="" authToken="t3" selectedRunId={null} onSelectRun={vi.fn()} />,
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
      <RunList agents={AGENTS} agentFilter="" authToken="t4" selectedRunId={null} onSelectRun={vi.fn()} />,
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
