import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { AgentSectionNav } from "../AgentSectionNav";
import type { AgentProfile } from "../../../shared/api/types";

const PROFILE: AgentProfile = {
  id: "lyre",
  label: "Lyre",
  is_default: true,
  reasoning_effort: null,
  llm: [
    {
      channel: null,
      provider: "sakura",
      model: "Kimi-K2.6",
      provider_source: "agent",
      model_source: "agent",
    },
  ],
  bindings: { discord_bot: null, telegram_bot: null },
  prompt_sources: {
    soul: { scope: "agent", path: "agents/lyre/SOUL.md", chars: 2100 },
    rules: [],
    secret: { path: "agents/lyre/SECRET.md", exists: false },
    memory: { episodic: 8000, semantic: 400, prospective: 0 },
    skills: 3,
  },
};

let tokenSeq = 0;

function renderNav(section: Parameters<typeof AgentSectionNav>[0]["section"], onSelectSection = vi.fn()) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue({ ok: true, json: () => Promise.resolve({ ok: true, profile: PROFILE }) }),
  );
  render(
    <AgentSectionNav
      agentId="lyre"
      // A distinct token per render keeps the module-level query cache isolated.
      authToken={`nav-${++tokenSeq}`}
      section={section}
      onSelectSection={onSelectSection}
    />,
  );
  return onSelectSection;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("AgentSectionNav", () => {
  it("lists_sections_with_a_summary_of_each", async () => {
    // Arrange / Act
    renderNav("overview");

    // Assert
    const nav = screen.getByRole("navigation", { name: "Agent sections" });
    const titles = Array.from(nav.querySelectorAll(".context-title")).map((title) => title.textContent);
    expect(titles).toEqual(["Overview", "Soul", "Rules", "Secret", "Memory", "Skills"]);
    await waitFor(() => expect(nav.querySelectorAll(".context-sub")).toHaveLength(6));
    const summaries = Array.from(nav.querySelectorAll(".context-sub")).map((sub) => sub.textContent);
    expect(summaries).toEqual([
      "Kimi-K2.6",
      "agents/lyre/SOUL.md",
      "None",
      "Not created",
      "8.4k chars",
      "3 available",
    ]);
  });

  it("marks_the_current_section_and_reports_selection", () => {
    // Arrange
    const onSelectSection = renderNav("memory");

    // Act
    fireEvent.click(screen.getByRole("button", { name: /Skills/ }));

    // Assert
    expect(screen.getByRole("button", { current: "page" }).textContent).toContain("Memory");
    expect(onSelectSection).toHaveBeenCalledWith("skills");
  });
});
