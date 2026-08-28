import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup, within } from "@testing-library/react";
import { SessionsSection } from "../SessionsSection";
import type { SessionEntry } from "../../../shared/api/types";

const SESSIONS: SessionEntry[] = [
  {
    session_key: "s1",
    label: "Web Chat",
    channel: "web",
    agent_id: "lyre",
    last_message_preview: "hello world",
    last_message_time: 100,
  },
  {
    session_key: "s2",
    label: "Dev",
    channel: "discord",
    agent_id: "lyre",
    last_message_preview: "review code",
    last_message_time: 300,
  },
  {
    session_key: "s3",
    label: "Notes",
    channel: "cli",
    agent_id: "ace",
    last_message_preview: "quick note",
    last_message_time: 200,
  },
];

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

    expect(screen.queryByText("quick note")).toBeNull();

    const items = screen.getAllByText(/hello world|review code/);
    expect(items).toHaveLength(2);

    const list = document.querySelector(".sessions-list") as HTMLElement;
    const previews = within(list)
      .getAllByText(/hello world|review code/)
      .map((el) => el.textContent);
    expect(previews).toEqual(["review code", "hello world"]);

    const filter = screen.getByLabelText("Filter sessions by channel");
    fireEvent.change(filter, { target: { value: "web" } });

    expect(screen.queryByText("review code")).toBeNull();
    expect(screen.queryByText("hello world")).not.toBeNull();

    fireEvent.click(screen.getByText("hello world"));
    expect(onSelectSession).toHaveBeenCalledWith("s1");
    cleanup();
  });

  it("sessions_section_does_not_render_session_label", () => {
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );
    expect(screen.queryByText("Web Chat")).toBeNull();
    expect(screen.queryByText("Dev")).toBeNull();
    cleanup();
  });

  it("sessions_section_shows_empty_state_when_no_sessions_for_agent", () => {
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="vega"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );
    expect(screen.getByText("No sessions yet. Start a new conversation.")).toBeTruthy();
    cleanup();
  });

  it("sessions_section_shows_empty_state_when_channel_filter_excludes_all", () => {
    render(
      <SessionsSection
        sessions={SESSIONS}
        selectedAgent="lyre"
        selectedSession=""
        onSelectSession={() => {}}
      />,
    );
    fireEvent.change(screen.getByLabelText("Filter sessions by channel"), {
      target: { value: "tui" },
    });
    expect(screen.getByText(/No TUI sessions for this agent/)).toBeTruthy();
  });
});
