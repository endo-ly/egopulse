import { useEffect, useMemo, useState } from "react";

import { api } from "../api";
import type { HealthPayload } from "../types";

import { useAuth } from "../hooks/useAuth";
import { useWebSocket } from "../hooks/useWebSocket";
import { useConfig } from "../hooks/useConfig";
import { useSessions } from "../hooks/useSessions";
import { useStream } from "../hooks/useStream";

import { Sidebar } from "./Sidebar";
import { ChatPanel } from "./ChatPanel";
import { AuthModal } from "./AuthModal";
import { SettingsModal } from "./SettingsModal";

export function App() {
  const [health, setHealth] = useState<HealthPayload>({});
  const [showSettings, setShowSettings] = useState(false);
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);

  const {
    authTokenRef,
    showAuth,
    setShowAuth,
    authDraft,
    setAuthDraft,
    saveAuth,
    withAuthHandling,
  } = useAuth();

  const { wsState, connect } = useWebSocket({
    authTokenRef,
    onAuthRequired: () => {
      setShowAuth(true);
    },
    onStatusChange: () => {},
  });

  const {
    config,
    configApiKey,
    setConfigApiKey,
    setConfig,
    selectedProvider,
    refreshConfig,
    saveConfig,
  } = useConfig({
    authTokenRef,
    onAuthRequired: () => setShowAuth(true),
    onStatusChange: () => {},
  });

  const sessions = useSessions({ authTokenRef });

  const { draft, setDraft, status, handleSend } = useStream({
    authTokenRef,
    selectedSessionRef: sessions.selectedSessionRef,
    setSelectedSession: sessions.setSelectedSession,
    setMessages: sessions.setMessages,
    onAuthRequired: () => setShowAuth(true),
    refreshSessions: sessions.refreshSessions,
    withAuthHandling,
  });

  const selectedLabel = useMemo(() => {
    return (
      sessions.sessions.find(
        (item) => item.session_key === sessions.selectedSession,
      )?.label || sessions.selectedSession
    );
  }, [sessions.selectedSession, sessions.sessions]);

  async function refreshHealth() {
    const payload = await api<{ ok: boolean; version: string }>(
      "/api/health",
      authTokenRef.current,
    );
    setHealth({ version: payload.version });
  }

  useEffect(() => {
    void withAuthHandling(async () => {
      await refreshHealth();
      await refreshConfig();
      await sessions.refreshSessions();
      await connect();
    });
  }, []);

  async function handleSaveAuth() {
    await saveAuth(async () => {
      await refreshHealth();
      await refreshConfig();
      await sessions.refreshSessions();
      await connect();
    });
  }

  async function handleSaveConfig() {
    try {
      await saveConfig();
      setShowSettings(false);
    } catch {
    }
  }

  return (
    <div className="app-shell">
      {isSidebarOpen && (
        <div
          className="sidebar-backdrop visible"
          onClick={() => setIsSidebarOpen(false)}
        />
      )}

      <Sidebar
        version={health.version ?? ""}
        sessions={sessions.sessions}
        selectedSession={sessions.selectedSession}
        onNewSession={sessions.handleNewSession}
        onSelectSession={(key) => {
          sessions.selectedSessionRef.current = key;
          sessions.setSelectedSession(key);
          void sessions.loadHistory(key);
        }}
        onOpenSettings={() => setShowSettings(true)}
        isOpen={isSidebarOpen}
        onToggle={() => setIsSidebarOpen(false)}
      />

      <ChatPanel
        selectedLabel={selectedLabel}
        wsState={wsState}
        authEnabled={config?.web_auth_enabled ?? false}
        status={status}
        messages={sessions.messages}
        messageEndRef={sessions.messageEndRef}
        draft={draft}
        setDraft={setDraft}
        onSend={handleSend}
        onToggleSidebar={() => setIsSidebarOpen(true)}
      />

      {showSettings && config ? (
        <SettingsModal
          config={config}
          selectedProvider={selectedProvider}
          configApiKey={configApiKey}
          setConfigApiKey={setConfigApiKey}
          setConfig={setConfig}
          onClose={() => setShowSettings(false)}
          onSave={handleSaveConfig}
        />
      ) : null}

      {showAuth ? (
        <AuthModal
          authDraft={authDraft}
          setAuthDraft={setAuthDraft}
          onSubmit={handleSaveAuth}
        />
      ) : null}
    </div>
  );
}
