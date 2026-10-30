import { MEMORY_LAYERS, type AgentProfile, type LlmSettingSource, type AgentLlmRow } from "../../shared/api/types";

/** Compact character count ("812 chars", "8.4k chars"). */
export function formatChars(chars: number): string {
  return chars >= 1000 ? `${(chars / 1000).toFixed(1)}k chars` : `${chars} chars`;
}

export function totalMemoryChars(profile: AgentProfile): number {
  return MEMORY_LAYERS.reduce((sum, layer) => sum + profile.prompt_sources.memory[layer], 0);
}

/** Names the config key a resolved LLM setting came from. */
export function llmSourceKey(
  source: LlmSettingSource,
  agentId: string,
  row: AgentLlmRow,
  kind: "provider" | "model",
): string {
  switch (source) {
    case "profile":
      return `agents.${agentId}.profiles.${row.channel}`;
    case "agent":
      return `agents.${agentId}`;
    case "global_default":
      return `default_${kind}`;
    case "provider_default":
      return `providers.${row.provider}`;
  }
}
