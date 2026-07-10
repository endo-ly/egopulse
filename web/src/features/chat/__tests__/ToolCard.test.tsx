import { describe, it, expect } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { ToolCard } from "../ToolCard";
import type { ToolEventData } from "../../../shared/api/types";

function evt(overrides: Partial<ToolEventData>): ToolEventData {
  return {
    name: "read",
    state: "success",
    input: { path: "file.ts" },
    output: "file contents",
    duration_ms: 120,
    ...overrides,
  };
}

describe("ToolCard", () => {
  it("tool_card_pending_shows_running_and_spinner", () => {
    const { container } = render(
      <ToolCard event={evt({ state: "pending", name: "read", input: { path: "a.ts" } })} />,
    );
    const summary = container.querySelector(".tool-card-summary");
    expect(summary?.textContent).toContain("running");
    const spinner = container.querySelector(".spinner");
    expect(spinner).toBeTruthy();
  });

  it("tool_card_success_shows_summary_and_duration", () => {
    const { container } = render(
      <ToolCard event={evt({ state: "success", duration_ms: 120 })} />,
    );
    const summary = container.querySelector(".tool-card-summary");
    expect(summary?.textContent).toContain("read");
    expect(summary?.textContent).toContain("file.ts");
    const duration = container.querySelector(".tool-card-duration");
    expect(duration?.textContent).toBe("120ms");
  });

  it("tool_card_initial_error_auto_expands", () => {
    const { container } = render(
      <ToolCard event={evt({ state: "error", output: "Error: something failed badly" })} />,
    );
    const header = container.querySelector(".tool-card-header") as HTMLElement;
    expect(header.getAttribute("aria-expanded")).toBe("true");
    const errBadge = container.querySelector(".tool-card-error-badge");
    expect(errBadge).toBeTruthy();
  });

  it("tool_card_error_transition_auto_expands", () => {
    const { container, rerender } = render(
      <ToolCard event={evt({ state: "pending" })} defaultExpanded={false} />,
    );
    const header = container.querySelector(".tool-card-header") as HTMLElement;
    expect(header.getAttribute("aria-expanded")).toBe("false");

    rerender(<ToolCard event={evt({ state: "error" })} defaultExpanded={false} />);

    expect(header.getAttribute("aria-expanded")).toBe("true");
  });

  it("tool_card_can_be_collapsed_after_error_auto_expand", () => {
    const { container } = render(<ToolCard event={evt({ state: "error" })} />);
    const header = container.querySelector(".tool-card-header") as HTMLElement;
    expect(header.getAttribute("aria-expanded")).toBe("true");

    fireEvent.click(header);

    expect(header.getAttribute("aria-expanded")).toBe("false");
  });

  it("tool_card_header_toggles_expansion", () => {
    const { container } = render(
      <ToolCard event={evt({ state: "success" })} defaultExpanded={false} />,
    );
    const header = container.querySelector(".tool-card-header") as HTMLElement;
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".tool-card-body")).toBeFalsy();

    fireEvent.click(header);

    const headerAfter = container.querySelector(".tool-card-header") as HTMLElement;
    expect(headerAfter.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector(".tool-card-body")).toBeTruthy();
  });
});
