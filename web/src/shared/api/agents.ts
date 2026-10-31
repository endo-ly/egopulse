import type {
  AgentEntry,
  AgentMemory,
  AgentProfile,
  MemoryChange,
  PromptFileKind,
  PromptFileLayer,
  SkillDetail,
  SkillSummary,
} from "./types";
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

function agentPath(agentId: string, suffix: string): string {
  return `/api/agents/${encodeURIComponent(agentId)}${suffix}`;
}

export async function fetchAgentProfile(agentId: string, authToken: string): Promise<AgentProfile> {
  const data = await apiFetch<{ ok: boolean; profile: AgentProfile }>(
    agentPath(agentId, "/profile"),
    authToken,
  );
  return data.profile;
}

export async function fetchPromptFile(
  agentId: string,
  kind: PromptFileKind,
  authToken: string,
): Promise<PromptFileLayer[]> {
  const data = await apiFetch<{ ok: boolean; layers: PromptFileLayer[] }>(
    agentPath(agentId, `/files/${kind}`),
    authToken,
  );
  return data.layers;
}

export async function fetchAgentMemory(agentId: string, authToken: string): Promise<AgentMemory> {
  const data = await apiFetch<{ ok: boolean; memory: AgentMemory }>(
    agentPath(agentId, "/memory"),
    authToken,
  );
  return data.memory;
}

export async function fetchMemoryHistory(agentId: string, authToken: string): Promise<MemoryChange[]> {
  const data = await apiFetch<{ ok: boolean; changes: MemoryChange[] }>(
    agentPath(agentId, "/memory/history"),
    authToken,
  );
  return data.changes;
}

export async function fetchSkills(authToken: string): Promise<SkillSummary[]> {
  const data = await apiFetch<{ ok: boolean; skills: SkillSummary[] }>("/api/skills", authToken);
  return data.skills;
}

export async function fetchSkill(name: string, authToken: string): Promise<SkillDetail> {
  const data = await apiFetch<{ ok: boolean; skill: SkillDetail }>(
    `/api/skills/${encodeURIComponent(name)}`,
    authToken,
  );
  return data.skill;
}

export async function fetchSkillFile(name: string, path: string, authToken: string): Promise<string> {
  const encodedPath = path.split("/").map(encodeURIComponent).join("/");
  const data = await apiFetch<{ ok: boolean; content: string }>(
    `/api/skills/${encodeURIComponent(name)}/files/${encodedPath}`,
    authToken,
  );
  return data.content;
}
