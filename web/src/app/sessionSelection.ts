import type { SessionEntry } from "../shared/api/types";

/**
 * Picks the session to show while the user has not chosen one explicitly.
 * Archived sessions are hidden from the initial pick; when the agent has no
 * active sessions, the fresh draft key is shown instead of reusing a
 * previously persisted session.
 */
export function resolveAutoSessionKey(
  sessions: SessionEntry[],
  agentId: string,
  currentKey: string,
  draftSessionKey: string,
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
  return firstActive?.session_key ?? draftSessionKey;
}
