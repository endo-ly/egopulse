import { describe, it, expect, vi } from "vitest";
import { render, fireEvent, waitFor } from "@testing-library/react";
import { RunDetail } from "../RunDetail";
import type { AgentEntry } from "../../../shared/api/types";

const AGENTS: AgentEntry[] = [{ id: "lyre", label: "Lyre", is_default: true, active: true }];

function stubDetail(detail: Record<string, unknown>) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(() =>
      Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true, ...detail }) }),
    ),
  );
}

const BASE_DETAIL = {
  run: {
    id: "run-abc123",
    agent_id: "lyre",
    status: "partial_failure",
    trigger: "scheduled",
    started_at: "2026-07-04T03:00:00.000Z",
    finished_at: "2026-07-04T03:04:00.000Z",
    source_chats_json: '[{"chat_id": 1}]',
    source_digest_md: "",
    input_tokens: 1000,
    output_tokens: 200,
    total_tokens: 1200,
    error_message: "semantic update failed: rate limited",
    session_count: 3,
  },
  snapshots: [
    {
      id: "snap-1",
      run_id: "run-abc123",
      agent_id: "lyre",
      file: "episodic",
      content_before: "# Episodic\n- old\n",
      content_after: "# Episodic\n- new\n",
      created_at: "2026-07-04T03:02:00.000Z",
    },
    {
      id: "snap-2",
      run_id: "run-abc123",
      agent_id: "lyre",
      file: "semantic",
      content_before: "# Semantic\n",
      content_after: "# Semantic\n",
      created_at: "2026-07-04T03:03:00.000Z",
    },
  ],
  steps: [
    { step: "event_extraction", status: "success", started_at: null, finished_at: null, input_tokens: 100, output_tokens: 50, error_message: null },
    { step: "semantic_update", status: "failed", started_at: null, finished_at: null, input_tokens: 10, output_tokens: 5, error_message: "rate limited" },
    { step: "prospective_update", status: "skipped", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
  ],
};

function renderDetail(runId: string) {
  return render(
    <RunDetail runId={runId} authToken="token" agents={AGENTS} onBack={vi.fn()} />,
  );
}

describe("RunDetail", () => {
  it("renders_status_meta_matrix_and_steps_side_by_side", async () => {
    stubDetail(BASE_DETAIL);
    const { container } = renderDetail("run-meta");

    await waitFor(() => {
      expect(container.querySelector(".run-meta")).toBeTruthy();
    });
    const labels = Array.from(container.querySelectorAll(".run-meta-label")).map((el) => el.textContent);
    expect(labels).toEqual(["Agent", "Trigger", "Sessions", "Started", "Finished", "Tokens"]);
    const values = Array.from(container.querySelectorAll(".run-meta-value")).map((el) => el.textContent);
    expect(values).toEqual([
      "Lyre",
      "Scheduled",
      "3",
      expect.stringContaining("Jul 4"),
      expect.stringContaining("4m 0s"),
      "1.2k",
    ]);
    // Meta matrix and steps render side by side above the diff.
    expect(container.querySelectorAll(":scope > .run-summary > *")).toHaveLength(2);
  });

  it("shows_failed_step_error_without_a_click", async () => {
    stubDetail(BASE_DETAIL);
    const { container } = renderDetail("run-error");

    await waitFor(() => {
      expect(container.querySelector(".run-step-error pre")).toBeTruthy();
    });
    expect(container.querySelector(".run-step-error pre")!.textContent).toContain("rate limited");
    // The step error is the single error surface; no separate top-level block.
    expect(container.querySelector(".run-detail > .run-error")).toBeNull();
  });

  it("shows_error_section_when_run_has_no_steps", async () => {
    stubDetail({ ...BASE_DETAIL, steps: [] });
    const { container } = renderDetail("run-no-steps");

    await waitFor(() => {
      expect(container.querySelector(".run-step-error pre")).toBeTruthy();
    });
    expect(container.querySelector(".run-step-error pre")!.textContent).toContain("rate limited");
  });

  it("renders_steps_in_order_with_status_glyphs", async () => {
    stubDetail(BASE_DETAIL);
    const { container } = renderDetail("run-steps");

    await waitFor(() => {
      expect(container.querySelectorAll(".run-step")).toHaveLength(3);
    });
    const rows = Array.from(container.querySelectorAll(".run-step"));
    expect(rows.map((row) => row.querySelector(".run-step-name")!.textContent)).toEqual([
      "Event extraction",
      "Semantic update",
      "Prospective update",
    ]);
    expect(rows[0].getAttribute("data-status")).toBe("success");
    expect(rows[1].getAttribute("data-status")).toBe("failed");
    expect(rows[2].getAttribute("data-status")).toBe("skipped");
  });

  it("defaults_to_first_changed_file_and_shows_change_counts", async () => {
    stubDetail(BASE_DETAIL);
    const { container } = renderDetail("run-files");

    await waitFor(() => {
      expect(container.querySelectorAll(".tab-switch-item")).toHaveLength(2);
    });
    const tabs = Array.from(container.querySelectorAll(".tab-switch-item"));
    expect(tabs[0].getAttribute("aria-selected")).toBe("true");
    // episodic changed: one added and one removed line.
    expect(tabs[0].querySelector(".count-add")!.textContent).toBe("+1");
    expect(tabs[0].querySelector(".count-remove")!.textContent).toBe("−1");
    expect(tabs[1].querySelector(".tab-switch-count")).toBeNull();
    expect(container.querySelector(".diff-line-add")!.textContent).toContain("new");
  });

  it("switches_files_via_tabs", async () => {
    stubDetail(BASE_DETAIL);
    const { container } = renderDetail("run-tabs");

    await waitFor(() => {
      expect(container.querySelectorAll(".tab-switch-item")).toHaveLength(2);
    });
    fireEvent.click(container.querySelectorAll(".tab-switch-item")[1]);

    await waitFor(() => {
      expect(container.querySelector(".diff-no-changes")!.textContent).toContain("semantic");
    });
  });

  it("hides_memory_section_for_skipped_runs", async () => {
    stubDetail({
      ...BASE_DETAIL,
      run: { ...BASE_DETAIL.run, status: "skipped", error_message: null },
      steps: BASE_DETAIL.steps.map((step) => ({ ...step, status: "skipped", error_message: null })),
    });
    const { container } = renderDetail("run-skipped");

    await waitFor(() => {
      expect(container.querySelector(".run-detail")).toBeTruthy();
    });
    expect(container.querySelector(".run-memory")).toBeNull();
  });

  it("shows_error_state_when_detail_is_missing", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(() =>
        Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ ok: false, error: "not_found" }) }),
      ),
    );
    const { container } = renderDetail("run-missing");

    await waitFor(() => {
      expect(container.querySelector(".run-error")).toBeTruthy();
    });
  });
});
