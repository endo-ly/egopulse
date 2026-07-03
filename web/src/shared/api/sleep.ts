import type { MemorySnapshot, SleepRun } from "./types";
import { apiFetch } from "./client";

export async function fetchSleepAgents(authToken = ""): Promise<string[]> {
  const data = await apiFetch<{ ok: boolean; agents: Array<{ id: string }> }>(
    "/api/agents",
    authToken,
  );
  return data.agents.map((agent) => agent.id);
}

export async function fetchSleepRuns(agentId?: string, authToken = ""): Promise<SleepRun[]> {
  const params = new URLSearchParams();
  if (agentId) params.set("agent_id", agentId);
  const qs = params.toString();
  const path = qs ? `/api/sleep/runs?${qs}` : "/api/sleep/runs";
  const data = await apiFetch<{ ok: boolean; runs: SleepRun[] }>(path, authToken);
  return data.runs;
}

export async function fetchSleepRunDetail(
  runId: string,
  authToken = "",
): Promise<{ run: SleepRun; snapshots: MemorySnapshot[] }> {
  const data = await apiFetch<{ ok: boolean; run: SleepRun; snapshots: MemorySnapshot[] }>(
    `/api/sleep/runs/${encodeURIComponent(runId)}`,
    authToken,
  );
  return { run: data.run, snapshots: data.snapshots };
}
