import type { AgentPulse, PulseRun, PulseRunDetail, PulseRunStatus } from "./types";
import { apiFetch } from "./client";

export async function fetchAgentPulse(agentId: string, authToken: string): Promise<AgentPulse> {
  const data = await apiFetch<{ ok: boolean; pulse: AgentPulse }>(
    `/api/agents/${encodeURIComponent(agentId)}/pulse`,
    authToken,
  );
  return data.pulse;
}

/** Fetches up to `limit` pulse runs of one agent, newest first. */
export async function fetchPulseRuns(
  agentId: string,
  statuses: readonly PulseRunStatus[],
  limit: number,
  authToken: string,
): Promise<PulseRun[]> {
  const params = new URLSearchParams({ agent_id: agentId, limit: String(limit) });
  if (statuses.length > 0) params.set("status", statuses.join(","));
  const data = await apiFetch<{ ok: boolean; runs: PulseRun[] }>(
    `/api/pulse/runs?${params}`,
    authToken,
  );
  return data.runs;
}

export async function fetchPulseRunDetail(runId: string, authToken: string): Promise<PulseRunDetail> {
  const data = await apiFetch<{ ok: boolean; run: PulseRunDetail }>(
    `/api/pulse/runs/${encodeURIComponent(runId)}`,
    authToken,
  );
  return data.run;
}
