import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { Sidebar } from "../Sidebar";

describe("Sidebar collapse", () => {
  it("sidebar_collapses_to_icon_only_bar", () => {
    const onToggle = vi.fn();
    const { container, rerender } = render(
      <Sidebar onNewSession={vi.fn()} onToggleCollapse={onToggle} collapsed={false} />,
    );

    const collapseBtn = container.querySelector(".sidebar-collapse-btn");
    expect(collapseBtn).toBeTruthy();

    const brandName = container.querySelector(".sidebar-brand-name");
    expect(brandName).toBeTruthy();

    fireEvent.click(collapseBtn!);
    expect(onToggle).toHaveBeenCalled();

    rerender(
      <Sidebar onNewSession={vi.fn()} onToggleCollapse={onToggle} collapsed={true} />,
    );

    const nav = container.querySelector(".sidebar-nav") as HTMLElement;
    expect(nav.classList.contains("collapsed")).toBe(true);

    const hiddenBrand = container.querySelector(".sidebar-brand-name");
    expect(hiddenBrand).toBeFalsy();

    const sections = container.querySelectorAll(".sidebar-section");
    expect(sections.length).toBe(0);
  });
});
