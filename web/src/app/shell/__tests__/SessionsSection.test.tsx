import { afterEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup, within } from "@testing-library/react";
import { SessionsSection } from "../SessionsSection";
import type { SessionEntry } from "../../../shared/api/types";

const SESSIONS: SessionEntry[] = [
  {
    session_key: "chat:1",
    chat_id: 1,
    name: "Web Chat",
    channel: "web",
    agent_id: "lyre",
    last_message_preview: "hello world",
    last_message_time: 100,
    archived: false,
    pinned: false,
  },
  {
    session_key: "chat:2",
    chat_id: 2,
    name: "Dev",
    channel: "discord",
    agent_id: "lyre",
    last_message_preview: "review code",
    last_message_time: 300,
    archived: false,
    pinned: false,
  },
  {
    session_key: "chat:3",
    chat_id: 3,
    name: "Notes",
    channel: "cli",
    agent_id: "ace",
    last_message_preview: "quick note",
    last_message_time: 200,
    archived: false,
    pinned: false,
  },
];

afterEach(() => {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: undefined,
  });
  Object.defineProperty(document, "execCommand", {
    configurable: true,
    value: undefined,
  });
  vi.useRealTimers();
  cleanup();
});

describe("SessionsSection", () => {
  it("sessions_section_renders_list_with_channel_and_agent_filter", () => {
    const onSelectSession = vi.fn();
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={onSelectSession}
      />,
    );

    expect(screen.queryByText("Notes")).toBeNull();

    const list = document.querySelector(".sessions-list") as HTMLElement;
    const titles = within(list)
      .getAllByText(/Web Chat|Dev/)
      .map((el) => el.textContent);
    // The list keeps the server order so pinned sessions stay on top.
    expect(titles).toEqual(["Web Chat", "Dev"]);

    const filter = screen.getByLabelText("Filter sessions by channel");
    fireEvent.change(filter, { target: { value: "web" } });

    expect(screen.queryByText("Dev")).toBeNull();
    expect(screen.queryByText("Web Chat")).not.toBeNull();

    fireEvent.click(screen.getByText("Web Chat"));
    expect(onSelectSession).toHaveBeenCalledWith("chat:1");
    cleanup();
  });

  it("sessions_section_shows_empty_state_for_empty_filtered_results", () => {
    const cases = [
      {
        selectedAgent: "vega",
        channel: undefined,
        expected: "No sessions yet. Start a new conversation.",
      },
      {
        selectedAgent: "lyre",
        channel: "tui",
        expected: "No TUI sessions for this agent",
      },
    ];

    for (const { selectedAgent, channel, expected } of cases) {
      render(
        <SessionsSection
          sessions={SESSIONS}
          selectedAgent={selectedAgent}
          selectedSession=""
          onSelectSession={() => {}}
        />,
      );
      if (channel) {
        fireEvent.change(screen.getByLabelText("Filter sessions by channel"), {
          target: { value: channel },
        });
      }
      expect(screen.getByText(new RegExp(expected))).toBeTruthy();
      cleanup();
    }
  });

  it("sessions_section_highlights_unread_sessions", () => {
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession="chat:1"
        onSelectSession={() => {}}
        unreadSessionKeys={new Set(["chat:2"])}
      />,
    );

    const unread = screen.getByText("Dev").closest(".session-item");
    const read = screen.getByText("Web Chat").closest(".session-item");
    expect(unread?.className).toContain("unread");
    expect(read?.className).not.toContain("unread");
    cleanup();
  });

  it("sessions_section_header_new_session_button_triggers_callback", () => {
    const onNewSession = vi.fn();
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onNewSession={onNewSession}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "New Session" }));
    expect(
      screen.getByText("The name is saved after the first message."),
    ).toBeTruthy();
    fireEvent.change(screen.getByLabelText("New session name"), {
      target: { value: "設計相談" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    expect(onNewSession).toHaveBeenCalledWith("設計相談");
    cleanup();
  });

  it("sessions_section_shows_compact_channel_badges", () => {
    // Arrange
    const sessions: SessionEntry[] = [
      { ...SESSIONS[0], channel: "web" },
      { ...SESSIONS[1], channel: "discord" },
      { ...SESSIONS[2], agent_id: "lyre", channel: "cli" },
    ];
    render(
      <SessionsSection
        sessions={sessions}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );

    // Act
    const list = document.querySelector(".sessions-list") as HTMLElement;

    // Assert
    expect(within(list).getByLabelText("Web").textContent).toBe("W");
    expect(within(list).getByLabelText("Discord").textContent).toBe("D");
    expect(within(list).getByLabelText("CLI").textContent).toBe("C");
    cleanup();
  });

  it("sessions_section_omits_the_last_message_time", () => {
    // Arrange
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-04T12:00:00Z"));
    const sessions = [
      {
        ...SESSIONS[0],
        last_message_time: Date.parse("2026-07-04T11:55:00Z"),
      },
    ];
    render(
      <SessionsSection
        sessions={sessions}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );

    // Act
    const list = document.querySelector(".sessions-list") as HTMLElement;

    // Assert
    expect(within(list).queryByText("5m")).toBeNull();
    cleanup();
  });

  it("sessions_section_allows_creating_an_unnamed_session", () => {
    // Arrange
    const onNewSession = vi.fn();
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onNewSession={onNewSession}
      />,
    );

    // Act
    fireEvent.click(screen.getByRole("button", { name: "New Session" }));
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    // Assert
    expect(onNewSession).toHaveBeenCalledWith(undefined);
    cleanup();
  });

  it("sessions_section_groups_pinned_active_and_archived", () => {
    // Arrange
    const sessions: SessionEntry[] = [
      { ...SESSIONS[0], session_key: "chat:10", chat_id: 10, name: "Pinned one", pinned: true },
      { ...SESSIONS[1], name: "Active one" },
      {
        ...SESSIONS[0],
        session_key: "chat:11",
        chat_id: 11,
        name: "Archived one",
        archived: true,
        pinned: true,
      },
    ];
    render(
      <SessionsSection
        sessions={sessions}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );

    // Assert: an archived session stays in ARCHIVED even when pinned.
    expect(screen.getByText("PINNED")).toBeTruthy();
    expect(screen.getByText("ACTIVE")).toBeTruthy();
    expect(screen.getByText("Pinned one")).toBeTruthy();
    expect(screen.getByText("Active one")).toBeTruthy();
    expect(screen.getByRole("button", { name: /ARCHIVED/ })).toBeTruthy();
    expect(screen.queryByText("Archived one")).toBeNull();

    // Act
    fireEvent.click(screen.getByRole("button", { name: /ARCHIVED/ }));

    // Assert
    expect(screen.getByText("Archived one")).toBeTruthy();
    cleanup();
  });

  it("sessions_section_scrolls_all_groups_in_one_region_below_the_header", () => {
    // Arrange
    const sessions: SessionEntry[] = [
      { ...SESSIONS[0], session_key: "chat:10", chat_id: 10, pinned: true },
      { ...SESSIONS[1] },
      { ...SESSIONS[0], session_key: "chat:11", chat_id: 11, archived: true },
    ];

    // Act
    const { container } = render(
      <SessionsSection
        sessions={sessions}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );

    // Assert: a single scroll region holds every group, so sessions past the
    // sidebar height stay reachable; the header is outside it.
    const scrollRegions = container.querySelectorAll(".sessions-scroll");
    expect(scrollRegions).toHaveLength(1);
    const groups = container.querySelectorAll(".sessions-group");
    expect(groups).toHaveLength(3);
    for (const group of groups) {
      expect(group.parentElement).toBe(scrollRegions[0]);
    }
    expect(scrollRegions[0].querySelector(".sessions-header")).toBeNull();
    cleanup();
  });

  it("sessions_section_offers_pin_archive_and_copy_actions", async () => {
    // Arrange
    const onUpdateSession = vi.fn();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onUpdateSession={onUpdateSession}
      />,
    );

    // Act: pin
    fireEvent.click(screen.getAllByRole("button", { name: "Session actions" })[0]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Pin" }));

    // Assert
    expect(onUpdateSession).toHaveBeenCalledWith(1, { pinned: true });

    // Act: archive
    fireEvent.click(screen.getAllByRole("button", { name: "Session actions" })[0]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Archive" }));

    // Assert
    expect(onUpdateSession).toHaveBeenCalledWith(1, { archived: true });

    // Act: copy session id
    fireEvent.click(screen.getAllByRole("button", { name: "Session actions" })[0]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy session ID" }));

    // Assert
    expect(writeText).toHaveBeenCalledWith("chat:1");
    expect(await screen.findByText("Copied")).toBeTruthy();
    cleanup();
  });

  it("sessions_section_opens_the_actions_menu_on_right_click", () => {
    // Arrange
    const onUpdateSession = vi.fn();
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onUpdateSession={onUpdateSession}
      />,
    );

    // Act
    fireEvent.contextMenu(screen.getByText("Web Chat"));
    const menu = screen.getByRole("menu", { name: "Session actions" });
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Pin" }));

    // Assert
    expect(onUpdateSession).toHaveBeenCalledWith(1, { pinned: true });
    expect(screen.queryByRole("menu")).toBeNull();
    cleanup();
  });

  it("sessions_section_closes_the_actions_menu_on_escape_and_outside_click", () => {
    // Arrange
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onUpdateSession={() => {}}
      />,
    );

    // Act: Escape
    fireEvent.contextMenu(screen.getByText("Web Chat"));
    fireEvent.keyDown(document, { key: "Escape" });

    // Assert
    expect(screen.queryByRole("menu")).toBeNull();

    // Act: outside click
    fireEvent.contextMenu(screen.getByText("Web Chat"));
    fireEvent.mouseDown(document.body);

    // Assert
    expect(screen.queryByRole("menu")).toBeNull();
    cleanup();
  });

  it("sessions_section_renames_from_the_actions_menu", () => {
    // Arrange
    const onUpdateSession = vi.fn();
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onUpdateSession={onUpdateSession}
      />,
    );

    // Act
    fireEvent.contextMenu(screen.getByText("Web Chat"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    const input = screen.getByLabelText("Session name");
    fireEvent.change(input, { target: { value: "  Renamed  " } });
    fireEvent.blur(input);

    // Assert
    expect(onUpdateSession).toHaveBeenCalledWith(1, { name: "Renamed" });
    cleanup();
  });

  it("sessions_section_copies_the_id_without_the_clipboard_api", async () => {
    // Arrange
    const execCommand = vi.fn().mockReturnValue(true);
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: execCommand,
    });
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
        onUpdateSession={() => {}}
      />,
    );

    // Act
    fireEvent.contextMenu(screen.getByText("Web Chat"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy session ID" }));

    // Assert
    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(await screen.findByText("Copied")).toBeTruthy();
    expect(screen.queryByText("chat:1")).toBeNull();
    cleanup();
  });

  it("sessions_section_falls_back_to_preview_for_unnamed_sessions", () => {
    // Arrange
    const sessions: SessionEntry[] = [{ ...SESSIONS[0], name: null }];
    render(
      <SessionsSection
        sessions={sessions}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );

    // Assert
    expect(screen.getByText("hello world")).toBeTruthy();
    cleanup();
  });
});
