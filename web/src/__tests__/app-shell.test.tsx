import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { App } from "../components/App";

function mockViewport(mobile: boolean): void {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    configurable: true,
    value: vi.fn().mockImplementation((query: string) => ({
      matches: mobile && query === "(max-width: 639px)",
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  });
}

afterEach(() => {
  cleanup();
});

describe("App shell", () => {
  it("app_shell_renders_three_regions_on_desktop", () => {
    mockViewport(false);
    const { container } = render(<App />);

    expect(container.querySelector(".app-shell")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(container.querySelector(".topbar")).not.toBeNull();
    expect(container.querySelector(".main")).not.toBeNull();
    expect(container.querySelector(".sidebar")?.className).toContain("open");
  });

  it("app_shell_renders_three_regions_and_mobile_overlay", () => {
    mockViewport(true);
    const { container } = render(<App />);

    expect(container.querySelector(".app-shell")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(container.querySelector(".topbar")).not.toBeNull();
    expect(container.querySelector(".main")).not.toBeNull();

    const sidebar = container.querySelector(".sidebar");
    expect(sidebar?.className).toContain("closed");
    expect(sidebar?.className).not.toContain("open");

    expect(screen.queryByRole("button", { name: /toggle sidebar/i })).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /toggle sidebar/i }));

    expect(container.querySelector(".sidebar")?.className).toContain("open");
    expect(container.querySelector(".sidebar-backdrop")).not.toBeNull();

    fireEvent.click(container.querySelector(".sidebar-backdrop") as HTMLElement);
    expect(container.querySelector(".sidebar")?.className).toContain("closed");
  });
});
