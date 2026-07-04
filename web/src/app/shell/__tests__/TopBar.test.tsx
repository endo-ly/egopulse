import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { TopBar } from "../TopBar";

describe("TopBar", () => {
  it("topbar_renders_palette_trigger_tabs_and_health", () => {
    const onOpenPalette = vi.fn();
    const onTabChange = vi.fn();
    render(
      <TopBar
        activeTab="chat"
        onTabChange={onTabChange}
        onOpenPalette={onOpenPalette}
        healthStatus="ok"
      />,
    );

    const palette = screen.getByRole("button", { name: /open command palette/i });
    expect(palette).toBeTruthy();
    fireEvent.click(palette);
    expect(onOpenPalette).toHaveBeenCalledTimes(1);

    const labels = ["Chat", "Sleep", "Pulse", "Metrics", "Config"];
    for (const label of labels) {
      expect(screen.getByRole("button", { name: label })).toBeTruthy();
    }

    const chatTab = screen.getByRole("button", { name: "Chat" });
    expect(chatTab.className).toContain("active");
    expect(chatTab.getAttribute("aria-current")).toBe("page");

    for (const label of ["Pulse", "Metrics", "Config"]) {
      const tab = screen.getByRole("button", { name: label });
      expect(tab.hasAttribute("disabled")).toBe(true);
    }

    const health = document.querySelector(".health-badge");
    expect(health).not.toBeNull();
    expect(health?.querySelector(".dot-live")).not.toBeNull();
    expect(health?.textContent).toContain("ok");
    cleanup();
  });

  it("topbar_disabled_tab_does_not_call_on_tab_change", () => {
    const onTabChange = vi.fn();
    render(
      <TopBar
        activeTab="chat"
        onTabChange={onTabChange}
        onOpenPalette={() => {}}
        healthStatus="ok"
      />,
    );
    const pulseTab = screen.getByRole("button", { name: "Pulse" });
    fireEvent.click(pulseTab);
    expect(onTabChange).not.toHaveBeenCalled();
  });
});
