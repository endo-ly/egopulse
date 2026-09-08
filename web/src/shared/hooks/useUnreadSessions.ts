import { useEffect, useMemo, useState } from "react";
import type { SessionEntry } from "../api/types";

const STORAGE_KEY = "egopulse.lastSeen.v1";

function loadLastSeen(): Record<string, number> {
  try {
    const storage = globalThis.localStorage;
    if (!storage) return {};
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const lastSeen: Record<string, number> = {};
    for (const [key, value] of Object.entries(
      parsed as Record<string, unknown>,
    )) {
      if (typeof value === "number" && Number.isFinite(value)) {
        lastSeen[key] = value;
      }
    }
    return lastSeen;
  } catch {
    return {};
  }
}

/**
 * Tracks sessions with messages newer than the last view.
 *
 * Read state lives in `localStorage` so it survives reloads on the same
 * device. A session counts as unread when its `last_message_time` advanced
 * after the first sight and the session is not currently selected; opening
 * a session marks it read. Unknown sessions initialize as read so the first
 * load never floods every dot.
 */
export function useUnreadSessions(
  sessions: SessionEntry[],
  selectedSession: string,
): ReadonlySet<string> {
  const [lastSeen, setLastSeen] =
    useState<Record<string, number>>(loadLastSeen);

  useEffect(() => {
    setLastSeen((prev) => {
      let changed = false;
      const next = { ...prev };
      for (const session of sessions) {
        const known = next[session.session_key];
        if (known === undefined) {
          next[session.session_key] = session.last_message_time;
          changed = true;
        } else if (
          session.session_key === selectedSession &&
          session.last_message_time > known
        ) {
          next[session.session_key] = session.last_message_time;
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  }, [sessions, selectedSession]);

  useEffect(() => {
    try {
      globalThis.localStorage?.setItem(STORAGE_KEY, JSON.stringify(lastSeen));
    } catch {
      // Private mode etc: unread still works for the session lifetime.
    }
  }, [lastSeen]);

  return useMemo(() => {
    const unread = new Set<string>();
    for (const session of sessions) {
      if (session.session_key === selectedSession) continue;
      const seen = lastSeen[session.session_key] ?? session.last_message_time;
      if (session.last_message_time > seen) {
        unread.add(session.session_key);
      }
    }
    return unread;
  }, [sessions, selectedSession, lastSeen]);
}
