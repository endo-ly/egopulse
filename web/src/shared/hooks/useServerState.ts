import { useCallback, useEffect, useRef, useState } from "react";

interface CacheEntry<T> {
  data: T | undefined;
  loading: boolean;
  error: Error | null;
  timestamp: number;
}

const cache = new Map<string, CacheEntry<unknown>>();
const listeners = new Map<string, Set<() => void>>();

function notify(key: string) {
  const set = listeners.get(key);
  if (set) {
    for (const fn of set) fn();
  }
}

/**
 * The sequence number of the newest request per key. A response is written to
 * the cache only while its own number is still the newest, so a slow request
 * can never land on top of a newer one, and a request that was already in
 * flight when the key was invalidated is dropped instead of resurrecting it.
 */
const requestSeq = new Map<string, number>();

function startRequest(key: string): number {
  const seq = (requestSeq.get(key) ?? 0) + 1;
  requestSeq.set(key, seq);
  return seq;
}

function isNewestRequest(key: string, seq: number): boolean {
  return (requestSeq.get(key) ?? 0) === seq;
}

export function invalidateQuery(key: string) {
  cache.delete(key);
  startRequest(key);
  notify(key);
}

export function invalidateQueries(prefix: string) {
  for (const key of cache.keys()) {
    if (key.startsWith(prefix)) {
      cache.delete(key);
      startRequest(key);
      notify(key);
    }
  }
}

export interface UseServerStateOptions {
  /** When set, refetch at this interval while the tab is visible. */
  pollIntervalMs?: number;
  /**
   * Refetch cached data once when the component mounts (or the key changes),
   * keeping the cached data visible until the fresh response arrives.
   */
  revalidateOnMount?: boolean;
}

export function useServerState<T>(
  key: string,
  fetcher: () => Promise<T>,
  options?: UseServerStateOptions,
): { data: T | undefined; loading: boolean; error: Error | null; invalidate: () => void } {
  const [, forceUpdate] = useState(0);
  const rerender = useCallback(() => forceUpdate((n) => n + 1), []);

  useEffect(() => {
    if (!listeners.has(key)) listeners.set(key, new Set());
    listeners.get(key)!.add(rerender);
    return () => {
      listeners.get(key)?.delete(rerender);
    };
  }, [key, rerender]);

  const entry = cache.get(key) as CacheEntry<T> | undefined;

  // Callers pass the fetcher as an inline closure, so its identity changes on
  // every render. Keeping the newest one in a ref lets `refetch` depend on the
  // key alone: a re-render must not restart the poll timer, or a screen that
  // re-renders more often than the interval would never poll.
  const fetcherRef = useRef(fetcher);
  useEffect(() => {
    fetcherRef.current = fetcher;
  });

  // Refresh in the background, keeping the previous data to avoid flicker.
  const refetch = useCallback(async () => {
    const seq = startRequest(key);
    const prev = cache.get(key) as CacheEntry<T> | undefined;
    cache.set(key, {
      data: prev?.data,
      // Mark loading on first fetch so the initial-fetch guard in the
      // mount effect does not re-trigger and loop.
      loading: prev?.loading ?? true,
      error: null,
      timestamp: Date.now(),
    });
    notify(key);
    try {
      const data = await fetcherRef.current();
      if (!isNewestRequest(key, seq)) return;
      cache.set(key, { data, loading: false, error: null, timestamp: Date.now() });
    } catch (e) {
      if (!isNewestRequest(key, seq)) return;
      const latest = cache.get(key) as CacheEntry<T> | undefined;
      cache.set(key, {
        data: latest?.data,
        loading: false,
        error: e instanceof Error ? e : new Error(String(e)),
        timestamp: Date.now(),
      });
    }
    notify(key);
  }, [key]);

  useEffect(() => {
    if (!entry || (entry.data === undefined && !entry.loading && !entry.error)) {
      refetch();
    }
  }, [entry, refetch]);

  // Revalidate cached data once per mount and key: `refetch` is stable for a
  // key, so this runs when the key changes rather than on every render.
  useEffect(() => {
    if (!options?.revalidateOnMount) return;
    const cached = cache.get(key);
    if (cached?.data !== undefined && !cached.loading) refetch();
  }, [key, options?.revalidateOnMount, refetch]);

  // Poll while the tab is visible, refetch immediately on visibility regain,
  // and pause while hidden to avoid wasted requests.
  useEffect(() => {
    const intervalMs = options?.pollIntervalMs;
    if (!intervalMs) return;
    let timer: ReturnType<typeof setInterval> | null = null;
    const stop = () => {
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    };
    const start = () => {
      if (timer !== null) return;
      timer = setInterval(refetch, intervalMs);
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") {
        refetch();
        start();
      } else {
        stop();
      }
    };
    if (document.visibilityState === "visible") start();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [options?.pollIntervalMs, refetch]);

  const invalidate = useCallback(() => {
    invalidateQuery(key);
    refetch();
  }, [key, refetch]);

  return {
    data: entry?.data,
    loading: entry?.loading ?? false,
    error: entry?.error ?? null,
    invalidate,
  };
}
