import { useCallback, useEffect, useMemo, useState } from "react";
import { App } from "./AppShell";
import { AuthModal } from "./AuthModal";
import { CommandPalette } from "./command-palette/CommandPalette";
import { ChatTab } from "../features/chat/ChatTab";
import { SleepBatchPanel } from "../features/sleep/SleepBatchPanel";
import { Toast } from "../shared/ui/Toast";
import { useChatTransport } from "../features/chat/useChatTransport";
import { isWaitingForAssistant, mergeChatMessages } from "../features/chat/chatReducer";
import { AuthRequiredError, loadAuthToken, persistAuthToken } from "../shared/api/auth";
import { fetchAgents } from "../shared/api/agents";
import { fetchHistory } from "../shared/api/history";
import { createSessionKey, fetchSessions, updateSession } from "../shared/api/sessions";
import type { SessionPatch } from "../shared/api/sessions";
import { invalidateQueries, useServerState } from "../shared/hooks/useServerState";
import { useAgentAvatars } from "../shared/hooks/useAgentAvatars";
import { useUnreadSessions } from "../shared/hooks/useUnreadSessions";
import { buildRoutePath, parseRoute, type AppRoute, type SleepView } from "./router";
import type { TabId } from "./navigation";
import { DEFAULT_SESSION_KEY, resolveAutoSessionKey } from "./sessionSelection";

/** Extracts the chat id from a canonical `chat:{id}` session key. */
function parseChatId(key: string): number | null {
  if (!key.startsWith("chat:")) return null;
  const id = Number(key.slice("chat:".length));
  return Number.isInteger(id) && id > 0 ? id : null;
}

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
  // Tracks whether the user explicitly chose a session (sidebar, palette, new
  // session, or a URL). The auto-select effect must not clobber an explicit
  // selection — otherwise a freshly created session is instantly replaced by
  // the first session of the active agent.
  const [sessionExplicit, setSessionExplicit] = useState(
    bootRoute?.tab === "chat" && bootRoute.sessionKey !== null,
  );
  const [sleepView, setSleepView] = useState<SleepView>(
    bootRoute?.tab === "sleep" ? bootRoute.sleepView : "runs",
  );
  const [sleepRunId, setSleepRunId] = useState<string | null>(
    bootRoute?.tab === "sleep" ? bootRoute.runId : null,
  );
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [messageJump, setMessageJump] = useState<{
    index: number;
    seq: number;
  } | null>(null);
  const [authToken, setAuthToken] = useState(loadAuthToken);
  const [authDraft, setAuthDraft] = useState(authToken);
  const [authMessage, setAuthMessage] = useState<string | null>(null);
  const [transportError, setTransportError] = useState<string | null>(null);
  const [pendingSessionName, setPendingSessionName] = useState<string | null>(null);

  const agentsState = useServerState("agents", () => fetchAgents(authToken));
  const sessionsState = useServerState("sessions", () => fetchSessions(authToken), {
    pollIntervalMs: 60_000,
  });
  const historyState = useServerState(
    `history:${selectedSession}`,
    () => fetchHistory(authToken, selectedSession),
    { pollIntervalMs: 60_000 },
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
    const next = resolveAutoSessionKey(sessions, selectedAgent, selectedSession);
    if (next !== selectedSession) setSelectedSession(next);
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
      sleepView,
      runId: sleepRunId,
    });
    if (path !== null) navigatePath(path, true);
  }, [activeTab, selectedAgent, selectedSession, sessionExplicit, sleepView, sleepRunId]);

  useEffect(() => {
    const onPopState = () => {
      const pathname = currentPathname();
      const route = pathname === null ? null : parseRoute(pathname);
      if (!route) return;
      setActiveTab(route.tab);
      if (route.agentId) setSelectedAgent(route.agentId);
      if (route.tab === "sleep") {
        setSleepView(route.sleepView);
        setSleepRunId(route.runId);
      }
      if (route.tab === "chat" && route.sessionKey) {
        setSelectedSession(route.sessionKey);
        setSessionExplicit(true);
      } else {
        setSessionExplicit(false);
      }
      setPendingSessionName(null);
    };
    globalThis.addEventListener("popstate", onPopState);
    return () => globalThis.removeEventListener("popstate", onPopState);
  }, []);

  const handleSelectSession = useCallback(
    (key: string) => {
      setSelectedSession(key);
      setSessionExplicit(true);
      setPendingSessionName(null);
      const path = buildRoutePath({
        tab: activeTab,
        agentId: selectedAgent,
        sessionKey: key,
        sleepView,
        runId: null,
      });
      if (path !== null) navigatePath(path, false);
    },
    [activeTab, selectedAgent, sleepView],
  );

  const handleSelectAgent = useCallback(
    (id: string) => {
      setSelectedAgent(id);
      setSessionExplicit(false);
      setPendingSessionName(null);
      setSleepRunId(null);
      const path = buildRoutePath({
        tab: activeTab,
        agentId: id,
        sessionKey: null,
        sleepView,
        runId: null,
      });
      if (path !== null) navigatePath(path, false);
    },
    [activeTab, sleepView],
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
        sleepView,
        runId: sleepRunId,
      });
      if (path !== null) navigatePath(path, false);
    },
    [selectedAgent, selectedSession, sessionExplicit, sleepView, sleepRunId],
  );

  const handleSleepViewChange = useCallback(
    (view: SleepView) => {
      setSleepView(view);
      setSleepRunId(null);
      const path = buildRoutePath({
        tab: "sleep",
        agentId: selectedAgent,
        sessionKey: null,
        sleepView: view,
        runId: null,
      });
      if (path !== null) navigatePath(path, false);
    },
    [selectedAgent],
  );

  const handleSelectRun = useCallback(
    (runId: string) => {
      setSleepRunId(runId);
      const path = buildRoutePath({
        tab: "sleep",
        agentId: selectedAgent,
        sessionKey: null,
        sleepView: "runs",
        runId,
      });
      if (path !== null) navigatePath(path, false);
    },
    [selectedAgent],
  );

  const handleBackFromRun = useCallback(() => {
    setSleepRunId(null);
    const path = buildRoutePath({
      tab: "sleep",
      agentId: selectedAgent,
      sessionKey: null,
      sleepView: "runs",
      runId: null,
    });
    if (path !== null) navigatePath(path, false);
  }, [selectedAgent]);

  const handleSessionResolved = useCallback(
    (key: string) => {
      setSelectedSession(key);
      setSessionExplicit(true);
      // A name typed before the first message is applied once the session is
      // persisted as `chat:{id}`; unnamed sessions stay untouched.
      const name = pendingSessionName;
      const chatId = parseChatId(key);
      if (name && chatId !== null) {
        setPendingSessionName(null);
        updateSession(authToken, chatId, { name })
          .then(() => invalidateQueries("sessions"))
          .catch((error: unknown) => setTransportError(String(error)));
      }
    },
    [authToken, pendingSessionName],
  );

  const handleUpdateSession = useCallback(
    (chatId: number, patch: SessionPatch) => {
      updateSession(authToken, chatId, patch)
        .then(() => invalidateQueries("sessions"))
        .catch((error: unknown) => setTransportError(String(error)));
    },
    [authToken],
  );

  const handleNewSession = (name?: string) => {
    const key = createSessionKey();
    setPendingSessionName(name?.trim() ? name.trim() : null);
    setSelectedSession(key);
    setSessionExplicit(true);
    setActiveTab("chat");
    // Push the draft session so a reload keeps the unsent draft scoped to
    // this agent; the replace effect keeps it current once it resolves.
    const path = buildRoutePath({
      tab: "chat",
      agentId: selectedAgent,
      sessionKey: key,
      sleepView,
      runId: null,
    });
    if (path !== null) navigatePath(path, false);
  };

  const transport = useChatTransport({
    sessionKey: selectedSession,
    agentId: selectedAgent,
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
    () => mergeChatMessages(historyState.data ?? [], transport.state.messages),
    [historyState.data, transport.state.messages],
  );

  const handleSend = useCallback(
    async (text: string, draftId: string): Promise<boolean> => {
      setTransportError(null);
      try {
        const durableRequestId = await transport.sendMessage(text, draftId);
        if (!durableRequestId) {
          setTransportError("gateway is not connected");
          return false;
        }
        return true;
      } catch (error) {
        if (error instanceof AuthRequiredError) {
          setAuthMessage(error.message);
        } else {
          setTransportError(error instanceof Error ? error.message : String(error));
        }
        return false;
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

  const agentAvatars = useAgentAvatars(agents, authToken);
  const unreadSessionKeys = useUnreadSessions(sessions, selectedSession);

  const chatMain = (
    <ChatTab
      channel={channel}
      readOnly={isReadOnly}
      messages={messages}
      onSend={handleSend}
      storageKey={selectedSession}
      jumpRequest={messageJump ?? undefined}
      agentAvatars={agentAvatars}
      waitingForAssistant={isWaitingForAssistant(transport.state)}
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
        onUpdateSession={handleUpdateSession}
        authToken={authToken}
        agentAvatars={agentAvatars}
        onAvatarChanged={() => invalidateQueries("agents")}
        unreadSessionKeys={unreadSessionKeys}
        main={
          activeTab === "chat" ? (
            <>
              {transportError && <div className="run-error">{transportError}</div>}
              {chatMain}
            </>
          ) : activeTab === "sleep" && selectedAgent ? (
            <SleepBatchPanel
              agents={agents}
              agentId={selectedAgent}
              authToken={authToken}
              view={sleepView}
              runId={sleepRunId}
              onViewChange={handleSleepViewChange}
              onSelectRun={handleSelectRun}
              onBack={handleBackFromRun}
              onRefresh={() => invalidateQueries("sleep")}
            />
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
