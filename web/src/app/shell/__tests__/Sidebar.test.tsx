import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Sidebar } from "../Sidebar";

function renderSidebar(overrides = {}) {
  const props = {
    activeTab: "chat" as const,
    onTabChange: vi.fn(),
    onOpenPalette: vi.fn(),
    healthStatus: "ok" as const,
    ...overrides,
  };
  const utils = render(<Sidebar {...props} />);
  return { ...utils, props };
}

afterEach(() => {
  cleanup();
});

describe("Sidebar", () => {
  it("renders_agent_screens_as_one_nav_group", () => {
    // Arrange / Act
    const { container } = renderSidebar();

    // Assert
    const navList = container.querySelector(".sidebar-nav-list") as HTMLElement;
    const rows = Array.from(navList.querySelectorAll(".nav-row"));
    const labels = rows.map((row) => row.querySelector(".nav-row-label")?.textContent);
    expect(labels).toEqual(["Chat", "Agent", "Schedule"]);
    for (const row of rows) {
      expect(row.hasAttribute("disabled")).toBe(false);
    }
  });

  it("nav_row_click_changes_tab", () => {
    // Arrange
    const { props } = renderSidebar();

    // Act
    fireEvent.click(screen.getByRole("button", { name: "Agent" }));

    // Assert
    expect(props.onTabChange).toHaveBeenCalledWith("agent");
    const chat = screen.getByRole("button", { name: "Chat" });
    expect(chat.className).toContain("active");
    expect(chat.getAttribute("aria-current")).toBe("page");
  });

  it("renders_the_context_section_below_agents", () => {
    // Arrange / Act
    renderSidebar({
      agents: <div>agents-slot</div>,
      context: <div>context-slot</div>,
    });

    // Assert
    const body = document.querySelector(".sidebar-body") as HTMLElement;
    expect(body.textContent).toBe("agents-slotcontext-slot");
  });

  it("brand_search_opens_command_palette", () => {
    const { props } = renderSidebar();

    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    expect(props.onOpenPalette).toHaveBeenCalledTimes(1);
  });

  it("footer_shows_runtime_status_and_global_screens", () => {
    // Arrange / Act
    renderSidebar();

    // Assert
    expect(screen.getByText("ok")).toBeTruthy();
    expect(document.querySelector(".dot-live")).not.toBeNull();
    const footer = document.querySelector(".sidebar-footer") as HTMLElement;
    const globals = Array.from(footer.querySelectorAll("button.footer-tab"));
    expect(globals.map((button) => button.textContent)).toEqual(["Metrics", "Config"]);
    for (const button of globals) {
      expect(button.hasAttribute("disabled")).toBe(true);
      expect(button.getAttribute("title")).toContain("coming soon");
    }
  });

  it("footer_marks_the_active_global_screen", () => {
    // Arrange / Act
    renderSidebar({ activeTab: "metrics" });

    // Assert
    const metrics = screen.getByRole("button", { name: "Metrics" });
    expect(metrics.getAttribute("aria-current")).toBe("page");
  });

  it("collapsed_mode_hides_labels_and_shows_brand_mark", () => {
    const { container } = renderSidebar({ collapsed: true, onToggleCollapse: vi.fn() });

    expect(screen.queryByText("EgoPulse")).toBeNull();
    expect(screen.getByText("E")).toBeTruthy();
    // The nav collapses to a vertical icon rail; labels are CSS-hidden and the
    // rows keep an accessible name via their title tooltip.
    expect(container.querySelector(".sidebar-nav")?.className).toContain("collapsed");
    expect(document.querySelector(".nav-row[title='Chat']")).not.toBeNull();
    // Search collapses into the palette shortcut only; ⌘K remains available.
    expect(
      screen.queryByRole("button", { name: /open command palette/i }),
    ).toBeNull();
    // Global screens stay in the collapsed footer.
    expect(screen.getByRole("button", { name: "Metrics" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Config" })).toBeTruthy();
  });
});
