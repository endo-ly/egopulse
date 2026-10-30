import { useServerState } from "../../shared/hooks/useServerState";

/** Prefix shared by every Agent screen query, so one refresh reloads them all. */
export const AGENT_QUERY_PREFIX = "agent-screen:";

/**
 * Fetches Agent screen data. Prompt files and memory change outside the UI
 * (Sleep, manual edits), so cached data is shown at once and revalidated on
 * every mount. The auth token is part of the key so cached data never leaks
 * across auth identities.
 */
export function useAgentQuery<T>(authToken: string, parts: string[], fetcher: () => Promise<T>) {
  return useServerState(`${AGENT_QUERY_PREFIX}${parts.join(":")}:${authToken}`, fetcher, {
    revalidateOnMount: true,
  });
}
