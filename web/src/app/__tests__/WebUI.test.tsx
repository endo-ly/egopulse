import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setUnsavedChanges } from "../../shared/lib/unsaved";
import { WebUI } from "../WebUI";

// The screens themselves fetch and stream; this test only exercises how the
// shell wires navigation, so each screen renders as a marker.
vi.mock("../../features/chat/ChatTab", () => ({ ChatTab: () => <div>chat screen</div> }));
vi.mock("../../features/agent/AgentPanel", () => ({
  AgentPanel: ({ onSelectSection }: { onSelectSection: (section: string) => void }) => (
    <div>
      agent screen
      <button type="button" onClick={() => onSelectSection("rules")}>
        open rules
      </button>
    </div>
  ),
}));
vi.mock("../../features/schedule/SchedulePanel", () => ({
  SchedulePanel: () => <div>schedule screen</div>,
}));
vi.mock("../../features/config/ConfigPanel", () => ({
  ConfigPanel: ({ onOpenSchedule }: { onOpenSchedule: (agentId: string) => void }) => (
    <div>
      config screen
      <button type="button" onClick={() => onOpenSchedule("lyre")}>
        open schedule
      </button>
    </div>
  ),
}));
vi.mock("../../features/config/ConfigSectionNav", () => ({
  ConfigSectionNav: () => <div>config nav</div>,
}));
vi.mock("../../features/chat/useChatTransport", () => ({
  useChatTransport: () => ({
    connectionState: "open",
    state: { messages: [], waitingRuns: [] },
    sendMessage: vi.fn(),
  }),
}));
vi.mock("../../features/chat/useChatHistory", () => ({
  useChatHistory: () => ({
    messages: [],
    error: null,
    invalidate: vi.fn(),
    loadOlder: vi.fn(),
    retryOlder: vi.fn(),
    loadingOlder: false,
    olderError: null,
    atHistoryStart: true,
    loadSeq: 0,
  }),
}));
vi.mock("../../shared/hooks/useAgentAvatars", () => ({ useAgentAvatars: () => ({}) }));
vi.mock("../../shared/hooks/useUnreadSessions", () => ({ useUnreadSessions: () => new Set() }));
vi.mock("../../shared/api/auth", () => ({
  AuthRequiredError: class AuthRequiredError extends Error {},
  loadAuthToken: () => "token",
  persistAuthToken: vi.fn(),
}));
vi.mock("../../shared/api/agents", () => ({
  fetchAgents: async () => [{ id: "lyre", label: "Lyre", is_default: true }],
}));
vi.mock("../../shared/api/sessions", () => ({
  createSessionKey: () => "draft:1",
  fetchSessions: async () => [
    {
      session_key: "chat:1",
      chat_id: 1,
      name: "Design chat",
      channel: "web",
      agent_id: "lyre",
      last_message_preview: "hi",
      last_message_time: 1,
      archived: false,
      pinned: false,
    },
  ],
  updateSession: vi.fn(),
}));

describe("WebUI", () => {
  beforeEach(() => {
    globalThis.history.replaceState({}, "", "/agents/lyre/schedule");
  });

  afterEach(() => {
    cleanup();
    setUnsavedChanges("config-form", false);
    setUnsavedChanges("agent-file", false);
  });

  it("opens the chat screen when a session is picked from the command palette", async () => {
    // Arrange: the Schedule screen is showing.
    render(<WebUI />);
    await screen.findByText("schedule screen");

    // Act: pick a session in the palette.
    fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
    fireEvent.click(await screen.findByText("Design chat"));

    // Assert: the chat screen takes over and the URL follows it, instead of
    // the Schedule route being restored under the new selection.
    expect(await screen.findByText("chat screen")).toBeTruthy();
    expect(globalThis.location.pathname).toBe("/agents/lyre/chat/s/chat:1");
  });

  it("asks_before_opening_an_agent_screen_from_config", async () => {
    // Arrange: Config is showing with an unsaved form open.
    globalThis.history.replaceState({}, "", "/config");
    setUnsavedChanges("config-form", true);
    const confirm = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    render(<WebUI />);
    await screen.findByText("config screen");

    // Act: open Schedule from the Config screen.
    fireEvent.click(screen.getByRole("button", { name: "open schedule" }));

    // Assert: the draft is not discarded and the screen stays put.
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(screen.getByText("config screen")).toBeTruthy();
    expect(globalThis.location.pathname).toBe("/config");

    // Act: accept the discard.
    confirm.mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: "open schedule" }));

    // Assert: the navigation now goes through.
    expect(await screen.findByText("schedule screen")).toBeTruthy();
    expect(globalThis.location.pathname.startsWith("/agents/lyre/schedule")).toBe(true);
    confirm.mockRestore();
  });

  it("asks_before_switching_agent_sections_with_unsaved_prompt_edit", async () => {
    // Arrange: the Agent screen shows a section with an unsaved edit.
    globalThis.history.replaceState({}, "", "/agents/lyre/soul");
    setUnsavedChanges("agent-file", true);
    const confirm = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    render(<WebUI />);
    await screen.findByText("agent screen");

    // Act: switch to another section and refuse the discard.
    fireEvent.click(screen.getByRole("button", { name: "open rules" }));

    // Assert: the draft and the URL stay put.
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(globalThis.location.pathname).toBe("/agents/lyre/soul");

    // Act: accept the discard.
    confirm.mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: "open rules" }));

    // Assert: the section changes.
    await waitFor(() => expect(globalThis.location.pathname).toBe("/agents/lyre/rules"));
    confirm.mockRestore();
  });

  it("asks_before_starting_a_new_session_with_unsaved_prompt_edit", async () => {
    // Arrange: the Agent screen shows a section with an unsaved edit.
    globalThis.history.replaceState({}, "", "/agents/lyre/soul");
    setUnsavedChanges("agent-file", true);
    const confirm = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    render(<WebUI />);
    await screen.findByText("agent screen");

    const startNewSession = async () => {
      fireEvent.click(screen.getByRole("button", { name: /open command palette/i }));
      // The palette can show the same action under Recent after the first use.
      fireEvent.click((await screen.findAllByText("New Session"))[0]);
    };

    // Act: refuse to discard the draft.
    await startNewSession();

    // Assert: the editor stays mounted.
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(globalThis.location.pathname).toBe("/agents/lyre/soul");
    expect(screen.getByText("agent screen")).toBeTruthy();

    // Act: accept the discard.
    confirm.mockReturnValue(true);
    await startNewSession();

    // Assert: the new session replaces the screen.
    expect(await screen.findByText("chat screen")).toBeTruthy();
    expect(globalThis.location.pathname).toBe("/agents/lyre/chat/s/draft:1");
    confirm.mockRestore();
  });

  it("keeps_a_config_edit_when_history_moves_between_sections", async () => {
    // Arrange: Config is showing with an unsaved form open.
    globalThis.history.replaceState({}, "", "/config");
    setUnsavedChanges("config-form", true);
    const confirm = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    render(<WebUI />);
    await screen.findByText("config screen");

    // Act: go back/forward to another section of the same page.
    globalThis.history.pushState({}, "", "/config/providers");
    globalThis.dispatchEvent(new PopStateEvent("popstate"));

    // Assert: the form is not discarded, so no prompt is shown.
    expect(confirm).not.toHaveBeenCalled();
    expect(screen.getByText("config screen")).toBeTruthy();
    await waitFor(() => expect(globalThis.location.pathname).toBe("/config/providers"));
    confirm.mockRestore();
  });
});
