import { useCallback, useEffect, useRef } from "react";
import { invalidateQueries } from "../../shared/hooks/useServerState";

export interface SessionUpdatedPayload {
  sessionKey?: string;
  resync?: boolean;
}

export type Invalidate = (prefix: string) => void;

/**
 * Coalesces session-updated notifications into cache invalidations.
 *
 * A burst of events from one turn must not trigger one refetch per event, so
 * refreshes are batched. The session list always refreshes; each affected
 * history cache is dropped so switching to it later loads fresh data. A resync
 * drops every history cache because the server reported lost events.
 */
export function useSessionRefresh(
  invalidate: Invalidate = invalidateQueries,
): (payload: SessionUpdatedPayload) => void {
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingHistoryRef = useRef(new Set<string>());
  const resyncRef = useRef(false);

  useEffect(() => {
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, []);

  return useCallback(
    (payload: SessionUpdatedPayload) => {
      if (payload.resync) {
        resyncRef.current = true;
      } else if (payload.sessionKey) {
        pendingHistoryRef.current.add(payload.sessionKey);
      }
      if (timerRef.current !== null) return;
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        invalidate("sessions");
        if (resyncRef.current) {
          resyncRef.current = false;
          pendingHistoryRef.current.clear();
          invalidate("history");
          return;
        }
        for (const key of pendingHistoryRef.current) {
          invalidate(`history:${key}`);
        }
        pendingHistoryRef.current.clear();
      }, 100);
    },
    [invalidate],
  );
}
