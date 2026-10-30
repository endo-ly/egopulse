import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, act } from "@testing-library/react";
import { App } from "../AppShell";

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
  it("desktop_renders_sidebar_and_main_without_topbar", () => {
    mockViewport(false);
    const { container } = render(<App />);

    expect(container.querySelector(".app-shell")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(container.querySelector(".main")).not.toBeNull();
    expect(container.querySelector(".topbar")).toBeNull();
    expect(container.querySelector(".sidebar")?.className).toContain("open");
  });

  it("mobile_renders_slim_topbar_and_sidebar_overlay", () => {
    mockViewport(true);
    const { container } = render(<App />);

    expect(container.querySelector(".app-shell")).not.toBeNull();
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

  it("mobile_item_tap_closes_sidebar", () => {
    mockViewport(true);
    const onSelectAgent = vi.fn();
    const onSelectSession = vi.fn();
    const onTabChange = vi.fn();
    const onNewSession = vi.fn();
    const { container } = render(
      <App
        agents={[{ id: "lyre", label: "Lyre", is_default: true }]}
        sessions={[
          {
            session_key: "chat:1",

            chat_id: 1,

            name: "Web Chat",

            channel: "web",

            agent_id: "lyre",

            last_message_preview: "hi",

            last_message_time: 1,

            archived: false,

            pinned: false,
          },
        ]}
        selectedAgent="lyre"
        selectedSession="chat:1"
        onSelectAgent={onSelectAgent}
        onSelectSession={onSelectSession}
        onTabChange={onTabChange}
        onNewSession={onNewSession}
      />,
    );
    const sidebar = () => container.querySelector(".sidebar") as HTMLElement;
    const openSidebar = () =>
      fireEvent.click(screen.getByRole("button", { name: /toggle sidebar/i }));

    // Arrange: session tap
    openSidebar();
    expect(sidebar().className).toContain("open");

    // Act
    fireEvent.click(screen.getByText("hi"));

    // Assert
    expect(onSelectSession).toHaveBeenCalledWith("chat:1");
    expect(sidebar().className).toContain("closed");

    // Arrange: tab tap
    openSidebar();

    // Act
    fireEvent.click(screen.getByRole("button", { name: "Schedule" }));

    // Assert
    expect(onTabChange).toHaveBeenCalledWith("schedule");
    expect(sidebar().className).toContain("closed");

    // Arrange: agent tap
    openSidebar();

    // Act
    fireEvent.click(screen.getByText("Lyre"));

    // Assert
    expect(onSelectAgent).toHaveBeenCalledWith("lyre");
    expect(sidebar().className).toContain("closed");

    // Arrange: new session tap
    openSidebar();

    // Act: the new-session button opens the optional name form
    fireEvent.click(screen.getByRole("button", { name: "New Session" }));
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    // Assert
    expect(onNewSession).toHaveBeenCalledTimes(1);
    expect(sidebar().className).toContain("closed");
  });

  it("mobile_escape_closes_sidebar", () => {
    mockViewport(true);
    const { container } = render(<App />);

    // Arrange
    fireEvent.click(screen.getByRole("button", { name: /toggle sidebar/i }));
    expect(container.querySelector(".sidebar")?.className).toContain("open");

    // Act
    act(() => {
      globalThis.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    // Assert
    expect(container.querySelector(".sidebar")?.className).toContain("closed");
    expect(container.querySelector(".sidebar")?.className).not.toContain("open");
  });

  it("wires_unread_sessions_to_agent_dot_and_session_row", () => {
    mockViewport(false);
    render(
      <App
        agents={[{ id: "lyre", label: "Lyre", is_default: true }]}
        sessions={[
          {
            session_key: "chat:1",

            chat_id: 1,

            name: "Web Chat",

            channel: "web",

            agent_id: "lyre",

            last_message_preview: "hi",

            last_message_time: 1,

            archived: false,

            pinned: false,
          },
          {
            session_key: "chat:2",

            chat_id: 2,

            name: "Dev",

            channel: "web",

            agent_id: "lyre",

            last_message_preview: "yo",

            last_message_time: 2,

            archived: false,

            pinned: false,
          },
        ]}
        selectedAgent="lyre"
        selectedSession="chat:1"
        unreadSessionKeys={new Set(["chat:2"])}
      />,
    );

    // Arrange + Act: only unread lights the dot.
    // Assert
    expect(
      screen.getByText("Lyre").closest(".agent-row")?.querySelector(".dot-unread"),
    ).not.toBeNull();
    expect(screen.getByText("yo").closest(".session-item")?.className).toContain(
      "unread",
    );
    expect(screen.getByText("hi").closest(".session-item")?.className).not.toContain(
      "unread",
    );
  });

  it("mobile_swipe_from_edge_opens_and_swipe_back_closes_sidebar", () => {
    mockViewport(true);
    const { container } = render(<App />);

    const sidebar = () => container.querySelector(".sidebar") as HTMLElement;

    const swipe = (startX: number, endX: number) => {
      fireEvent.touchStart(document, {
        touches: [{ clientX: startX, clientY: 300 }],
      });
      fireEvent.touchEnd(document, {
        changedTouches: [{ clientX: endX, clientY: 300 }],
      });
    };

    swipe(10, 120);
    expect(sidebar().className).toContain("open");

    swipe(300, 160);
    expect(sidebar().className).toContain("closed");
  });

  it("mobile_swipe_right_anywhere_opens_sidebar", () => {
    mockViewport(true);
    const { container } = render(<App />);

    const swipe = (startX: number, endX: number, startY = 300, endY = 300) => {
      fireEvent.touchStart(document, {
        touches: [{ clientX: startX, clientY: startY }],
      });
      fireEvent.touchEnd(document, {
        changedTouches: [{ clientX: endX, clientY: endY }],
      });
    };

    swipe(200, 320);
    expect(
      (container.querySelector(".sidebar") as HTMLElement).className,
    ).toContain("open");
  });

  it("mobile_vertical_scroll_does_not_toggle_sidebar", () => {
    mockViewport(true);
    const { container } = render(<App />);

    fireEvent.touchStart(document, {
      touches: [{ clientX: 200, clientY: 200 }],
    });
    fireEvent.touchEnd(document, {
      changedTouches: [{ clientX: 280, clientY: 380 }],
    });

    expect(container.querySelector(".sidebar")?.className).toContain("closed");
  });

  it("app_wires_sidebar_navigation_and_sections_together", () => {
    mockViewport(false);
    const onSelectAgent = vi.fn();
    const onSelectSession = vi.fn();
    const onNewSession = vi.fn();
    const onOpenPalette = vi.fn();

    render(
      <App
        agents={[
          { id: "lyre", label: "Lyre", is_default: true },
        ]}
        sessions={[
          {
            session_key: "chat:1",

            chat_id: 1,

            name: "Web Chat",

            channel: "web",

            agent_id: "lyre",

            last_message_preview: "hi",

            last_message_time: 1,

            archived: false,

            pinned: false,
          },
        ]}
        selectedAgent="lyre"
        selectedSession="chat:1"
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
    // Named sessions show their display name; unnamed ones fall back to the preview.
    expect(screen.getByText("Web Chat")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Chat" }).className).toContain(
      "active",
    );

    fireEvent.click(screen.getByText("Web Chat"));
    expect(onSelectSession).toHaveBeenCalledWith("chat:1");

    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    expect(onOpenPalette).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "New Session" }));
    fireEvent.change(screen.getByLabelText("New session name"), {
      target: { value: "設計相談" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    expect(onNewSession).toHaveBeenCalledWith("設計相談");
  });

  it("context_section_follows_the_active_screen", () => {
    // Arrange
    mockViewport(false);
    const onSelectAgentSection = vi.fn();
    const props = {
      agents: [{ id: "lyre", label: "Lyre", is_default: true }],
      sessions: [],
      selectedAgent: "lyre",
      agentSection: "overview" as const,
      onSelectAgentSection,
    };

    // Act
    const { rerender } = render(<App {...props} activeTab="chat" />);

    // Assert: chat shows sessions
    expect(screen.getByText("SESSIONS")).toBeTruthy();
    expect(screen.queryByRole("navigation", { name: "Agent sections" })).toBeNull();

    // Act
    rerender(<App {...props} activeTab="agent" />);
    fireEvent.click(screen.getByRole("button", { name: /Soul/ }));

    // Assert: agent shows its section list
    expect(screen.queryByText("SESSIONS")).toBeNull();
    expect(screen.getByRole("navigation", { name: "Agent sections" })).toBeTruthy();
    expect(onSelectAgentSection).toHaveBeenCalledWith("soul");

    // Act
    rerender(<App {...props} activeTab="schedule" />);

    // Assert: other screens have no context section yet
    expect(screen.queryByText("SESSIONS")).toBeNull();
    expect(screen.queryByRole("navigation", { name: "Agent sections" })).toBeNull();
  });
});
