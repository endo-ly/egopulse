import { NAV_TABS, type TabId } from "../navigation";
import type { AgentEntry, ChatMessage, SessionEntry } from "../../shared/api/types";
import { loadPaletteHistory, pushPaletteHistory } from "./paletteHistory";
import { sessionTitle } from "../../shared/lib/session";

export interface PaletteItem {
  id: string;
  label: string;
  section: string;
  description?: string;
  disabled?: boolean;
  onSelect: () => void;
}

export interface PaletteActions {
  close: () => void;
  navigate: (tab: TabId) => void;
  selectAgent: (id: string) => void;
  selectSession: (key: string) => void;
  newSession: () => void;
  refresh: () => void;
  jumpToMessage: (index: number) => void;
}

export interface BuildPaletteItemsInput {
  agents: AgentEntry[];
  sessions: SessionEntry[];
  selectedAgent: string;
  query: string;
  messages: ChatMessage[];
  actions: PaletteActions;
}

const MAX_MESSAGE_RESULTS = 20;

export function buildPaletteItems({
  agents,
  sessions,
  selectedAgent,
  query,
  messages,
  actions,
}: BuildPaletteItemsInput): PaletteItem[] {
  const liveItems = [
    ...quickActions(actions),
    ...navigationItems(actions),
    ...agentItems(agents, selectedAgent, actions),
    ...sessionItems(sessions, selectedAgent, actions),
    ...messageItems(messages, query, actions),
  ];
  const recentItems = loadPaletteHistory().map((historyItem) => {
    const original = liveItems.find((item) => item.id === historyItem.id);
    return {
      id: historyItem.id,
      label: historyItem.label,
      section: "Recent",
      disabled: !original || original.disabled,
      onSelect: original?.onSelect ?? (() => undefined),
    };
  });

  return [...recentItems, ...liveItems];
}

function quickActions(actions: PaletteActions): PaletteItem[] {
  return [
    actionItem("qa-new-session", "New Session", "Quick Actions", actions.newSession, actions),
    actionItem("qa-refresh", "Refresh current tab", "Quick Actions", actions.refresh, actions),
  ];
}

function navigationItems(actions: PaletteActions): PaletteItem[] {
  return NAV_TABS.map((tab) => {
    const historyItem = { id: `nav-${tab.id}`, label: `Go to ${tab.label}`, section: "Navigation" };
    return {
      ...historyItem,
      disabled: tab.disabled,
      onSelect: () => selectAndRemember(historyItem, () => actions.navigate(tab.id), actions),
    };
  });
}

function agentItems(
  agents: AgentEntry[],
  selectedAgent: string,
  actions: PaletteActions,
): PaletteItem[] {
  return agents
    .filter((agent) => agent.id !== selectedAgent)
    .map((agent) => ({
      id: `agent-${agent.id}`,
      label: agent.label,
      section: "Agents",
      description: agent.is_default ? "default" : undefined,
      onSelect: () => selectAndRemember(
        { id: `agent-${agent.id}`, label: agent.label, section: "Agents" },
        () => actions.selectAgent(agent.id),
        actions,
      ),
    }));
}

function sessionItems(
  sessions: SessionEntry[],
  selectedAgent: string,
  actions: PaletteActions,
): PaletteItem[] {
  return sessions
    .filter((session) => session.agent_id === selectedAgent && !session.archived)
    .slice(0, 5)
    .map((session) => {
      const label = sessionTitle(session);
      return {
        id: `session-${session.session_key}`,
        label,
        section: "Sessions",
        description: session.channel,
        onSelect: () =>
          selectAndRemember(
            { id: `session-${session.session_key}`, label, section: "Sessions" },
            () => actions.selectSession(session.session_key),
            actions,
          ),
      };
    });
}

function messageItems(
  messages: ChatMessage[],
  query: string,
  actions: PaletteActions,
): PaletteItem[] {
  if (!query) return [];
  const q = query.toLowerCase();
  const results: PaletteItem[] = [];
  for (let index = 0; index < messages.length; index++) {
    if (results.length >= MAX_MESSAGE_RESULTS) break;
    const message = messages[index];
    if (message.message_kind === "tool_call") continue;
    if (!message.content.toLowerCase().includes(q)) continue;
    results.push({
      id: `msg-${message.id}`,
      // Full content: the palette filter re-matches against the label, and CSS
      // ellipsis handles display truncation.
      label: message.content,
      section: "Messages",
      description: message.sender_kind,
      onSelect: () => {
        actions.jumpToMessage(index);
        actions.close();
      },
    });
  }
  return results;
}

function actionItem(
  id: string,
  label: string,
  section: string,
  action: () => void,
  actions: PaletteActions,
): PaletteItem {
  return {
    id,
    label,
    section,
    onSelect: () => selectAndRemember({ id, label, section }, action, actions),
  };
}

function selectAndRemember(
  historyItem: { id: string; label: string; section: string },
  action: () => void,
  actions: PaletteActions,
): void {
  pushPaletteHistory(historyItem);
  action();
  actions.close();
}
