import type { MemorySnapshot, SleepRun } from "./types";

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(1)}k`;
}

async function apiFetch<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { "Content-Type": "application/json" } });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error(String((data as { error?: string }).error ?? `HTTP ${res.status}`));
  }
  return data as T;
}

export async function fetchSleepAgents(): Promise<string[]> {
  const data = await apiFetch<{ ok: boolean; agents: Array<{ id: string }> }>("/api/agents");
  return data.agents.map((a) => a.id);
}

export async function fetchSleepRuns(agentId?: string): Promise<SleepRun[]> {
  const params = new URLSearchParams();
  if (agentId) params.set("agent_id", agentId);
  const qs = params.toString();
  const path = qs ? `/api/sleep/runs?${qs}` : "/api/sleep/runs";
  const data = await apiFetch<{ ok: boolean; runs: SleepRun[] }>(path);
  return data.runs;
}

export async function fetchSleepRunDetail(
  runId: string,
): Promise<{ run: SleepRun; snapshots: MemorySnapshot[] }> {
  const data = await apiFetch<{ ok: boolean; run: SleepRun; snapshots: MemorySnapshot[] }>(
    `/api/sleep/runs/${encodeURIComponent(runId)}`,
  );
  return { run: data.run, snapshots: data.snapshots };
}
