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
        onOpenPalette={vi.fn()}
        onToggleSidebar={onToggleSidebar}
        sidebarOpen={false}
        healthStatus="ok"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /toggle sidebar/i }));
    expect(onToggleSidebar).toHaveBeenCalledTimes(1);
  });

  it("palette_button_opens_command_palette", () => {
    const onOpenPalette = vi.fn();
    render(
      <MobileBar
        onOpenPalette={onOpenPalette}
        onToggleSidebar={vi.fn()}
        sidebarOpen={false}
        healthStatus="ok"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    expect(onOpenPalette).toHaveBeenCalledTimes(1);
  });

  it("has_no_primary_navigation_select", () => {
    render(
      <MobileBar
        onOpenPalette={vi.fn()}
        onToggleSidebar={vi.fn()}
        sidebarOpen={false}
        healthStatus="ok"
      />,
    );

    expect(screen.queryByLabelText("Primary navigation")).toBeNull();
  });
});
