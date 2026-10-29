import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { ChatMessage, HistoryPage } from "../../shared/api/types";
import { fetchHistory } from "../../shared/api/history";
import { useServerState } from "../../shared/hooks/useServerState";
import {
  gapBounds,
  mergeHistoryMessages,
  prependHistoryMessages,
} from "./historyPaging";

const EMPTY_MESSAGES: ChatMessage[] = [];
const HISTORY_POLL_MS = 60_000;
const MAX_OLDER_PAGE_FETCHES = 5;
const MAX_GAP_PAGE_FETCHES = 10;

interface LoadedHistory {
  sessionKey: string;
  messages: ChatMessage[];
  /** Exclusive cursor for the next older page; null at the beginning. */
  oldestCursor: number | null;
  /** Highest seq covered contiguously from the oldest loaded message. */
  newestSeq: number | null;
  loadedOlder: boolean;
  /** Increments whenever older messages are prepended (scroll anchoring). */
  loadSeq: number;
}

interface GapFill {
  messages: ChatMessage[];
  /** False when the page cap was hit before the gap was closed. */
  complete: boolean;
}

function emptyHistory(sessionKey: string): LoadedHistory {
  return {
    sessionKey,
    messages: EMPTY_MESSAGES,
    oldestCursor: null,
    newestSeq: null,
    loadedOlder: false,
    loadSeq: 0,
  };
}

function seedHistory(sessionKey: string, page: HistoryPage): LoadedHistory {
  return {
    sessionKey,
    messages: page.messages,
    oldestCursor: page.nextBeforeSeq,
    newestSeq: page.newestSeq,
    loadedOlder: false,
    loadSeq: 0,
  };
}

function maxSeq(current: number | null, incoming: number | null): number | null {
  if (current === null) return incoming;
  if (incoming === null) return current;
  return Math.max(current, incoming);
}

function applyNewestPage(
  previous: LoadedHistory,
  sessionKey: string,
  page: HistoryPage,
  gap: GapFill,
): LoadedHistory {
  if (previous.sessionKey !== sessionKey) return seedHistory(sessionKey, page);

  // A chat with no committed data yet adopts the first populated page,
  // including its older cursor.
  if (previous.messages.length === 0 && previous.newestSeq === null) {
    return {
      ...seedHistory(sessionKey, page),
      loadedOlder: previous.loadedOlder,
    };
  }

  // Two overlapping refetches can resolve out of order. A page whose newest
  // seq is older than the loaded top (or an empty page after data) must not
  // roll the conversation back; the next poll delivers the current page.
  if (
    previous.newestSeq !== null &&
    (page.newestSeq === null || page.newestSeq < previous.newestSeq)
  ) {
    return previous;
  }

  // Reaching the first row means this page supersedes every loaded slice:
  // no older rows exist, so the chat fits in one page.
  if (page.nextBeforeSeq === null) {
    return {
      ...previous,
      messages: page.messages,
      oldestCursor: null,
      newestSeq: page.newestSeq,
    };
  }

  // Applying only part of a gap would append older rows after rows that are
  // already newer. Keep the current view and let the next poll retry the
  // whole range.
  if (!gap.complete) return previous;

  let messages = previous.messages;
  if (gap.messages.length > 0) {
    messages = mergeHistoryMessages(messages, gap.messages);
  }
  messages = mergeHistoryMessages(messages, page.messages);

  return {
    ...previous,
    messages,
    newestSeq: maxSeq(previous.newestSeq, page.newestSeq),
  };
}

async function fetchGapMessages(
  authToken: string,
  sessionKey: string,
  from: number,
  downTo: number,
): Promise<GapFill> {
  const pages: ChatMessage[][] = [];
  let cursor: number | null = from;
  let fetched = 0;
  while (cursor !== null && cursor > downTo + 1 && fetched < MAX_GAP_PAGE_FETCHES) {
    const page = await fetchHistory(authToken, sessionKey, cursor);
    if (page.messages.length > 0) pages.push(page.messages);
    cursor = page.nextBeforeSeq;
    fetched += 1;
  }
  return {
    messages: pages.reverse().flat(),
    complete: cursor === null || cursor <= downTo + 1,
  };
}

export interface ChatHistory {
  messages: ChatMessage[];
  error: Error | null;
  /** Loads one older page below the current oldest message. */
  loadOlder: () => Promise<void>;
  /** Retries an older page after a failure. */
  retryOlder: () => Promise<void>;
  loadingOlder: boolean;
  olderError: string | null;
  atHistoryStart: boolean;
  /** Bumped after a prepend so the timeline can keep the viewport anchored. */
  loadSeq: number;
  invalidate: () => void;
}

/**
 * Owns the paginated committed history of one session.
 *
 * The newest page keeps the existing server-state cache (polling and
 * invalidation). Older pages are prepended without dropping anything, and a
 * newest-page jump fills the skipped seq range before merging.
 */
export function useChatHistory(sessionKey: string, authToken: string): ChatHistory {
  const fetcher = useCallback(
    () => fetchHistory(authToken, sessionKey),
    [authToken, sessionKey],
  );
  const newest = useServerState(`history:${sessionKey}`, fetcher, {
    pollIntervalMs: HISTORY_POLL_MS,
  });

  const [state, setState] = useState<LoadedHistory>(() => emptyHistory(sessionKey));
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [olderError, setOlderError] = useState<string | null>(null);

  const stateRef = useRef(state);
  const sessionRef = useRef(sessionKey);
  const loadingOlderRef = useRef(false);
  const failedOlderRef = useRef(false);
  const syncTokenRef = useRef(0);

  // Layout effects run before child passive effects (the timeline's
  // viewport-fill check calls loadOlder), so the refs are current there.
  useLayoutEffect(() => {
    stateRef.current = state;
  }, [state]);
  useLayoutEffect(() => {
    sessionRef.current = sessionKey;
  }, [sessionKey]);

  // Reset before the newest-page sync effect so a session switch cannot
  // merge the previous session's rows.
  useEffect(() => {
    stateRef.current = emptyHistory(sessionKey);
    setState(stateRef.current);
    setLoadingOlder(false);
    setOlderError(null);
    loadingOlderRef.current = false;
    failedOlderRef.current = false;
    syncTokenRef.current += 1;
  }, [sessionKey]);

  useEffect(() => {
    const page = newest.data;
    if (!page) return;

    const token = ++syncTokenRef.current;
    const current = stateRef.current;
    if (current.sessionKey !== sessionKey) {
      setState(seedHistory(sessionKey, page));
      return;
    }

    const gap = gapBounds(current.newestSeq, page.nextBeforeSeq);
    if (!gap) {
      setState((previous) =>
        applyNewestPage(previous, sessionKey, page, {
          messages: [],
          complete: true,
        }),
      );
      return;
    }

    void (async () => {
      try {
        const filled = await fetchGapMessages(
          authToken,
          sessionKey,
          gap.from,
          gap.downTo,
        );
        if (token !== syncTokenRef.current || sessionRef.current !== sessionKey) {
          return;
        }
        setState((previous) => applyNewestPage(previous, sessionKey, page, filled));
      } catch {
        // Keep the current view; the next poll retries the same gap.
      }
    })();
  }, [newest.data, sessionKey, authToken]);

  const requestOlder = useCallback(async () => {
    if (loadingOlderRef.current) return;
    const current = stateRef.current;
    if (current.sessionKey !== sessionKey || current.oldestCursor === null) {
      return;
    }

    loadingOlderRef.current = true;
    setLoadingOlder(true);
    setOlderError(null);
    const requestSession = sessionKey;
    const expectedCursor = current.oldestCursor;

    try {
      const pages: ChatMessage[][] = [];
      let cursor: number | null = current.oldestCursor;
      let fetched = 0;
      // A page can contain only hidden tool previews; keep walking until
      // something visible arrives or the cursor reaches the beginning.
      while (cursor !== null && fetched < MAX_OLDER_PAGE_FETCHES) {
        const page = await fetchHistory(authToken, sessionKey, cursor);
        cursor = page.nextBeforeSeq;
        fetched += 1;
        if (page.messages.length > 0) {
          pages.push(page.messages);
          break;
        }
      }
      if (sessionRef.current !== requestSession) return;

      const older = pages.reverse().flat();
      failedOlderRef.current = false;
      setState((previous) => {
        if (previous.sessionKey !== sessionKey) return previous;
        // A newest-page refetch can rebase the history while this page is in
        // flight. The old chain is superseded, so drop the result.
        if (previous.oldestCursor !== expectedCursor) return previous;
        const messages =
          older.length > 0
            ? prependHistoryMessages(previous.messages, older)
            : previous.messages;
        return {
          ...previous,
          messages,
          oldestCursor: cursor,
          loadedOlder: true,
          loadSeq:
            messages === previous.messages ? previous.loadSeq : previous.loadSeq + 1,
        };
      });
    } catch (error) {
      if (
        sessionRef.current === requestSession &&
        stateRef.current.oldestCursor === expectedCursor
      ) {
        failedOlderRef.current = true;
        setOlderError(error instanceof Error ? error.message : String(error));
      }
    } finally {
      loadingOlderRef.current = false;
      if (sessionRef.current === requestSession) {
        setLoadingOlder(false);
      }
    }
  }, [authToken, sessionKey]);

  const loadOlder = useCallback(async () => {
    // Scroll/fill triggers must not hammer a failing endpoint; the user
    // retries explicitly from the error banner.
    if (failedOlderRef.current) return;
    await requestOlder();
  }, [requestOlder]);

  const retryOlder = useCallback(async () => {
    failedOlderRef.current = false;
    setOlderError(null);
    await requestOlder();
  }, [requestOlder]);

  const current = state.sessionKey === sessionKey ? state : null;
  const messages = current?.messages ?? EMPTY_MESSAGES;

  return {
    messages,
    error: newest.error,
    loadOlder,
    retryOlder,
    loadingOlder,
    olderError,
    atHistoryStart: Boolean(
      current?.loadedOlder && current.oldestCursor === null && !loadingOlder,
    ),
    loadSeq: current?.loadSeq ?? 0,
    invalidate: newest.invalidate,
  };
}
