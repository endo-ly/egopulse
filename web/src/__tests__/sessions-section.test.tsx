import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup, within } from "@testing-library/react";
import { SessionsSection } from "../components/SessionsSection";
import type { SessionEntry } from "../components/SessionsSection";

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

    expect(screen.queryByText("Notes")).toBeNull();

    const items = screen.getAllByText(/Web Chat|Dev/);
    expect(items).toHaveLength(2);

    const list = document.querySelector(".sessions-list") as HTMLElement;
    const labels = within(list)
      .getAllByText(/Web Chat|Dev/)
      .map((el) => el.textContent);
    expect(labels).toEqual(["Dev", "Web Chat"]);

    const filter = screen.getByLabelText("Filter sessions by channel");
    fireEvent.change(filter, { target: { value: "web" } });

    expect(screen.queryByText("Dev")).toBeNull();
    expect(screen.queryByText("Web Chat")).not.toBeNull();

    fireEvent.click(screen.getByText("Web Chat"));
    expect(onSelectSession).toHaveBeenCalledWith("s1");
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
