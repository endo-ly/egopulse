import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { Sidebar } from "../Sidebar";

describe("Sidebar collapse", () => {
  const baseProps = {
    activeTab: "chat" as const,
    onTabChange: vi.fn(),
    onOpenPalette: vi.fn(),
  };

  it("sidebar_collapses_to_icon_only_bar", () => {
    const onToggle = vi.fn();
    const { container, rerender } = render(
      <Sidebar
        {...baseProps}
        onToggleCollapse={onToggle}
        collapsed={false}
      />,
    );

    const collapseBtn = container.querySelector(".sidebar-collapse-btn");
    expect(collapseBtn).toBeTruthy();

    const brandName = container.querySelector(".sidebar-brand-name");
    expect(brandName).toBeTruthy();

    fireEvent.click(collapseBtn!);
    expect(onToggle).toHaveBeenCalled();

    rerender(
      <Sidebar
        {...baseProps}
        onToggleCollapse={onToggle}
        collapsed={true}
      />,
    );

    const nav = container.querySelector(".sidebar-nav") as HTMLElement;
    expect(nav.classList.contains("collapsed")).toBe(true);

    const hiddenBrand = container.querySelector(".sidebar-brand-name");
    expect(hiddenBrand).toBeFalsy();

    const body = container.querySelectorAll(".sidebar-body");
    expect(body.length).toBe(0);
  });
});
