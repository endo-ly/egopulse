import { useCallback, useEffect, useMemo, useState } from "react";
import { App } from "./AppShell";
import { AuthModal } from "./AuthModal";
import { CommandPalette } from "./command-palette/CommandPalette";
import { ChatTab } from "../features/chat/ChatTab";
import { SleepBatchPanel } from "../features/sleep/SleepBatchPanel";
import { Toast } from "../shared/ui/Toast";
import { useChatTransport } from "../features/chat/useChatTransport";
import { AuthRequiredError, loadAuthToken, persistAuthToken } from "../shared/api/auth";
import { fetchAgents } from "../shared/api/agents";
import { fetchHistory } from "../shared/api/history";
import { createSessionKey, fetchSessions } from "../shared/api/sessions";
import { invalidateQueries, useServerState } from "../shared/hooks/useServerState";
import type { TabId } from "./navigation";

const DEFAULT_SESSION_KEY = "main";

export function WebUI() {
  const [activeTab, setActiveTab] = useState<TabId>("chat");
  const [selectedAgent, setSelectedAgent] = useState("");
  const [selectedSession, setSelectedSession] = useState(DEFAULT_SESSION_KEY);
  // Tracks whether the user explicitly chose a session (sidebar, palette, or new
  // session). The auto-select effect must not clobber an explicit selection —
  // otherwise a freshly created session is instantly replaced by the first
  // session of the active agent.
  const [sessionExplicit, setSessionExplicit] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [authToken, setAuthToken] = useState(loadAuthToken);
  const [authDraft, setAuthDraft] = useState(authToken);
  const [authMessage, setAuthMessage] = useState<string | null>(null);
  const [transportError, setTransportError] = useState<string | null>(null);

  const agentsState = useServerState("agents", () => fetchAgents(authToken));
  const sessionsState = useServerState("sessions", () => fetchSessions(authToken));
  const historyState = useServerState(`history:${selectedSession}`, () =>
    fetchHistory(authToken, selectedSession),
  );

  const agents = agentsState.data ?? [];
  const sessions = sessionsState.data ?? [];

  useEffect(() => {
    const authError = [agentsState.error, sessionsState.error, historyState.error]
      .find((error) => error instanceof AuthRequiredError);
    if (authError) {
      setAuthMessage(authError.message);
    }
  }, [agentsState.error, historyState.error, sessionsState.error]);

  const genericFetchError = useMemo(() => {
    for (const error of [agentsState.error, sessionsState.error, historyState.error]) {
      if (error && !(error instanceof AuthRequiredError)) return error;
    }
    return null;
  }, [agentsState.error, sessionsState.error, historyState.error]);

  const [dismissedError, setDismissedError] = useState<string | null>(null);
  const visibleFetchError =
    genericFetchError && genericFetchError.message !== dismissedError
      ? genericFetchError
      : null;

  useEffect(() => {
    if (selectedAgent || agents.length === 0) return;
    setSelectedAgent(agents.find((agent) => agent.is_default)?.id ?? agents[0].id);
  }, [agents, selectedAgent]);

  useEffect(() => {
    if (sessionExplicit) return;
    if (
      sessions.some(
        (session) =>
          session.session_key === selectedSession &&
          session.agent_id === selectedAgent,
      )
    )
      return;
    const firstAgentSession = sessions.find(
      (session) => session.agent_id === selectedAgent,
    );
    if (firstAgentSession) {
      setSelectedSession(firstAgentSession.session_key);
    }
  }, [selectedAgent, selectedSession, sessions, sessionExplicit]);

  const handleSelectSession = useCallback((key: string) => {
    setSelectedSession(key);
    setSessionExplicit(true);
  }, []);

  const handleSelectAgent = useCallback((id: string) => {
    setSelectedAgent(id);
    setSessionExplicit(false);
  }, []);

  const handleSessionResolved = useCallback((key: string) => {
    setSelectedSession(key);
    setSessionExplicit(true);
  }, []);

  const handleNewSession = () => {
    setSelectedSession(createSessionKey());
    setSessionExplicit(true);
    setActiveTab("chat");
  };

  const transport = useChatTransport({
    sessionKey: selectedSession,
    authToken,
    onAuthRequired: setAuthMessage,
    onError: setTransportError,
    onSessionResolved: handleSessionResolved,
    onDone: () => {
      invalidateQueries("sessions");
      invalidateQueries(`history:${selectedSession}`);
    },
  });

  const selectedSessionData = sessions.find(
    (session) => session.session_key === selectedSession,
  );
  const channel = selectedSessionData?.channel ?? "web";
  const isReadOnly = channel !== "web";

  const messages = useMemo(
    () => [...(historyState.data ?? []), ...transport.state.messages],
    [historyState.data, transport.state.messages],
  );

  const handleSend = useCallback(
    async (text: string) => {
      setTransportError(null);
      try {
        const requestId = await transport.sendMessage(text);
        if (!requestId) {
          setTransportError("gateway is not connected");
        }
      } catch (error) {
        if (error instanceof AuthRequiredError) {
          setAuthMessage(error.message);
        } else {
          setTransportError(error instanceof Error ? error.message : String(error));
        }
      }
    },
    [transport],
  );

  const handleUnlock = () => {
    persistAuthToken(authDraft);
    setAuthToken(authDraft.trim());
    setAuthMessage(null);
    invalidateQueries("agents");
    invalidateQueries("sessions");
    invalidateQueries("history");
  };

  const refreshCurrentTab = () => {
    if (activeTab === "chat") {
      historyState.invalidate();
      sessionsState.invalidate();
    } else if (activeTab === "sleep") {
      invalidateQueries("sleep");
    }
  };

  const chatMain = (
    <ChatTab
      sessionLabel={selectedSessionData?.label ?? "Web Chat"}
      channel={channel}
      readOnly={isReadOnly}
      messages={messages}
      onSend={handleSend}
      storageKey={selectedSession}
    />
  );

  return (
    <>
      <App
        agents={agents}
        sessions={sessions}
        selectedAgent={selectedAgent}
        selectedSession={selectedSession}
        activeTab={activeTab}
        healthStatus={transport.connectionState === "closed" ? "degraded" : "ok"}
        onTabChange={setActiveTab}
        onSelectAgent={handleSelectAgent}
        onSelectSession={handleSelectSession}
        onOpenPalette={() => setPaletteOpen(true)}
        onNewSession={handleNewSession}
        main={
          activeTab === "chat" ? (
            <>
              {transportError && <div className="run-error">{transportError}</div>}
              {chatMain}
            </>
          ) : activeTab === "sleep" ? (
            <SleepBatchPanel authToken={authToken} />
          ) : null
        }
      />
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        agents={agents}
        sessions={sessions}
        selectedAgent={selectedAgent}
        onNavigate={setActiveTab}
        onSelectAgent={handleSelectAgent}
        onSelectSession={handleSelectSession}
        onNewSession={handleNewSession}
        onRefresh={refreshCurrentTab}
      />
      {authMessage && (
        <AuthModal
          message={authMessage}
          token={authDraft}
          onTokenChange={setAuthDraft}
          onSubmit={handleUnlock}
        />
      )}
      {visibleFetchError && (
        <div className="toast-container">
          <Toast
            tone="error"
            message={`Couldn't load data: ${visibleFetchError.message}`}
            onClose={() => setDismissedError(visibleFetchError.message)}
          />
        </div>
      )}
    </>
  );
}
