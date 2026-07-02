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

export function useServerState<T>(
  key: string,
  fetcher: () => Promise<T>,
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

  const doFetch = useCallback(async () => {
    if (entry && entry.data !== undefined) return;
    cache.set(key, { data: undefined, loading: true, error: null, timestamp: Date.now() });
    notify(key);
    try {
      const data = await fetcher();
      cache.set(key, { data, loading: false, error: null, timestamp: Date.now() });
    } catch (e) {
      cache.set(key, {
        data: undefined,
        loading: false,
        error: e instanceof Error ? e : new Error(String(e)),
        timestamp: Date.now(),
      });
    }
    notify(key);
  }, [key, fetcher, entry]);

  useEffect(() => {
    if (!entry || (entry.data === undefined && !entry.loading)) {
      doFetch();
    }
  }, [entry, doFetch]);

  const invalidate = useCallback(() => {
    invalidateQuery(key);
    doFetch();
  }, [key, doFetch]);

  return {
    data: entry?.data,
    loading: entry?.loading ?? false,
    error: entry?.error ?? null,
    invalidate,
  };
}
