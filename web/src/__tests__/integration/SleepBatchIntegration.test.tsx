import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { SessionItem } from "../../types";

import { Sidebar } from "../../components/Sidebar";

const makeSession = (overrides: Partial<SessionItem> = {}): SessionItem => ({
  session_key: "ses-1",
  label: "Test Session",
  chat_id: 1,
  channel: "cli",
  last_message_preview: "hello",
  ...overrides,
});

const baseSidebarProps = {
  version: "1.0.0",
  sessions: [] as SessionItem[],
  selectedSession: "",
  onNewSession: vi.fn(),
  onSelectSession: vi.fn(),
  onOpenSettings: vi.fn(),
  onOpenSleepBatch: vi.fn(),
  isOpen: false,
  onToggle: vi.fn(),
};

afterEach(cleanup);

describe("Sidebar – Sleep Batch button", () => {
  it("Sidebar_renders_sleep_batch_button", () => {
    // Arrange & Act
    render(<Sidebar {...baseSidebarProps} />);

    // Assert
    expect(screen.getByText("Sleep Batch")).toBeDefined();
  });

  it("Sidebar_sleep_batch_button_triggers_view_change", () => {
    // Arrange
    const onOpenSleepBatch = vi.fn();
    render(
      <Sidebar {...baseSidebarProps} onOpenSleepBatch={onOpenSleepBatch} />,
    );

    // Act
    fireEvent.click(screen.getByText("Sleep Batch"));

    // Assert
    expect(onOpenSleepBatch).toHaveBeenCalledOnce();
  });
});

vi.mock("../../hooks/useAuth", () => ({
  useAuth: () => ({
    authTokenRef: { current: "test-token" },
    showAuth: false,
    setShowAuth: vi.fn(),
    authDraft: "",
    setAuthDraft: vi.fn(),
    saveAuth: vi.fn(),
    withAuthHandling: (fn: () => Promise<void>) => fn(),
  }),
}));

vi.mock("../../hooks/useWebSocket", () => ({
  useWebSocket: () => ({
    wsState: "connected",
    connect: vi.fn(),
  }),
}));

vi.mock("../../hooks/useConfig", () => ({
  useConfig: () => ({
    config: { web_auth_enabled: false },
    configApiKey: "",
    setConfigApiKey: vi.fn(),
    setConfig: vi.fn(),
    selectedProvider: "",
    refreshConfig: vi.fn(),
    saveConfig: vi.fn().mockResolvedValue(undefined),
  }),
}));

vi.mock("../../hooks/useSessions", () => ({
  useSessions: () => ({
    sessions: [makeSession()],
    selectedSession: "",
    selectedSessionRef: { current: "" },
    setSelectedSession: vi.fn(),
    messages: [],
    messageEndRef: { current: null },
    setMessages: vi.fn(),
    refreshSessions: vi.fn(),
    loadHistory: vi.fn(),
    handleNewSession: vi.fn(),
  }),
}));

vi.mock("../../hooks/useStream", () => ({
  useStream: () => ({
    draft: "",
    setDraft: vi.fn(),
    status: "idle",
    handleSend: vi.fn(),
  }),
}));

vi.mock("../../api", () => ({
  api: vi.fn().mockResolvedValue({ ok: true, version: "1.0.0" }),
  AuthRequiredError: class AuthRequiredError extends Error {},
  loadAuthToken: () => "",
  persistAuthToken: () => {},
  fetchAgents: () => Promise.resolve([]),
  fetchSleepRuns: () => Promise.resolve([]),
  fetchRunDetail: () => Promise.resolve({ run: {}, snapshots: [] }),
}));

vi.mock("../../components/SleepBatchPanel", () => ({
  SleepBatchPanel: ({
    onBack,
  }: {
    authTokenRef: React.MutableRefObject<string>;
    onBack: () => void;
  }) => (
    <div data-testid="sleep-batch-panel">
      <span>SleepBatchPanel</span>
      <button onClick={onBack}>Back</button>
    </div>
  ),
}));

vi.mock("../../components/ChatPanel", () => ({
  ChatPanel: () => <div data-testid="chat-panel">ChatPanel</div>,
}));

vi.mock("../../components/AuthModal", () => ({
  AuthModal: () => null,
}));

vi.mock("../../components/SettingsModal", () => ({
  SettingsModal: () => null,
}));

const { App } = await import("../../components/App");

describe("App – Sleep Batch view switching", () => {
  it("App_switches_to_sleep_batch_view", () => {
    // Arrange & Act
    render(<App />);
    expect(screen.getByTestId("chat-panel")).toBeDefined();

    // Act
    fireEvent.click(screen.getByText("Sleep Batch"));

    // Assert
    expect(screen.getByTestId("sleep-batch-panel")).toBeDefined();
  });

  it("App_returns_to_chat_on_session_select", () => {
    // Arrange
    render(<App />);
    fireEvent.click(screen.getByText("Sleep Batch"));
    expect(screen.getByTestId("sleep-batch-panel")).toBeDefined();

    // Act
    fireEvent.click(screen.getByText("Test Session"));

    // Assert
    expect(screen.getByTestId("chat-panel")).toBeDefined();
  });
});
