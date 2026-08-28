import type { TabId } from "./navigation";

export interface AppRoute {
  tab: TabId;
  agentId: string;
  /** URL 上に明示された session key。未指定 (自動選択に任せる) 場合は null */
  sessionKey: string | null;
}

/** Agent-scoped tabs that live under /agents/:agentId */
const AGENT_SCOPED_TABS: ReadonlySet<TabId> = new Set<TabId>(["chat", "sleep"]);
const TAB_IDS: ReadonlySet<string> = new Set<string>([
  "chat",
  "sleep",
  "pulse",
  "metrics",
  "config",
]);

/**
 * Parses a pathname into an AppRoute following the URL structure defined in
 * docs/webui/layout.md §3.2. Returns null for paths that don't map to a view.
 */
export function parseRoute(pathname: string): AppRoute | null {
  const segments = pathname.split("/").filter(Boolean);

  if (segments.length === 0) {
    return { tab: "chat", agentId: "", sessionKey: null };
  }

  if (segments[0] === "agents") {
    const agentId = segments[1] ?? "";
    const scope = segments[2];
    if (!agentId || !scope || !AGENT_SCOPED_TABS.has(scope as TabId)) {
      return null;
    }
    if (scope === "chat") {
      if (segments.length === 3) {
        return { tab: "chat", agentId, sessionKey: null };
      }
      if (segments.length === 5 && segments[3] === "s" && segments[4]) {
        return { tab: "chat", agentId, sessionKey: segments[4] };
      }
      return null;
    }
    if (segments.length === 3) {
      return { tab: "sleep", agentId, sessionKey: null };
    }
    return null;
  }

  if (segments.length === 1 && TAB_IDS.has(segments[0])) {
    return { tab: segments[0] as TabId, agentId: "", sessionKey: null };
  }

  return null;
}

/**
 * Builds the canonical pathname for a route. Returns null when the route
 * cannot be represented (e.g. an agent-scoped tab without an agent).
 */
export function buildRoutePath(route: AppRoute): string | null {
  if (route.tab === "chat" || route.tab === "sleep") {
    if (!route.agentId) {
      // "/" is the boot view: chat on the default agent, no explicit session.
      return route.tab === "chat" && !route.sessionKey ? "/" : null;
    }
    const base = `/agents/${route.agentId}/${route.tab}`;
    if (route.tab === "chat" && route.sessionKey) {
      return `${base}/s/${route.sessionKey}`;
    }
    return base;
  }
  return `/${route.tab}`;
}
