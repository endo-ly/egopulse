import { describe, it, expect, vi } from "vitest";
import { render, fireEvent, waitFor } from "@testing-library/react";
import { SleepRunBody } from "../SleepRunBody";

function stubDetail(detail: Record<string, unknown>) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(() =>
      Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true, ...detail }) }),
    ),
  );
}

const RUN = {
  id: "run-abc123",
  agent_id: "lyre",
  status: "partial_failure",
  trigger: "scheduled",
  started_at: "2026-07-04T03:00:00.000Z",
  finished_at: "2026-07-04T03:04:00.000Z",
  source_chats_json: "[]",
  source_digest_md: null,
  input_tokens: 1000,
  output_tokens: 200,
  total_tokens: 1200,
  error_message: "semantic update failed: rate limited",
  session_count: 3,
  changed_memory: ["episodic", "prospective"],
};

function snapshot(file: string, before: string, after: string) {
  return { id: `snap-${file}`, run_id: RUN.id, agent_id: "lyre", file, content_before: before, content_after: after, created_at: RUN.started_at };
}

function step(name: string, status: string, error: string | null = null) {
  return { step: name, status, started_at: null, finished_at: null, input_tokens: 100, output_tokens: 50, error_message: error };
}

const DETAIL = {
  run: RUN,
  snapshots: [
    snapshot("episodic", "# Episodic\n- old\n", "# Episodic\n- new\n"),
    snapshot("semantic", "# Semantic\n", "# Semantic\n"),
    snapshot("prospective", "- a\n", "- a\n- b\n"),
  ],
  steps: [
    step("event_extraction", "success"),
    step("episodic_update", "success"),
    step("semantic_update", "failed", "rate limited"),
    step("prospective_update", "skipped"),
  ],
};

describe("SleepRunBody", () => {
  it("summarizes_the_run_beside_its_steps_in_pipeline_order_with_failures", async () => {
    // Arrange
    stubDetail(DETAIL);

    // Act
    const { container } = render(<SleepRunBody runId="run-pipeline" authToken="b1" />);

    // Assert
    await waitFor(() => {
      expect(container.querySelectorAll(".pipeline-step")).toHaveLength(4);
    });
    const facts = container.querySelector(".sleep-run-facts")!;
    // The duration follows the start time; step outcomes are the pipeline rows.
    expect(Array.from(facts.querySelectorAll("dt")).map((el) => el.textContent)).toEqual([
      "Tokens",
      "Sessions",
      "Started",
      "Took",
      "Run",
    ]);
    expect(facts.textContent).toContain("1.2k");
    expect(facts.textContent).toContain("4m 0s");
    expect(container.querySelector(".run-detail-title")!.textContent).toBe("Sleep");
    const steps = Array.from(container.querySelectorAll(".pipeline-step"));
    expect(steps.map((el) => el.getAttribute("data-status"))).toEqual(["success", "success", "failed", "skipped"]);
    expect(steps.map((el) => el.querySelector(".pipeline-step-icon")!.textContent)).toEqual(["✓", "✓", "✕", "–"]);
    expect(steps[0].textContent).toContain("150");
    expect(container.querySelector(".pipeline-failure")!.textContent).toContain("rate limited");
  });

  it("lists_only_changed_files_and_switches_the_diff", async () => {
    // Arrange
    stubDetail(DETAIL);
    const { container } = render(<SleepRunBody runId="run-files" authToken="b2" />);
    await waitFor(() => {
      expect(container.querySelectorAll(".file-chip")).toHaveLength(2);
    });
    const chips = Array.from(container.querySelectorAll(".file-chip"));

    // Act
    fireEvent.click(chips[1]);

    // Assert
    expect(chips.map((chip) => chip.firstChild!.textContent)).toEqual(["episodic", "prospective"]);
    expect(chips[0].querySelector(".count-add")!.textContent).toBe("+1");
    await waitFor(() => {
      expect(chips[1].getAttribute("aria-pressed")).toBe("true");
    });
    expect(container.querySelector(".diff-line-add")!.textContent).toContain("b");
  });

  it("shows_the_run_error_when_no_step_ran", async () => {
    // Arrange
    stubDetail({ ...DETAIL, run: { ...RUN, status: "failed" }, steps: [], snapshots: [] });

    // Act
    const { container } = render(<SleepRunBody runId="run-early" authToken="b3" />);

    // Assert
    await waitFor(() => {
      expect(container.querySelector(".row-error")).toBeTruthy();
    });
    expect(container.querySelector(".row-error")!.textContent).toContain("rate limited");
  });

  it("keeps_every_step_listed_with_its_tokens_when_the_run_succeeded", async () => {
    // Arrange
    const steps = DETAIL.steps.map((entry) => ({ ...entry, status: "success", error_message: null }));
    stubDetail({ ...DETAIL, run: { ...RUN, status: "success", error_message: null }, steps });

    // Act
    const { container } = render(<SleepRunBody runId="run-done" authToken="b4" />);

    // Assert
    await waitFor(() => {
      expect(container.querySelectorAll(".pipeline-step")).toHaveLength(4);
    });
    const rows = Array.from(container.querySelectorAll(".pipeline-step"));
    expect(rows.map((el) => el.querySelector(".pipeline-step-name")!.textContent)).toEqual([
      "Event extraction",
      "Episodic update",
      "Semantic update",
      "Prospective update",
    ]);
    expect(rows.map((el) => el.querySelector(".pipeline-step-meta")!.textContent)).toEqual([
      "150",
      "150",
      "150",
      "150",
    ]);
    expect(container.querySelectorAll(".file-chip")).toHaveLength(2);
  });

  it("says_why_a_skipped_check_did_not_run", async () => {
    // Arrange
    stubDetail({
      run: { ...RUN, status: "skipped", error_message: "3 new messages; runs when there are more than 64" },
      steps: [],
      snapshots: [],
    });

    // Act
    const { container } = render(<SleepRunBody runId="run-skip" authToken="b5" />);

    // Assert
    await waitFor(() => {
      expect(container.textContent).toContain(
        "Did not run: 3 new messages; runs when there are more than 64.",
      );
    });
    expect(container.querySelector("[aria-label='Memory changes']")).toBeNull();
  });

  it("does_not_present_unpublished_candidates_as_memory_changes", async () => {
    // Arrange
    stubDetail({ ...DETAIL, run: { ...RUN, status: "running", error_message: null } });

    // Act
    const { container } = render(<SleepRunBody runId="run-live" authToken="b6" />);

    // Assert
    await waitFor(() => {
      expect(container.querySelector(".pipeline")).toBeTruthy();
    });
    expect(container.querySelector("[aria-label='Memory changes']")).toBeNull();
  });
});
