import type { AgentMemory, SleepRun, SleepRunDetail } from "./types";
import { apiFetch } from "./client";

/**
 * Fetches up to `limit` sleep runs, newest first.
 *
 * @param agentId restricts results to one agent; omit for all agents
 */
export async function fetchSleepRuns(
  agentId: string | undefined,
  limit: number,
  authToken: string,
): Promise<SleepRun[]> {
  const params = new URLSearchParams({ limit: String(limit) });
  if (agentId) params.set("agent_id", agentId);
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

export async function fetchAgentMemory(
  agentId: string,
  authToken: string,
): Promise<AgentMemory> {
  const data = await apiFetch<{ ok: boolean; memory: AgentMemory }>(
    `/api/agents/${encodeURIComponent(agentId)}/memory`,
    authToken,
  );
  return data.memory;
}
