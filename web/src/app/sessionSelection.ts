import type { SessionEntry } from "../shared/api/types";

/** Session key used when a new-conversation state is shown. */
export const DEFAULT_SESSION_KEY = "main";

/**
 * Picks the session to show while the user has not chosen one explicitly.
 * Archived sessions are hidden from the initial pick; when the agent has no
 * active sessions the new-conversation state is shown instead.
 */
export function resolveAutoSessionKey(
  sessions: SessionEntry[],
  agentId: string,
  currentKey: string,
): string {
  const currentIsActive = sessions.some(
    (session) =>
      session.session_key === currentKey &&
      session.agent_id === agentId &&
      !session.archived,
  );
  if (currentIsActive) return currentKey;
  const firstActive = sessions.find(
    (session) => session.agent_id === agentId && !session.archived,
  );
  return firstActive?.session_key ?? DEFAULT_SESSION_KEY;
}
