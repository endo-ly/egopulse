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
import { buildRoutePath, parseRoute, type AppRoute } from "./router";
import type { TabId } from "./navigation";

const DEFAULT_SESSION_KEY = "main";

function currentPathname(): string | null {
  try {
    return globalThis.location.pathname;
  } catch {
    return null;
  }
}

function navigatePath(path: string, replace: boolean): void {
  try {
    const url = new URL(globalThis.location.href);
    if (url.pathname === path) return;
    url.pathname = path;
    globalThis.history[replace ? "replaceState" : "pushState"](null, "", url.toString());
  } catch {
    // Non-browser environment: nothing to sync.
  }
}

function initialRoute(): AppRoute | null {
  const pathname = currentPathname();
  return pathname === null ? null : parseRoute(pathname);
}

export function WebUI() {
  const bootRoute = useMemo(initialRoute, []);
  const [activeTab, setActiveTab] = useState<TabId>(bootRoute?.tab ?? "chat");
  const [selectedAgent, setSelectedAgent] = useState(bootRoute?.agentId ?? "");
  const [selectedSession, setSelectedSession] = useState(
    bootRoute?.sessionKey ?? DEFAULT_SESSION_KEY,
  );
  // Tracks whether the user explicitly chose a session (sidebar, palette, or new
  // session). The auto-select effect must not clobber an explicit selection —
  // otherwise a freshly created session is instantly replaced by the first
  // session of the active agent.
  const [sessionExplicit, setSessionExplicit] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [messageJump, setMessageJump] = useState<{
    index: number;
    seq: number;
  } | null>(null);
  const [authToken, setAuthToken] = useState(loadAuthToken);
  const [authDraft, setAuthDraft] = useState(authToken);
  const [authMessage, setAuthMessage] = useState<string | null>(null);
  const [transportError, setTransportError] = useState<string | null>(null);

  const agentsState = useServerState("agents", () => fetchAgents(authToken));
  const sessionsState = useServerState("sessions", () => fetchSessions(authToken), {
    pollIntervalMs: 10_000,
  });
  const historyState = useServerState(
    `history:${selectedSession}`,
    () => fetchHistory(authToken, selectedSession),
    { pollIntervalMs: 10_000 },
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

  // Reflects auto-derived selection changes in the URL. User actions push
  // their own history entry beforehand, so this only ever replaces.
  useEffect(() => {
    const path = buildRoutePath({
      tab: activeTab,
      agentId: selectedAgent,
      sessionKey:
        activeTab === "chat" &&
        (sessionExplicit || selectedSession !== DEFAULT_SESSION_KEY)
          ? selectedSession
          : null,
    });
    if (path !== null) navigatePath(path, true);
  }, [activeTab, selectedAgent, selectedSession, sessionExplicit]);

  useEffect(() => {
    const onPopState = () => {
      const pathname = currentPathname();
      const route = pathname === null ? null : parseRoute(pathname);
      if (!route) return;
      setActiveTab(route.tab);
      if (route.agentId) setSelectedAgent(route.agentId);
      if (route.tab === "chat" && route.sessionKey) {
        setSelectedSession(route.sessionKey);
        setSessionExplicit(true);
      } else {
        setSessionExplicit(false);
      }
    };
    globalThis.addEventListener("popstate", onPopState);
    return () => globalThis.removeEventListener("popstate", onPopState);
  }, []);

  const handleSelectSession = useCallback(
    (key: string) => {
      setSelectedSession(key);
      setSessionExplicit(true);
      const path = buildRoutePath({
        tab: activeTab,
        agentId: selectedAgent,
        sessionKey: key,
      });
      if (path !== null) navigatePath(path, false);
    },
    [activeTab, selectedAgent],
  );

  const handleSelectAgent = useCallback(
    (id: string) => {
      setSelectedAgent(id);
      setSessionExplicit(false);
      const path = buildRoutePath({ tab: activeTab, agentId: id, sessionKey: null });
      if (path !== null) navigatePath(path, false);
    },
    [activeTab],
  );

  const handleTabChange = useCallback(
    (tab: TabId) => {
      setActiveTab(tab);
      const path = buildRoutePath({
        tab,
        agentId: selectedAgent,
        sessionKey:
          tab === "chat" &&
          (sessionExplicit || selectedSession !== DEFAULT_SESSION_KEY)
            ? selectedSession
            : null,
      });
      if (path !== null) navigatePath(path, false);
    },
    [selectedAgent, selectedSession, sessionExplicit],
  );

  const handleSessionResolved = useCallback((key: string) => {
    setSelectedSession(key);
    setSessionExplicit(true);
  }, []);

  const handleNewSession = () => {
    const key = createSessionKey();
    setSelectedSession(key);
    setSessionExplicit(true);
    setActiveTab("chat");
    // Push the draft session so a reload keeps the unsent draft scoped to
    // this agent; the replace effect keeps it current once it resolves.
    const path = buildRoutePath({
      tab: "chat",
      agentId: selectedAgent,
      sessionKey: key,
    });
    if (path !== null) navigatePath(path, false);
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

  // A dropped connection surfaces an error banner once; recovering clears it.
  useEffect(() => {
    if (transport.connectionState === "open") {
      setTransportError(null);
    }
  }, [transport.connectionState]);

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

  const handleJumpToMessage = useCallback(
    (index: number) => {
      setMessageJump((prev) => ({ index, seq: (prev?.seq ?? 0) + 1 }));
      if (activeTab !== "chat") {
        handleTabChange("chat");
      }
    },
    [activeTab, handleTabChange],
  );

  const chatMain = (
    <ChatTab
      channel={channel}
      readOnly={isReadOnly}
      messages={messages}
      onSend={handleSend}
      storageKey={selectedSession}
      jumpRequest={messageJump ?? undefined}
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
        onTabChange={handleTabChange}
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
        messages={messages}
        onNavigate={setActiveTab}
        onSelectAgent={handleSelectAgent}
        onSelectSession={handleSelectSession}
        onNewSession={handleNewSession}
        onRefresh={refreshCurrentTab}
        onJumpToMessage={handleJumpToMessage}
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
