import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { AgentsSection } from "../app/shell/AgentsSection";
import type { AgentEntry } from "../shared/api/types";

const AGENTS: AgentEntry[] = [
  { id: "lyre", label: "Lyre", is_default: true, active: true },
  { id: "ace", label: "Ace", is_default: false, active: false },
];

describe("AgentsSection", () => {
  it("agents_section_renders_list_and_active_state", () => {
    const onSelectAgent = vi.fn();
    render(
      <AgentsSection
        agents={AGENTS}
        selectedAgent="lyre"
        onSelectAgent={onSelectAgent}
      />,
    );

    const lyreRow = screen.getByText("Lyre").closest(".agent-row");
    const aceRow = screen.getByText("Ace").closest(".agent-row");

    expect(lyreRow).not.toBeNull();
    expect(aceRow).not.toBeNull();
    expect(lyreRow?.className).toContain("active");
    expect(aceRow?.className).not.toContain("active");

    expect(lyreRow?.querySelector(".dot-live")).not.toBeNull();
    expect(aceRow?.querySelector(".dot-idle")).not.toBeNull();

    expect(lyreRow?.querySelector(".agent-default-tag")?.textContent).toBe(
      "default",
    );
    expect(aceRow?.querySelector(".agent-default-tag")).toBeNull();

    fireEvent.click(aceRow as HTMLElement);
    expect(onSelectAgent).toHaveBeenCalledWith("ace");
    cleanup();
  });
});
