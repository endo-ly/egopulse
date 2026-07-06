import { useEffect, useState, useCallback } from "react";

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

export function invalidateQuery(key: string) {
  cache.delete(key);
  notify(key);
}

export function invalidateQueries(prefix: string) {
  for (const key of cache.keys()) {
    if (key.startsWith(prefix)) {
      cache.delete(key);
      notify(key);
    }
  }
}

export interface UseServerStateOptions {
  /** When set, refetch at this interval while the tab is visible. */
  pollIntervalMs?: number;
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

  // Refresh in the background, keeping the previous data to avoid flicker.
  const refetch = useCallback(async () => {
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
      const data = await fetcher();
      cache.set(key, { data, loading: false, error: null, timestamp: Date.now() });
    } catch (e) {
      cache.set(key, {
        data: prev?.data,
        loading: false,
        error: e instanceof Error ? e : new Error(String(e)),
        timestamp: Date.now(),
      });
    }
    notify(key);
  }, [key, fetcher]);

  useEffect(() => {
    if (!entry || (entry.data === undefined && !entry.loading && !entry.error)) {
      refetch();
    }
  }, [entry, refetch]);

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
