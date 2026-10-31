/** Agent screen sections; `overview` is the agent's root page. */
export const AGENT_SECTIONS = ["overview", "soul", "rules", "secret", "memory", "skills"] as const;
export type AgentSection = (typeof AGENT_SECTIONS)[number];

/** One Pulse or Sleep run. */
export interface ScheduleRun {
  kind: "pulse" | "sleep";
  id: string;
}

/**
 * The Schedule screen's section and its selection: Activity with the run
 * open beside the timeline, Pulse with the selected intention (null for the
 * first), or Sleep.
 */
export type ScheduleView =
  | { section: "activity"; run: ScheduleRun | null }
  | { section: "pulse"; intentionId: string | null }
  | { section: "sleep" };

export const SCHEDULE_ACTIVITY: ScheduleView = { section: "activity", run: null };

export type AppRoute =
  /** `sessionKey` is null when the session is left to auto-selection. */
  | { tab: "chat"; agentId: string; sessionKey: string | null }
  | { tab: "agent"; agentId: string; section: AgentSection }
  | { tab: "schedule"; agentId: string; view: ScheduleView }
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
    const view = parseScheduleView(tail);
    return view ? { tab: "schedule", agentId, view } : null;
  }
  // The overview is the agent root, so it has no segment of its own.
  if (scope !== "overview" && isAgentSection(scope) && tail.length === 0) {
    return { tab: "agent", agentId, section: scope };
  }
  return null;
}

function isRunKind(value: string | undefined): value is ScheduleRun["kind"] {
  return value === "pulse" || value === "sleep";
}

/**
 * Parses the segments after `schedule`: Activity (nothing, or
 * `runs/{pulse|sleep}/:id`), Pulse (`pulse`, `pulse/:intentionId`), or Sleep
 * (`sleep`).
 */
function parseScheduleView(tail: string[]): ScheduleView | null {
  const [first, second, third] = tail;
  if (tail.length === 0) return SCHEDULE_ACTIVITY;
  if (first === "runs") {
    return tail.length === 3 && isRunKind(second) && third
      ? { section: "activity", run: { kind: second, id: third } }
      : null;
  }
  if (first === "pulse" && tail.length <= 2) {
    if (tail.length === 1) return { section: "pulse", intentionId: null };
    return second ? { section: "pulse", intentionId: second } : null;
  }
  if (first === "sleep" && tail.length === 1) return { section: "sleep" };
  return null;
}

function scheduleViewPath(view: ScheduleView): string {
  switch (view.section) {
    case "activity":
      return view.run === null ? "" : `/runs/${view.run.kind}/${view.run.id}`;
    case "pulse":
      return view.intentionId === null ? "/pulse" : `/pulse/${view.intentionId}`;
    case "sleep":
      return "/sleep";
  }
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
      return `${base}/schedule${scheduleViewPath(route.view)}`;
  }
}
