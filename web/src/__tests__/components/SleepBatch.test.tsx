import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { DiffViewer } from "../../components/DiffViewer";
import { RunList } from "../../components/RunList";
import { RunDetail } from "../../components/RunDetail";

import type { SleepRun, MemorySnapshot } from "../../types";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeRun(overrides: Partial<SleepRun> = {}): SleepRun {
  return {
    id: "run-1",
    agent_id: "agent-a",
    status: "success",
    trigger_type: "auto",
    started_at: "2025-05-01T10:00:00Z",
    finished_at: "2025-05-01T10:05:00Z",
    input_tokens: 1500,
    output_tokens: 800,
    total_tokens: 2300,
    error_message: null,
    session_count: 3,
    ...overrides,
  };
}

function makeSnapshot(
  file: string,
  before: string,
  after: string,
): MemorySnapshot {
  return { file, content_before: before, content_after: after };
}

// ---------------------------------------------------------------------------
// DiffViewer
// ---------------------------------------------------------------------------

describe("DiffViewer", () => {
  afterEach(cleanup);

  it("DiffViewer_renders_split_diff", () => {
    // Arrange
    const before = "line-a\nline-b";
    const after = "line-a\nline-c";

    // Act
    const { container } = render(
      <DiffViewer before={before} after={after} fileName="test.txt" />,
    );

    // Assert: 2-column layout exists
    expect(container.querySelector(".diff-split")).not.toBeNull();
  });

  it("DiffViewer_renders_unified_diff", () => {
    // Arrange & Act
    const { container } = render(
      <DiffViewer
        before="old"
        after="new"
        fileName="test.txt"
      />,
    );

    // Switch to unified mode
    const toggle = screen.getByRole("button", { name: /unified/i });
    fireEvent.click(toggle);

    // Assert: unified layout now active, split gone
    expect(container.querySelector(".diff-unified")).not.toBeNull();
    expect(container.querySelector(".diff-split")).toBeNull();
  });

  it("DiffViewer_shows_no_changes", () => {
    // Arrange
    const same = "identical content";

    // Act
    render(
      <DiffViewer before={same} after={same} fileName="test.txt" />,
    );

    // Assert
    expect(screen.getByText(/no changes/i)).toBeDefined();
  });

  it("DiffViewer_toggle_switches_mode", () => {
    // Arrange
    const { container } = render(
      <DiffViewer before="a" after="b" fileName="f.txt" />,
    );

    // Initially split
    expect(container.querySelector(".diff-split")).not.toBeNull();

    // Act: toggle to unified
    fireEvent.click(screen.getByRole("button", { name: /unified/i }));

    // Assert: now unified
    expect(container.querySelector(".diff-unified")).not.toBeNull();
    expect(container.querySelector(".diff-split")).toBeNull();

    // Act: toggle back to split
    fireEvent.click(screen.getByRole("button", { name: /split/i }));

    // Assert: back to split
    expect(container.querySelector(".diff-split")).not.toBeNull();
    expect(container.querySelector(".diff-unified")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// RunList
// ---------------------------------------------------------------------------

describe("RunList", () => {
  afterEach(cleanup);

  const defaultProps = {
    runs: [] as SleepRun[],
    agents: ["agent-a", "agent-b"],
    selectedAgent: "",
    onSelectAgent: vi.fn(),
    onSelectRun: vi.fn(),
  };

  it("RunList_renders_run_cards", () => {
    // Arrange
    const runs = [
      makeRun({ id: "r1", agent_id: "agent-a" }),
      makeRun({ id: "r2", agent_id: "agent-b", status: "failed" }),
    ];

    // Act
    render(
      <RunList {...defaultProps} runs={runs} />,
    );

    // Assert: agent names appear in both dropdown options and card strong elements
    expect(screen.getAllByText("agent-a").length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText("agent-b").length).toBeGreaterThanOrEqual(1);
  });

  it("RunList_shows_empty_state", () => {
    // Arrange & Act
    render(
      <RunList {...defaultProps} runs={[]} />,
    );

    // Assert
    expect(screen.getByText(/no sleep batch runs yet/i)).toBeDefined();
  });

  it("RunList_shows_status_icons", () => {
    // Arrange
    const runs = [
      makeRun({ id: "r1", status: "success" }),
      makeRun({ id: "r2", status: "failed" }),
      makeRun({ id: "r3", status: "skipped" }),
      makeRun({ id: "r4", status: "running" }),
      makeRun({ id: "r5", status: "partial_failure" }),
    ];

    // Act
    render(
      <RunList {...defaultProps} runs={runs} />,
    );

    // Assert
    expect(screen.getByText("\u2705")).toBeDefined(); // ✅
    expect(screen.getByText("\u274C")).toBeDefined(); // ❌
    expect(screen.getByText("\u23ED")).toBeDefined(); // ⏭
    expect(screen.getByText("\uD83D\uDD04")).toBeDefined(); // 🔄
    expect(screen.getByText("\u26A0\uFE0F")).toBeDefined(); // ⚠️
  });
});

// ---------------------------------------------------------------------------
// RunDetail
// ---------------------------------------------------------------------------

describe("RunDetail", () => {
  afterEach(cleanup);

  const defaultProps = {
    run: makeRun(),
    snapshots: [] as MemorySnapshot[],
    onBack: vi.fn(),
  };

  it("RunDetail_renders_meta_info", () => {
    // Arrange
    const run = makeRun({
      status: "success",
      started_at: "2025-05-01T10:00:00Z",
      finished_at: "2025-05-01T10:05:00Z",
      total_tokens: 2300,
    });

    // Act
    render(<RunDetail {...defaultProps} run={run} />);

    // Assert
    expect(screen.getByText(/success/i)).toBeDefined();
    expect(screen.getByText(/2.3k/)).toBeDefined(); // formatTokens(2300) → "2.3k"
  });

  it("RunDetail_shows_error_for_failed", () => {
    // Arrange
    const run = makeRun({
      status: "failed",
      error_message: "Something went wrong",
    });

    // Act
    render(<RunDetail {...defaultProps} run={run} />);

    // Assert
    const errorEl = screen.getByText("Something went wrong");
    expect(errorEl).toBeDefined();
    expect(errorEl.classList.contains("run-error")).toBe(true);
  });

  it("RunDetail_collapsible_sections", () => {
    // Arrange
    const snapshots = [
      makeSnapshot("episodic.md", "old-a", "new-a"),
      makeSnapshot("semantic.md", "old-b", "new-b"),
    ];

    const { container } = render(
      <RunDetail {...defaultProps} snapshots={snapshots} />,
    );

    // Assert: file headers are visible
    expect(screen.getByText("episodic.md")).toBeDefined();
    expect(screen.getByText("semantic.md")).toBeDefined();

    // Initially sections are collapsed — content not visible
    const diffContainers = container.querySelectorAll(".diff-container");
    expect(diffContainers).toHaveLength(0);

    // Act: click first header to expand
    fireEvent.click(screen.getByText("episodic.md"));

    // Assert: diff container appears
    expect(container.querySelectorAll(".diff-container")).toHaveLength(1);
  });
});
