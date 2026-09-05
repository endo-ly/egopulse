import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { MobileBar } from "../MobileBar";

afterEach(() => {
  cleanup();
});

describe("MobileBar", () => {
  it("hamburger_toggles_sidebar", () => {
    const onToggleSidebar = vi.fn();
    render(
      <MobileBar
        activeTab="chat"
        onTabChange={vi.fn()}
        onOpenPalette={vi.fn()}
        onToggleSidebar={onToggleSidebar}
        sidebarOpen={false}
        healthStatus="ok"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /toggle sidebar/i }));
    expect(onToggleSidebar).toHaveBeenCalledTimes(1);
  });

  it("tab_select_changes_active_tab", () => {
    const onTabChange = vi.fn();
    render(
      <MobileBar
        activeTab="chat"
        onTabChange={onTabChange}
        onOpenPalette={vi.fn()}
        onToggleSidebar={vi.fn()}
        sidebarOpen={false}
        healthStatus="ok"
      />,
    );

    fireEvent.change(screen.getByLabelText("Primary navigation"), {
      target: { value: "sleep" },
    });
    expect(onTabChange).toHaveBeenCalledWith("sleep");

    const pulseOption = screen.getByRole("option", { name: "Pulse" });
    expect((pulseOption as HTMLOptionElement).disabled).toBe(true);
  });

  it("palette_button_opens_command_palette", () => {
    const onOpenPalette = vi.fn();
    render(
      <MobileBar
        activeTab="chat"
        onTabChange={vi.fn()}
        onOpenPalette={onOpenPalette}
        onToggleSidebar={vi.fn()}
        sidebarOpen={false}
        healthStatus="ok"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    expect(onOpenPalette).toHaveBeenCalledTimes(1);
  });
});
