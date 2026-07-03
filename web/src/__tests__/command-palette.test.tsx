import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { CommandPalette } from "../app/command-palette/CommandPalette";
import type { AgentEntry, SessionEntry } from "../shared/api/types";

const noop = () => {};

const agents: AgentEntry[] = [
  { id: "lyre", label: "Lyre", is_default: true, active: false },
  { id: "ace", label: "Ace", is_default: false, active: true },
];

const sessions: SessionEntry[] = [
  { session_key: "web:main", label: "Web Chat", channel: "web", agent_id: "lyre", last_message_time: 0, last_message_preview: "hello" },
];

describe("CommandPalette", () => {
  it("command_palette_opens_and_shows_input", () => {
    const { container } = render(
      <CommandPalette
        open={true}
        onClose={noop}
        agents={agents}
        sessions={sessions}
        selectedAgent="lyre"
        onNavigate={noop}
        onSelectAgent={noop}
        onSelectSession={noop}
        onNewSession={noop}
        onRefresh={noop}
      />,
    );

    const overlay = container.querySelector(".palette-overlay");
    expect(overlay).toBeTruthy();

    const input = container.querySelector(".palette-input") as HTMLInputElement;
    expect(input).toBeTruthy();
  });

  it("command_palette_escape_closes", () => {
    const onClose = vi.fn();
    render(
      <CommandPalette
        open={true}
        onClose={onClose}
        agents={agents}
        sessions={sessions}
        selectedAgent="lyre"
        onNavigate={noop}
        onSelectAgent={noop}
        onSelectSession={noop}
        onNewSession={noop}
        onRefresh={noop}
      />,
    );

    globalThis.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(onClose).toHaveBeenCalled();
  });

  it("command_palette_backdrop_click_closes", () => {
    const onClose = vi.fn();
    const { container } = render(
      <CommandPalette
        open={true}
        onClose={onClose}
        agents={agents}
        sessions={sessions}
        selectedAgent="lyre"
        onNavigate={noop}
        onSelectAgent={noop}
        onSelectSession={noop}
        onNewSession={noop}
        onRefresh={noop}
      />,
    );

    const overlay = container.querySelector(".palette-overlay") as HTMLElement;
    fireEvent.click(overlay);
    expect(onClose).toHaveBeenCalled();
  });

  it("command_palette_renders_all_sections", () => {
    const { container } = render(
      <CommandPalette
        open={true}
        onClose={noop}
        agents={agents}
        sessions={sessions}
        selectedAgent="lyre"
        onNavigate={noop}
        onSelectAgent={noop}
        onSelectSession={noop}
        onNewSession={noop}
        onRefresh={noop}
      />,
    );

    const sectionTitles = container.querySelectorAll(".palette-section-title");
    const titles = Array.from(sectionTitles).map((t) => t.textContent);
    expect(titles).toContain("Quick Actions");
    expect(titles).toContain("Navigation");
    expect(titles).toContain("Agents");
    expect(titles).toContain("Sessions");
    expect(titles).toContain("Sleep & Pulse Runs");

    const agentItems = container.querySelectorAll(".palette-item");
    const labels = Array.from(agentItems).map((i) => i.textContent?.trim());
    expect(labels?.some((l) => l?.includes("New Session"))).toBe(true);
    expect(labels?.some((l) => l?.includes("Ace"))).toBe(true);
    expect(labels?.some((l) => l?.includes("Web Chat"))).toBe(true);
  });

  it("palette_recent_reads_from_localstorage", () => {
    localStorage.setItem(
      "egopulse.paletteHistory",
      JSON.stringify([
        { id: "nav-chat", label: "Go to Chat", section: "Navigation" },
        { id: "qa-refresh", label: "Refresh current tab", section: "Quick Actions" },
      ]),
    );

    const { container } = render(
      <CommandPalette
        open={true}
        onClose={noop}
        agents={agents}
        sessions={sessions}
        selectedAgent="lyre"
        onNavigate={noop}
        onSelectAgent={noop}
        onSelectSession={noop}
        onNewSession={noop}
        onRefresh={noop}
      />,
    );

    const sectionTitles = container.querySelectorAll(".palette-section-title");
    const titles = Array.from(sectionTitles).map((t) => t.textContent);
    expect(titles).toContain("Recent");

    const recentSection = Array.from(sectionTitles).find((t) => t.textContent === "Recent");
    const recentItems = recentSection?.parentElement?.querySelectorAll(".palette-item");
    expect(recentItems?.length).toBe(2);
  });

  it("command_palette_filter_narrows_results", () => {
    const { container } = render(
      <CommandPalette
        open={true}
        onClose={noop}
        agents={agents}
        sessions={sessions}
        selectedAgent="lyre"
        onNavigate={noop}
        onSelectAgent={noop}
        onSelectSession={noop}
        onNewSession={noop}
        onRefresh={noop}
      />,
    );

    const input = container.querySelector(".palette-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "Ace" } });

    const items = container.querySelectorAll(".palette-item");
    const labels = Array.from(items).map((i) => i.textContent?.trim());
    expect(labels?.some((l) => l?.includes("Ace"))).toBe(true);
    expect(labels?.some((l) => l?.includes("New Session"))).toBe(false);
  });
});
