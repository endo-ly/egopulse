import type { RunPage, SleepOutlook, SleepRun, SleepRunDetail, SleepRunStatus } from "./types";
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

export interface SleepRunsQuery {
  limit: number;
  /** Keeps only these statuses; every status when omitted. */
  statuses?: readonly SleepRunStatus[];
  /** Cursor below which to continue listing: `{started_at}|{id}` of the oldest run already shown. */
  before?: string;
}

/** Fetches one page of an agent's sleep runs, newest first. */
export async function fetchSleepRuns(
  agentId: string,
  authToken: string,
  { limit, statuses = [], before }: SleepRunsQuery,
): Promise<RunPage<SleepRun>> {
  const params = new URLSearchParams({ agent_id: agentId, limit: String(limit) });
  if (statuses.length > 0) params.set("status", statuses.join(","));
  if (before !== undefined) params.set("before", before);
  const data = await apiFetch<{ ok: boolean } & RunPage<SleepRun>>(
    `/api/sleep/runs?${params}`,
    authToken,
  );
  return { runs: data.runs, has_more: data.has_more };
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
