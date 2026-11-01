/** Screens scoped to the selected agent. */
export type AgentTabId = "chat" | "agent" | "schedule";
/** Runtime-wide screens, independent of the selected agent. */
export type GlobalTabId = "metrics" | "config";
export type TabId = AgentTabId | GlobalTabId;

export interface NavTab<T extends TabId = TabId> {
  id: T;
  label: string;
  disabled: boolean;
}

/** Agent screens in display order (sidebar nav). */
export const AGENT_TABS: NavTab<AgentTabId>[] = [
  { id: "chat", label: "Chat", disabled: false },
  { id: "agent", label: "Agent", disabled: false },
  { id: "schedule", label: "Schedule", disabled: false },
];

/** Global screens in display order (sidebar footer). */
export const GLOBAL_TABS: NavTab<GlobalTabId>[] = [
  { id: "metrics", label: "Metrics", disabled: false },
  { id: "config", label: "Config", disabled: false },
];

export const NAV_TABS: NavTab[] = [...AGENT_TABS, ...GLOBAL_TABS];

export function isAgentTab(tab: TabId): tab is AgentTabId {
  return AGENT_TABS.some((entry) => entry.id === tab);
}
