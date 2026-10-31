import type { SleepOutlook, SleepRun, SleepRunDetail, SleepRunStatus } from "./types";
import { apiFetch } from "./client";

/**
 * Statuses of sleep runs that did work. A skipped check decided not to run,
 * so it says nothing about whether sleep works and is not activity.
 */
export const WORKED_SLEEP_STATUSES: readonly SleepRunStatus[] = [
  "running",
  "success",
  "partial_failure",
  "failed",
];

/**
 * Fetches up to `limit` sleep runs of one agent, newest first. A non-empty
 * `statuses` keeps only those statuses.
 */
export async function fetchSleepRuns(
  agentId: string,
  limit: number,
  authToken: string,
  statuses: readonly SleepRunStatus[] = [],
): Promise<SleepRun[]> {
  const params = new URLSearchParams({ agent_id: agentId, limit: String(limit) });
  if (statuses.length > 0) params.set("status", statuses.join(","));
  const data = await apiFetch<{ ok: boolean; runs: SleepRun[] }>(
    `/api/sleep/runs?${params}`,
    authToken,
  );
  return data.runs;
}

export function fetchSleepRunDetail(runId: string, authToken: string): Promise<SleepRunDetail> {
  return apiFetch<SleepRunDetail>(
    `/api/sleep/runs/${encodeURIComponent(runId)}`,
    authToken,
  );
}

export async function fetchSleepOutlook(agentId: string, authToken: string): Promise<SleepOutlook> {
  const data = await apiFetch<{ ok: boolean; outlook: SleepOutlook }>(
    `/api/agents/${encodeURIComponent(agentId)}/sleep/next`,
    authToken,
  );
  return data.outlook;
}
