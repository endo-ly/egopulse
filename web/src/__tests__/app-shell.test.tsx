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

  it("app_wires_sidebar_topbar_and_sections_together", () => {
    mockViewport(false);
    const onSelectAgent = vi.fn();
    const onSelectSession = vi.fn();
    const onNewSession = vi.fn();
    const onOpenPalette = vi.fn();

    render(
      <App
        agents={[
          { id: "lyre", label: "Lyre", is_default: true, active: true },
        ]}
        sessions={[
          {
            session_key: "s1",
            label: "Web Chat",
            channel: "web",
            agent_id: "lyre",
            last_message_preview: "hi",
            last_message_time: 1,
          },
        ]}
        selectedAgent="lyre"
        selectedSession="s1"
        onSelectAgent={onSelectAgent}
        onSelectSession={onSelectSession}
        onNewSession={onNewSession}
        onOpenPalette={onOpenPalette}
      />,
    );

    expect(screen.getByText("EgoPulse")).toBeTruthy();
    expect(screen.getByText("AGENTS")).toBeTruthy();
    expect(screen.getByText("SESSIONS")).toBeTruthy();
    expect(screen.getByText("Lyre")).toBeTruthy();
    expect(screen.getByText("Web Chat")).toBeTruthy();
    expect(screen.getByText("Chat").closest(".tab")?.className).toContain(
      "active",
    );

    fireEvent.click(screen.getByText("Web Chat"));
    expect(onSelectSession).toHaveBeenCalledWith("s1");

    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    expect(onOpenPalette).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByText("New Session"));
    expect(onNewSession).toHaveBeenCalledTimes(1);
  });
});
