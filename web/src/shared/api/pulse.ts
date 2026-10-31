import type { AgentPulse, PulseRun, PulseRunDetail, PulseRunStatus, RunPage } from "./types";
import { apiFetch } from "./client";

export async function fetchAgentPulse(agentId: string, authToken: string): Promise<AgentPulse> {
  const data = await apiFetch<{ ok: boolean; pulse: AgentPulse }>(
    `/api/agents/${encodeURIComponent(agentId)}/pulse`,
    authToken,
  );
  return data.pulse;
}

export interface PulseRunsQuery {
  limit: number;
  /** Keeps only these statuses; every status when omitted. */
  statuses?: readonly PulseRunStatus[];
  /** Pages back to runs started before this instant. */
  before?: string;
}

/** Fetches one page of an agent's pulse runs, newest first. */
export async function fetchPulseRuns(
  agentId: string,
  authToken: string,
  { limit, statuses = [], before }: PulseRunsQuery,
): Promise<RunPage<PulseRun>> {
  const params = new URLSearchParams({ agent_id: agentId, limit: String(limit) });
  if (statuses.length > 0) params.set("status", statuses.join(","));
  if (before !== undefined) params.set("before", before);
  const data = await apiFetch<{ ok: boolean } & RunPage<PulseRun>>(
    `/api/pulse/runs?${params}`,
    authToken,
  );
  return { runs: data.runs, has_more: data.has_more };
}

export async function fetchPulseRunDetail(runId: string, authToken: string): Promise<PulseRunDetail> {
  const data = await apiFetch<{ ok: boolean; run: PulseRunDetail }>(
    `/api/pulse/runs/${encodeURIComponent(runId)}`,
    authToken,
  );
  return data.run;
}
