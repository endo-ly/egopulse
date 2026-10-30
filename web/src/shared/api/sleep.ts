import type { SleepRun, SleepRunDetail } from "./types";
import { apiFetch } from "./client";

/** Fetches up to `limit` sleep runs of one agent, newest first. */
export async function fetchSleepRuns(
  agentId: string,
  limit: number,
  authToken: string,
): Promise<SleepRun[]> {
  const params = new URLSearchParams({ agent_id: agentId, limit: String(limit) });
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
