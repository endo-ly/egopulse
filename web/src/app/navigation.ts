export type TabId = "chat" | "sleep" | "pulse" | "metrics" | "config";

export interface NavTab {
  id: TabId;
  label: string;
  disabled: boolean;
}

/** Primary navigation entries in display order. Disabled tabs are placeholders. */
export const NAV_TABS: NavTab[] = [
  { id: "chat", label: "Chat", disabled: false },
  { id: "sleep", label: "Sleep", disabled: false },
  { id: "pulse", label: "Pulse", disabled: true },
  { id: "metrics", label: "Metrics", disabled: true },
  { id: "config", label: "Config", disabled: true },
];
