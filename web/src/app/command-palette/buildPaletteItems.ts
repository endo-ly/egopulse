import type { TabId } from "../navigation";
import type { AgentEntry, SessionEntry } from "../../shared/api/types";
import { loadPaletteHistory, pushPaletteHistory } from "./paletteHistory";

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
}

export interface BuildPaletteItemsInput {
  agents: AgentEntry[];
  sessions: SessionEntry[];
  selectedAgent: string;
  actions: PaletteActions;
}

const TAB_LABELS: Record<TabId, string> = {
  chat: "Chat",
  sleep: "Sleep",
  pulse: "Pulse",
  metrics: "Metrics",
  config: "Config",
};

const DISABLED_TABS: TabId[] = ["pulse", "metrics", "config"];

export function buildPaletteItems({
  agents,
  sessions,
  selectedAgent,
  actions,
}: BuildPaletteItemsInput): PaletteItem[] {
  const liveItems = [
    ...quickActions(actions),
    ...navigationItems(actions),
    ...agentItems(agents, selectedAgent, actions),
    ...sessionItems(sessions, selectedAgent, actions),
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
  return (Object.keys(TAB_LABELS) as TabId[]).map((tab) => ({
    id: `nav-${tab}`,
    label: `Go to ${TAB_LABELS[tab]}`,
    section: "Navigation",
    disabled: DISABLED_TABS.includes(tab),
    onSelect: () => selectAndRemember(
      { id: `nav-${tab}`, label: `Go to ${TAB_LABELS[tab]}`, section: "Navigation" },
      () => actions.navigate(tab),
      actions,
    ),
  }));
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
    .filter((session) => session.agent_id === selectedAgent)
    .slice(0, 5)
    .map((session) => ({
      id: `session-${session.session_key}`,
      label: session.label,
      section: "Sessions",
      description: session.channel,
      onSelect: () => selectAndRemember(
        { id: `session-${session.session_key}`, label: session.label, section: "Sessions" },
        () => actions.selectSession(session.session_key),
        actions,
      ),
    }));
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
