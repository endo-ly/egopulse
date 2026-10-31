import { useServerState } from "../../shared/hooks/useServerState";

/** Prefix shared by every Schedule screen query, so one refresh reloads them all. */
export const SCHEDULE_QUERY_PREFIX = "schedule:";

export interface ScheduleQueryOptions {
  /** Poll while the tab is visible (default). Off for data paged past its live head. */
  poll?: boolean;
}

/**
 * Fetches Schedule screen data. Runs start and finish without user action,
 * so data is revalidated on mount and polled while the tab is visible. The
 * auth token is part of the key so cached data never leaks across auth
 * identities.
 */
export function useScheduleQuery<T>(
  authToken: string,
  parts: string[],
  fetcher: () => Promise<T>,
  { poll = true }: ScheduleQueryOptions = {},
) {
  return useServerState(`${SCHEDULE_QUERY_PREFIX}${parts.join(":")}:${authToken}`, fetcher, {
    pollIntervalMs: poll ? 10_000 : undefined,
    revalidateOnMount: true,
  });
}
