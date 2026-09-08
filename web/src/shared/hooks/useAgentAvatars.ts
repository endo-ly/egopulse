import { useEffect, useMemo, useRef, useState } from "react";

import { apiFetchBlob } from "../api/client";
import type { AgentEntry } from "../api/types";

interface AvatarEntry {
  /** The avatar_url the object URL was created from (cache-busted). */
  source: string;
  url: string;
}

/**
 * Resolves each agent's authorized avatar URL into an object URL usable by
 * `<img>` tags: avatar endpoints sit behind bearer auth, which image elements
 * cannot send. Object URLs are refetched only when the server-side version
 * changes and are revoked when the agent loses its avatar.
 */
export function useAgentAvatars(
  agents: AgentEntry[],
  authToken: string,
): Record<string, string> {
  const [avatars, setAvatars] = useState<Record<string, AvatarEntry>>({});
  const agentsRef = useRef(agents);
  agentsRef.current = agents;

  // Reconcile on id:url content, not array identity, so a parent re-render
  // with a freshly built list never triggers a redundant refetch loop.
  const avatarKey = useMemo(
    () =>
      agents.map((agent) => `${agent.id}:${agent.avatar_url ?? ""}`).join("|"),
    [agents],
  );

  useEffect(() => {
    let cancelled = false;

    void (async () => {
      const previous = avatars;
      const next: Record<string, AvatarEntry> = {};
      for (const agent of agentsRef.current) {
        if (!agent.avatar_url) continue;
        const existing = previous[agent.id];
        if (existing && existing.source === agent.avatar_url) {
          next[agent.id] = existing;
          continue;
        }
        try {
          const blob = await apiFetchBlob(agent.avatar_url, authToken);
          if (cancelled) return;
          next[agent.id] = {
            source: agent.avatar_url,
            url: URL.createObjectURL(blob),
          };
        } catch {
          // Avatar failures are cosmetic; the letter avatar remains.
        }
      }

      for (const [agentId, entry] of Object.entries(previous)) {
        if (next[agentId]?.url !== entry.url) {
          URL.revokeObjectURL(entry.url);
        }
      }
      if (!cancelled) {
        setAvatars(next);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [avatarKey, authToken]);

  return Object.fromEntries(
    Object.entries(avatars).map(([agentId, entry]) => [agentId, entry.url]),
  );
}
