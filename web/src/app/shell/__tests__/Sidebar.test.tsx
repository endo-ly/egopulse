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
  it("renders_operational_tabs_as_one_nav_group", () => {
    const { container } = renderSidebar();

    const navList = container.querySelector(".sidebar-nav-list") as HTMLElement;
    const rows = Array.from(navList.querySelectorAll(".nav-row"));
    const labels = rows.map((row) => row.querySelector(".nav-row-label")?.textContent);
    expect(labels).toEqual(["Chat", "Sleep", "Pulse", "Metrics"]);

    // Enabled tabs navigate; placeholder tabs stay disabled in the same group.
    for (const row of rows) {
      const label = row.querySelector(".nav-row-label")?.textContent;
      if (label === "Chat" || label === "Sleep") {
        expect(row.hasAttribute("disabled")).toBe(false);
      } else {
        expect(row.hasAttribute("disabled")).toBe(true);
        expect(row.getAttribute("title")).toContain("coming soon");
      }
    }
  });

  it("nav_row_click_changes_tab", () => {
    const { props } = renderSidebar();

    fireEvent.click(screen.getByRole("button", { name: "Sleep" }));
    expect(props.onTabChange).toHaveBeenCalledWith("sleep");

    const chat = screen.getByRole("button", { name: "Chat" });
    expect(chat.className).toContain("active");
    expect(chat.getAttribute("aria-current")).toBe("page");
    expect(chat.hasAttribute("disabled")).toBe(false);
  });

  it("brand_search_opens_command_palette", () => {
    const { props } = renderSidebar();

    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    expect(props.onOpenPalette).toHaveBeenCalledTimes(1);
  });

  it("footer_shows_runtime_status_and_config_utility", () => {
    renderSidebar();

    expect(screen.getByText("ok")).toBeTruthy();
    expect(screen.queryByText(/turns live/)).toBeNull();
    expect(document.querySelector(".dot-live")).not.toBeNull();

    const config = screen.getByRole("button", { name: /Config/ });
    expect(config.hasAttribute("disabled")).toBe(true);
    expect(config.getAttribute("title")).toContain("coming soon");
    expect(config.className).toContain("config-btn");
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
    // Config gear stays in the collapsed footer.
    expect(screen.getByRole("button", { name: /Config/ })).toBeTruthy();
  });
});
