import { useState, useCallback } from "react";
import { createRoot } from "react-dom/client";
import "./app.css";
import { App } from "./components/App";
import { ChatTab } from "./components/ChatTab";
import { CommandPalette } from "./components/CommandPalette";
import { useChatTransport } from "./hooks/useChatTransport";
import type { AgentEntry } from "./components/AgentsSection";
import type { SessionEntry } from "./components/SessionsSection";
import type { TabId } from "./components/TopBar";

const MOCK_AGENTS: AgentEntry[] = [
  { id: "default", label: "Default Agent", is_default: true, active: false },
];

const MOCK_SESSIONS: SessionEntry[] = [
  { session_key: "main", label: "Web Chat", channel: "web", agent_id: "default", last_message_time: 0, last_message_preview: "hello" },
];

function WebUI() {
  const [activeTab, setActiveTab] = useState<TabId>("chat");
  const [selectedAgent, setSelectedAgent] = useState("default");
  const [selectedSession, setSelectedSession] = useState("main");
  const [paletteOpen, setPaletteOpen] = useState(false);

  const transport = useChatTransport({ sessionKey: selectedSession });

  const handleNewSession = () => {
    setSelectedSession("main");
    setActiveTab("chat");
  };

  const handleSend = useCallback((text: string) => {
    transport.sendMessage(text);
  }, [transport]);

  const selectedSessionData = MOCK_SESSIONS.find((s) => s.session_key === selectedSession);
  const isReadOnly = selectedSessionData?.channel !== "web";

  const chatMain = (
    <ChatTab
      sessionLabel={selectedSessionData?.label ?? "Web Chat"}
      channel={selectedSessionData?.channel ?? "web"}
      messageCount={transport.state.messages.length}
      readOnly={isReadOnly}
      messages={transport.state.messages}
      onSend={handleSend}
      storageKey={selectedSession}
    />
  );

  return (
    <>
      <App
        agents={MOCK_AGENTS}
        sessions={MOCK_SESSIONS}
        selectedAgent={selectedAgent}
        selectedSession={selectedSession}
        activeTab={activeTab}
        onTabChange={setActiveTab}
        onSelectAgent={setSelectedAgent}
        onSelectSession={setSelectedSession}
        onOpenPalette={() => setPaletteOpen(true)}
        onNewSession={handleNewSession}
        main={activeTab === "chat" ? chatMain : null}
      />
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        agents={MOCK_AGENTS}
        sessions={MOCK_SESSIONS}
        selectedAgent={selectedAgent}
        onNavigate={setActiveTab}
        onSelectAgent={setSelectedAgent}
        onSelectSession={setSelectedSession}
        onNewSession={handleNewSession}
        onRefresh={() => {}}
      />
    </>
  );
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(<WebUI />);
}
