import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { App } from "./AppShell";
import { AuthModal } from "./AuthModal";
import { CommandPalette } from "./command-palette/CommandPalette";
import { ChatTab } from "../features/chat/ChatTab";
import { AgentPanel } from "../features/agent/AgentPanel";
import { AGENT_QUERY_PREFIX } from "../features/agent/agentQuery";
import { SchedulePanel } from "../features/schedule/SchedulePanel";
import { SCHEDULE_QUERY_PREFIX } from "../features/schedule/scheduleQuery";
import { EmptyState } from "../shared/ui/EmptyState";
import { Toast } from "../shared/ui/Toast";
import { useChatTransport } from "../features/chat/useChatTransport";
import { useChatHistory } from "../features/chat/useChatHistory";
import { isWaitingForAssistant, mergeChatMessages } from "../features/chat/chatReducer";
import { buildTimelineEntries } from "../features/chat/timelineEntries";
import { AuthRequiredError, loadAuthToken, persistAuthToken } from "../shared/api/auth";
import { fetchAgents } from "../shared/api/agents";
import { createSessionKey, fetchSessions, updateSession } from "../shared/api/sessions";
import type { SessionPatch } from "../shared/api/sessions";
import { invalidateQueries, useServerState } from "../shared/hooks/useServerState";
import { useAgentAvatars } from "../shared/hooks/useAgentAvatars";
import { useUnreadSessions } from "../shared/hooks/useUnreadSessions";
import {
  buildRoutePath,
  parseRoute,
  SCHEDULE_ACTIVITY,
  type AgentSection,
  type AppRoute,
  type ScheduleView,
} from "./router";
import { NAV_TABS, isAgentTab, type TabId } from "./navigation";
import { resolveAutoSessionKey } from "./sessionSelection";

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

const COMING_SOON_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7v5l3 2" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

/** Per-screen selections that a route carries besides the tab and agent. */
interface ScreenSelection {
  sessionKey: string | null;
  agentSection: AgentSection;
  scheduleView: ScheduleView;
}

function composeRoute(tab: TabId, agentId: string, selection: ScreenSelection): AppRoute {
  switch (tab) {
    case "chat":
      return { tab, agentId, sessionKey: selection.sessionKey };
    case "agent":
      return { tab, agentId, section: selection.agentSection };
    case "schedule":
      return { tab, agentId, view: selection.scheduleView };
    case "metrics":
    case "config":
      return { tab };
  }
}

/** Navigates to `route`, pushing a history entry unless `replace` is set. */
function navigateTo(route: AppRoute, replace = false): void {
  const path = buildRoutePath(route);
  if (path !== null) navigatePath(path, replace);
}

export function WebUI() {
  const bootRoute = useMemo(initialRoute, []);
  const [activeTab, setActiveTab] = useState<TabId>(bootRoute?.tab ?? "chat");
  const [selectedAgent, setSelectedAgent] = useState(
    bootRoute && "agentId" in bootRoute ? bootRoute.agentId : "",
  );
  // A fresh key for the new-conversation state. It is never a persisted
  // session key, so an archived session cannot be reused through it.
  const [draftSessionKey, setDraftSessionKey] = useState(createSessionKey);
  const [selectedSession, setSelectedSession] = useState(
    bootRoute?.tab === "chat" ? (bootRoute.sessionKey ?? draftSessionKey) : draftSessionKey,
  );
  // Tracks whether the user explicitly chose a session (sidebar, palette, new
  // session, or a URL). The auto-select effect must not clobber an explicit
  // selection — otherwise a freshly created session is instantly replaced by
  // the first session of the active agent.
  const [sessionExplicit, setSessionExplicit] = useState(
    bootRoute?.tab === "chat" && bootRoute.sessionKey !== null,
  );
  const [agentSection, setAgentSection] = useState<AgentSection>(
    bootRoute?.tab === "agent" ? bootRoute.section : "overview",
  );
  const [scheduleView, setScheduleView] = useState<ScheduleView>(
    bootRoute?.tab === "schedule" ? bootRoute.view : SCHEDULE_ACTIVITY,
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
  // Names typed for drafts that have not been acknowledged yet. Keyed by the
  // draft key so an ack that arrives after the user moved away still finds the
  // right name.
  const pendingSessionNamesRef = useRef(new Map<string, string>());

  const agentsState = useServerState("agents", () => fetchAgents(authToken));
  const sessionsState = useServerState("sessions", () => fetchSessions(authToken), {
    pollIntervalMs: 60_000,
  });
  const history = useChatHistory(selectedSession, authToken);

  const agents = agentsState.data ?? [];
  const sessions = sessionsState.data ?? [];

  useEffect(() => {
    const authError = [agentsState.error, sessionsState.error, history.error]
      .find((error) => error instanceof AuthRequiredError);
    if (authError) {
      setAuthMessage(authError.message);
    }
  }, [agentsState.error, history.error, sessionsState.error]);

  const genericFetchError = useMemo(() => {
    for (const error of [agentsState.error, sessionsState.error, history.error]) {
      if (error && !(error instanceof AuthRequiredError)) return error;
    }
    return null;
  }, [agentsState.error, sessionsState.error, history.error]);

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
    const next = resolveAutoSessionKey(
      sessions,
      selectedAgent,
      selectedSession,
      draftSessionKey,
    );
    if (next !== selectedSession) setSelectedSession(next);
  }, [
    draftSessionKey,
    selectedAgent,
    selectedSession,
    sessions,
    sessionExplicit,
  ]);

  // The session key the URL carries: only an explicit or resolved session,
  // never the throwaway draft key of the new-conversation state.
  const urlSessionKey =
    sessionExplicit || selectedSession !== draftSessionKey ? selectedSession : null;

  // Reflects auto-derived selection changes in the URL. User actions push
  // their own history entry beforehand, so this only ever replaces.
  useEffect(() => {
    navigateTo(
      composeRoute(activeTab, selectedAgent, {
        sessionKey: urlSessionKey,
        agentSection,
        scheduleView,
      }),
      true,
    );
  }, [activeTab, agentSection, scheduleView, selectedAgent, urlSessionKey]);

  useEffect(() => {
    const onPopState = () => {
      const pathname = currentPathname();
      const route = pathname === null ? null : parseRoute(pathname);
      if (!route) return;
      setActiveTab(route.tab);
      if ("agentId" in route && route.agentId) setSelectedAgent(route.agentId);
      if (route.tab === "agent") setAgentSection(route.section);
      if (route.tab === "schedule") setScheduleView(route.view);
      if (route.tab === "chat" && route.sessionKey) {
        setSelectedSession(route.sessionKey);
        setSessionExplicit(true);
      } else {
        setSessionExplicit(false);
      }
      setDraftSessionKey(createSessionKey());
    };
    globalThis.addEventListener("popstate", onPopState);
    return () => globalThis.removeEventListener("popstate", onPopState);
  }, []);

  const handleSelectSession = useCallback(
    (key: string) => {
      setSelectedSession(key);
      setSessionExplicit(true);
      setDraftSessionKey(createSessionKey());
      navigateTo({ tab: "chat", agentId: selectedAgent, sessionKey: key });
    },
    [selectedAgent],
  );

  const handleOpenSession = useCallback(
    (key: string) => {
      setActiveTab("chat");
      handleSelectSession(key);
    },
    [handleSelectSession],
  );

  const handleSelectAgent = useCallback(
    (id: string) => {
      // Global screens are agent-independent; picking an agent there opens
      // that agent's Agent screen.
      const tab = isAgentTab(activeTab) ? activeTab : "agent";
      setActiveTab(tab);
      setSelectedAgent(id);
      setSessionExplicit(false);
      setDraftSessionKey(createSessionKey());
      // Runs and intentions belong to the previous agent.
      setScheduleView(SCHEDULE_ACTIVITY);
      navigateTo(composeRoute(tab, id, { sessionKey: null, agentSection, scheduleView: SCHEDULE_ACTIVITY }));
    },
    [activeTab, agentSection],
  );

  const handleTabChange = useCallback(
    (tab: TabId) => {
      setActiveTab(tab);
      navigateTo(
        composeRoute(tab, selectedAgent, {
          sessionKey: urlSessionKey,
          agentSection,
          scheduleView,
        }),
      );
    },
    [agentSection, scheduleView, selectedAgent, urlSessionKey],
  );

  const handleSelectAgentSection = useCallback(
    (section: AgentSection) => {
      setActiveTab("agent");
      setAgentSection(section);
      navigateTo({ tab: "agent", agentId: selectedAgent, section });
    },
    [selectedAgent],
  );

  const handleChangeScheduleView = useCallback(
    (view: ScheduleView) => {
      setActiveTab("schedule");
      setScheduleView(view);
      navigateTo({ tab: "schedule", agentId: selectedAgent, view });
    },
    [selectedAgent],
  );

  const handleOpenSleepRun = useCallback(
    (runId: string) => handleChangeScheduleView({ section: "activity", run: { kind: "sleep", id: runId } }),
    [handleChangeScheduleView],
  );

  const handleSessionResolved = useCallback((key: string) => {
    setSelectedSession(key);
    setSessionExplicit(true);
    setDraftSessionKey(createSessionKey());
  }, []);

  const handleSessionAccepted = useCallback(
    (key: string, sourceSessionKey: string) => {
      const name = pendingSessionNamesRef.current.get(sourceSessionKey);
      const chatId = parseChatId(key);
      if (!name || chatId === null) return;
      pendingSessionNamesRef.current.delete(sourceSessionKey);
      updateSession(authToken, chatId, { name })
        .then(() => invalidateQueries("sessions"))
        .catch((error: unknown) => setTransportError(String(error)));
    },
    [authToken],
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
    const trimmedName = name?.trim();
    setDraftSessionKey(key);
    if (trimmedName) pendingSessionNamesRef.current.set(key, trimmedName);
    setSelectedSession(key);
    setSessionExplicit(true);
    setActiveTab("chat");
    // Push the draft session so a reload keeps the unsent draft scoped to
    // this agent; the replace effect keeps it current once it resolves.
    navigateTo({ tab: "chat", agentId: selectedAgent, sessionKey: key });
  };

  const transport = useChatTransport({
    sessionKey: selectedSession,
    agentId: selectedAgent,
    authToken,
    onAuthRequired: setAuthMessage,
    onError: setTransportError,
    onSessionAccepted: handleSessionAccepted,
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

  const timelineEntries = useMemo(
    () =>
      buildTimelineEntries(
        mergeChatMessages(history.messages, transport.state.messages),
      ),
    [history.messages, transport.state.messages],
  );
  // The palette searches the top-level rows so its jump indices match the
  // rendered timeline.
  const messages = useMemo(
    () => timelineEntries.map((entry) => entry.message),
    [timelineEntries],
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
      history.invalidate();
      sessionsState.invalidate();
    } else if (activeTab === "schedule") {
      invalidateQueries(SCHEDULE_QUERY_PREFIX);
    } else if (activeTab === "agent") {
      invalidateQueries(AGENT_QUERY_PREFIX);
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
      key={selectedSession}
      channel={channel}
      readOnly={isReadOnly}
      entries={timelineEntries}
      onSend={handleSend}
      storageKey={selectedSession}
      jumpRequest={messageJump ?? undefined}
      agentAvatars={agentAvatars}
      waitingForAssistant={isWaitingForAssistant(transport.state)}
      onLoadOlder={history.loadOlder}
      onRetryOlder={history.retryOlder}
      loadingOlder={history.loadingOlder}
      olderError={history.olderError}
      atHistoryStart={history.atHistoryStart}
      olderLoadSeq={history.loadSeq}
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
        agentSection={agentSection}
        scheduleView={scheduleView}
        onSelectAgentSection={handleSelectAgentSection}
        onChangeScheduleView={handleChangeScheduleView}
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
          ) : activeTab === "agent" && selectedAgent ? (
            <AgentPanel
              agentId={selectedAgent}
              authToken={authToken}
              section={agentSection}
              avatarUrl={agentAvatars[selectedAgent]}
              sessions={sessions}
              onSelectSection={handleSelectAgentSection}
              onOpenSleepRun={handleOpenSleepRun}
            />
          ) : activeTab === "schedule" && selectedAgent ? (
            <SchedulePanel
              agentId={selectedAgent}
              authToken={authToken}
              view={scheduleView}
              sessions={sessions}
              onChangeView={handleChangeScheduleView}
              onOpenSession={handleOpenSession}
            />
          ) : activeTab === "metrics" || activeTab === "config" ? (
            <EmptyState
              icon={COMING_SOON_ICON}
              title={NAV_TABS.find((tab) => tab.id === activeTab)?.label ?? activeTab}
              description="This screen is coming soon."
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
        onNavigate={handleTabChange}
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
