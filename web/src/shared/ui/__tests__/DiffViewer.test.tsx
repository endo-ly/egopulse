import { describe, it, expect } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { DiffViewer } from "../DiffViewer";

function lines(count: number, prefix: string): string {
  return Array.from({ length: count }, (_, i) => `${prefix} ${i}`).join("\n");
}

/** jsdom has no matchMedia, so the viewer defaults to split; tests opt into unified. */
function switchToUnified(container: HTMLElement) {
  fireEvent.click(Array.from(container.querySelectorAll(".diff-toolbar button")).find(
    (button) => button.textContent === "Unified",
  )!);
}

describe("DiffViewer", () => {
  it("renders_split_columns_by_default", () => {
    const { container } = render(
      <DiffViewer before={"a\nb\nc"} after={"a\nB\nc"} fileName="episodic" />,
    );
    expect(container.querySelectorAll(".diff-column")).toHaveLength(2);
    expect(container.querySelector(".diff-line-remove")!.textContent).toContain("b");
    expect(container.querySelector(".diff-line-add")!.textContent).toContain("B");
  });

  it("renders_prefixed_lines_in_unified_mode", () => {
    const { container } = render(
      <DiffViewer before={"a\nb\nc"} after={"a\nB\nc"} fileName="episodic" />,
    );
    switchToUnified(container);
    expect(container.querySelector(".diff-unified")).toBeTruthy();
    expect(container.querySelector(".diff-line-remove")!.textContent).toContain("- b");
    expect(container.querySelector(".diff-line-add")!.textContent).toContain("+ B");
  });

  it("reports_no_changes_for_identical_content", () => {
    const { container } = render(
      <DiffViewer before={"a\nb"} after={"a\nb"} fileName="semantic" />,
    );
    expect(container.querySelector(".diff-no-changes")!.textContent).toContain("semantic");
    expect(container.querySelector(".diff-container")).toBeNull();
  });

  it("caps_long_diffs_behind_a_show_all_button", () => {
    const before = lines(600, "same");
    const after = `${lines(600, "same")}\nextra`;
    const { container } = render(
      <DiffViewer before={before} after={after} fileName="prospective" />,
    );
    switchToUnified(container);

    const rendered = container.querySelectorAll(".diff-unified > div").length;
    expect(rendered).toBe(500);
    const button = container.querySelector(".diff-show-all")!;
    expect(button.textContent).toContain("601");

    fireEvent.click(button);
    // The button disappears and the remaining line renders.
    expect(container.querySelector(".diff-show-all")).toBeNull();
    expect(container.querySelectorAll(".diff-unified > div").length).toBe(601);
  });
});
