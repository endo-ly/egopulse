import { fetchMetrics, fetchRuntimeStatus, type MetricsRange } from "../../shared/api/metrics";
import { useServerState } from "../../shared/hooks/useServerState";

/** Prefix shared by every Metrics query, so one refresh reloads them all. */
export const METRICS_QUERY_PREFIX = "metrics:";

/** Current runtime health; changes on its own, so it is polled briskly. */
export function useRuntimeStatus(authToken: string) {
  return useServerState(`${METRICS_QUERY_PREFIX}status:${authToken}`, () => fetchRuntimeStatus(authToken), {
    pollIntervalMs: 10_000,
    revalidateOnMount: true,
  });
}

/** DB aggregates for a range; a minute of staleness is fine for trends. */
export function useMetricsReport(authToken: string, range: MetricsRange, agentId: string | null) {
  return useServerState(
    `${METRICS_QUERY_PREFIX}report:${range}:${agentId ?? ""}:${authToken}`,
    () => fetchMetrics(authToken, range, agentId),
    { pollIntervalMs: 60_000, revalidateOnMount: true },
  );
}
