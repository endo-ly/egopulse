import type { AgentEntry } from "./types";
import { apiFetch } from "./client";

export async function fetchAgents(authToken: string): Promise<AgentEntry[]> {
  const data = await apiFetch<{ ok: boolean; agents: AgentEntry[] }>("/api/agents", authToken);
  return data.agents;
}

/** Uploads an avatar image blob (already resized by the caller). */
export async function putAgentAvatar(
  agentId: string,
  image: Blob,
  authToken: string,
): Promise<{ ok: boolean; avatar_url: string }> {
  return apiFetch<{ ok: boolean; avatar_url: string }>(
    `/api/agents/${encodeURIComponent(agentId)}/avatar`,
    authToken,
    {
      method: "PUT",
      headers: { "Content-Type": image.type || "image/png" },
      body: image,
    },
  );
}

export async function deleteAgentAvatar(agentId: string, authToken: string): Promise<void> {
  await apiFetch(`/api/agents/${encodeURIComponent(agentId)}/avatar`, authToken, {
    method: "DELETE",
  });
}
