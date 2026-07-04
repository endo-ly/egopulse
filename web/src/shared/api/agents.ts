import type { AgentEntry } from "./types";
import { apiFetch } from "./client";

export async function fetchAgents(authToken: string): Promise<AgentEntry[]> {
  const data = await apiFetch<{ ok: boolean; agents: AgentEntry[] }>("/api/agents", authToken);
  return data.agents;
}
