/** Agent screen sections; `overview` is the agent's root page. */
export const AGENT_SECTIONS = ["overview", "soul", "rules", "secret", "memory", "skills"] as const;
export type AgentSection = (typeof AGENT_SECTIONS)[number];

export type AppRoute =
  /** `sessionKey` is null when the session is left to auto-selection. */
  | { tab: "chat"; agentId: string; sessionKey: string | null }
  | { tab: "agent"; agentId: string; section: AgentSection }
  /** `runId` is the selected sleep run; null when none is selected. */
  | { tab: "schedule"; agentId: string; runId: string | null }
  | { tab: "metrics" }
  | { tab: "config" };

function isAgentSection(value: string): value is AgentSection {
  return (AGENT_SECTIONS as readonly string[]).includes(value);
}

function parseAgentScoped(agentId: string, rest: string[]): AppRoute | null {
  const [scope, ...tail] = rest;
  if (scope === undefined) {
    return { tab: "agent", agentId, section: "overview" };
  }
  if (scope === "chat") {
    if (tail.length === 0) return { tab: "chat", agentId, sessionKey: null };
    if (tail.length === 2 && tail[0] === "s" && tail[1]) {
      return { tab: "chat", agentId, sessionKey: tail[1] };
    }
    return null;
  }
  if (scope === "schedule") {
    if (tail.length === 0) return { tab: "schedule", agentId, runId: null };
    if (tail.length === 2 && tail[0] === "sleep" && tail[1]) {
      return { tab: "schedule", agentId, runId: tail[1] };
    }
    return null;
  }
  // The overview is the agent root, so it has no segment of its own.
  if (scope !== "overview" && isAgentSection(scope) && tail.length === 0) {
    return { tab: "agent", agentId, section: scope };
  }
  return null;
}

/**
 * Parses a pathname into an AppRoute following the URL structure defined in
 * docs/webui/layout.md §4. Returns null for paths that don't map to a view.
 */
export function parseRoute(pathname: string): AppRoute | null {
  // A single trailing slash is tolerated; any other empty segment
  // (e.g. `/agents//chat`) does not map to a view.
  const segments = pathname.replace(/\/$/, "").split("/").slice(1);
  if (segments.some((segment) => segment === "")) return null;

  if (segments.length === 0) {
    return { tab: "chat", agentId: "", sessionKey: null };
  }
  if (segments[0] === "agents") {
    const [, agentId, ...rest] = segments;
    return agentId ? parseAgentScoped(agentId, rest) : null;
  }
  if (segments.length === 1 && (segments[0] === "metrics" || segments[0] === "config")) {
    return { tab: segments[0] };
  }
  return null;
}

/**
 * Builds the canonical pathname for a route. Returns null when the route
 * cannot be represented (e.g. an agent-scoped route without an agent).
 */
export function buildRoutePath(route: AppRoute): string | null {
  if (route.tab === "metrics" || route.tab === "config") {
    return `/${route.tab}`;
  }
  if (!route.agentId) {
    // "/" is the boot view: chat on the default agent, no explicit session.
    return route.tab === "chat" && !route.sessionKey ? "/" : null;
  }
  const base = `/agents/${route.agentId}`;
  switch (route.tab) {
    case "chat":
      return route.sessionKey ? `${base}/chat/s/${route.sessionKey}` : `${base}/chat`;
    case "agent":
      return route.section === "overview" ? base : `${base}/${route.section}`;
    case "schedule":
      return route.runId ? `${base}/schedule/sleep/${route.runId}` : `${base}/schedule`;
  }
}
