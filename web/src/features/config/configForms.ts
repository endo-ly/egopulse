import type { ConfigTarget, RuntimeConfig } from "../../shared/api/config";
import type { FieldOption, FieldSpec, FormValues } from "./EditForm";

/** Common reasoning effort values; any other value can still be typed. */
const EFFORT_SUGGESTIONS = ["minimal", "low", "medium", "high", "xhigh"];

function providerOptions(config: RuntimeConfig): FieldOption[] {
  return config.providers.map((p) => ({ value: p.id, label: p.label === p.id ? p.id : `${p.label} (${p.id})` }));
}

function modelSuggestions(config: RuntimeConfig, providerId: string | null): FieldOption[] {
  const provider = config.providers.find((p) => p.id === providerId);
  return (provider?.models ?? []).map((m) => ({ value: m.name, label: m.name }));
}

function providerDefaultModel(config: RuntimeConfig, providerId: string | null): string {
  return config.providers.find((p) => p.id === providerId)?.default_model ?? "provider default";
}

export interface EditTarget {
  target: ConfigTarget;
  specs: FieldSpec[];
  values: FormValues;
}

export function generalTarget(config: RuntimeConfig): EditTarget {
  const g = config.general;
  return {
    target: { kind: "general" },
    values: {
      default_agent: g.default_agent,
      default_provider: g.default_provider,
      default_model: g.default_model,
      timezone: g.timezone,
      max_history_messages: g.max_history_messages,
      compaction_enabled: g.compaction_enabled,
      compaction_reserve_tokens: g.compaction_reserve_tokens,
      compaction_keep_recent_tokens: g.compaction_keep_recent_tokens,
    },
    specs: [
      {
        key: "default_agent",
        label: "Default agent",
        kind: "select",
        options: () => config.agents.map((a) => ({ value: a.id, label: a.label })),
        hint: "Answers where a channel does not name an agent.",
      },
      { key: "default_provider", label: "Default provider", kind: "select", options: () => providerOptions(config) },
      {
        key: "default_model",
        label: "Default model",
        kind: "text",
        nullable: true,
        options: (draft) => modelSuggestions(config, draft.default_provider as string),
        placeholder: (draft) => providerDefaultModel(config, draft.default_provider as string),
        hint: "Used by agents without a model of their own.",
      },
      {
        key: "timezone",
        label: "Timezone",
        kind: "text",
        options: () => timezoneOptions(),
        hint: "IANA name, e.g. Asia/Tokyo. Sleep, Pulse, and timestamps use it.",
      },
      { key: "max_history_messages", label: "History messages", kind: "number", min: 1, hint: `Messages restored when a session has no saved state. Default ${g.defaults.max_history_messages}.` },
      { key: "compaction_enabled", label: "Automatic compaction", kind: "toggle", hint: `Compact long contexts automatically. Default ${g.defaults.compaction_enabled}.` },
      { key: "compaction_reserve_tokens", label: "Reserve tokens", kind: "number", min: 0, hint: `Context window headroom for output, tools, and margin. Default ${g.defaults.compaction_reserve_tokens}.` },
      { key: "compaction_keep_recent_tokens", label: "Keep recent tokens", kind: "number", min: 0, hint: `Recent history kept verbatim. Default ${g.defaults.compaction_keep_recent_tokens}.` },
    ],
  };
}

let timezoneCache: FieldOption[] | null = null;
function timezoneOptions(): FieldOption[] {
  if (timezoneCache) return timezoneCache;
  const supported = (Intl as unknown as { supportedValuesOf?: (key: string) => string[] }).supportedValuesOf;
  timezoneCache = (supported ? supported("timeZone") : ["UTC"]).map((zone) => ({ value: zone, label: zone }));
  return timezoneCache;
}

export function agentTarget(config: RuntimeConfig, agentId: string): EditTarget | null {
  const agent = config.agents.find((a) => a.id === agentId);
  if (!agent) return null;
  const discordBots = config.channels.discord.bots.map((b) => ({ value: b.id, label: b.id }));
  const telegramBots = config.channels.telegram.bots.map((b) => ({ value: b.id, label: b.id }));
  const resolvedProvider = (draft: FormValues) => (draft.provider as string | null) ?? config.general.default_provider;
  return {
    target: { kind: "agent", id: agent.id },
    values: {
      label: agent.label,
      provider: agent.provider,
      model: agent.model,
      reasoning_effort: agent.reasoning_effort,
      discord_bot: agent.discord_bot,
      telegram_bot: agent.telegram_bot,
    },
    specs: [
      { key: "label", label: "Name", kind: "text" },
      {
        key: "provider",
        label: "Provider",
        kind: "select",
        nullable: true,
        options: () => providerOptions(config),
        placeholder: () => `default, ${config.general.default_provider}`,
      },
      {
        key: "model",
        label: "Model",
        kind: "text",
        nullable: true,
        options: (draft) => modelSuggestions(config, resolvedProvider(draft)),
        placeholder: (draft) =>
          config.general.default_model ?? providerDefaultModel(config, resolvedProvider(draft)),
      },
      {
        key: "reasoning_effort",
        label: "Reasoning effort",
        kind: "text",
        nullable: true,
        options: () => EFFORT_SUGGESTIONS.map((v) => ({ value: v, label: v })),
        placeholder: () => "provider default",
        hint: "Sent as is to the provider.",
      },
      discordBots.length > 0
        ? { key: "discord_bot", label: "Discord bot", kind: "select", nullable: true, options: () => discordBots, placeholder: () => "none" }
        : { key: "discord_bot", label: "Discord bot", kind: "text", nullable: true, readOnlyReason: "No Discord bots are configured" },
      telegramBots.length > 0
        ? { key: "telegram_bot", label: "Telegram bot", kind: "select", nullable: true, options: () => telegramBots, placeholder: () => "none" }
        : { key: "telegram_bot", label: "Telegram bot", kind: "text", nullable: true, readOnlyReason: "No Telegram bots are configured" },
    ],
  };
}

export function providerTarget(config: RuntimeConfig, providerId: string): EditTarget | null {
  const provider = config.providers.find((p) => p.id === providerId);
  if (!provider) return null;
  const processManaged = provider.api_key.kind === "env" && provider.api_key.origin === "process";
  const storageHint =
    provider.api_key.kind === "literal"
      ? "Stored in the YAML file. Replacing it stores the new key in .env."
      : provider.api_key.ref
        ? `Stored in .env as ${provider.api_key.ref}.`
        : "A new key is stored in .env; the YAML keeps only a reference.";
  const removeReason = provider.api_key_optional
    ? undefined
    : "This provider requires an API key; it can be replaced but not removed.";
  return {
    target: { kind: "provider", id: provider.id },
    values: {
      label: provider.label,
      base_url: provider.base_url,
      models: provider.models.map((m) => m.name),
      default_model: provider.default_model,
      api_key: { action: "keep" },
    },
    specs: [
      { key: "label", label: "Name", kind: "text" },
      { key: "base_url", label: "Base URL", kind: "text", hint: "OpenAI-compatible endpoint, e.g. https://openrouter.ai/api/v1" },
      { key: "models", label: "Models", kind: "list", hint: "Context window and instructions per model stay as set in the YAML file." },
      {
        key: "default_model",
        label: "Default model",
        kind: "text",
        options: (draft) => ((draft.models as string[]) ?? []).map((m) => ({ value: m, label: m })),
        hint: "Added to the model list if missing.",
      },
      {
        key: "api_key",
        label: "API key",
        kind: "secret",
        readOnlyReason: processManaged
          ? `Set by the process environment (${provider.api_key.ref}). Change it there.`
          : undefined,
        removeDisabledReason: processManaged ? undefined : removeReason,
        hint: processManaged
          ? undefined
          : [storageHint, removeReason].filter(Boolean).join(" "),
      },
    ],
  };
}

export function sleepTarget(config: RuntimeConfig): EditTarget {
  const s = config.sleep;
  const resolvedProvider = (draft: FormValues) => (draft.provider as string | null) ?? config.general.default_provider;
  return {
    target: { kind: "sleep" },
    values: {
      enabled: s.enabled,
      schedule: s.schedule,
      agents: s.agents,
      provider: s.provider,
      model: s.model,
      reasoning_effort: s.reasoning_effort,
      retry_max_attempts: s.retry_max_attempts,
      retry_interval_minutes: s.retry_interval_minutes,
    },
    specs: [
      { key: "enabled", label: "Run on a schedule", kind: "toggle", hint: "When off, Sleep runs only when started by hand." },
      { key: "schedule", label: "Time", kind: "text", nullable: true, hint: `HH:MM, daily in ${config.general.timezone}. Required while scheduled.` },
      { key: "agents", label: "Agents", kind: "choices", options: () => config.agents.map((a) => ({ value: a.id, label: a.label })) },
      { key: "provider", label: "Provider", kind: "select", nullable: true, options: () => providerOptions(config), placeholder: () => `default, ${config.general.default_provider}` },
      {
        key: "model",
        label: "Model",
        kind: "text",
        nullable: true,
        options: (draft) => modelSuggestions(config, resolvedProvider(draft)),
        placeholder: (draft) => config.general.default_model ?? providerDefaultModel(config, resolvedProvider(draft)),
      },
      { key: "reasoning_effort", label: "Reasoning effort", kind: "text", nullable: true, options: () => EFFORT_SUGGESTIONS.map((v) => ({ value: v, label: v })), placeholder: () => "provider default" },
      { key: "retry_max_attempts", label: "Max attempts", kind: "number", min: 1, hint: "Total attempts, including the first run." },
      { key: "retry_interval_minutes", label: "Retry after", kind: "number", min: 1, hint: "Minutes between attempts." },
    ],
  };
}

export function pulseTarget(config: RuntimeConfig): EditTarget {
  return {
    target: { kind: "pulse" },
    values: { tick_interval: config.pulse.tick_interval },
    specs: [
      {
        key: "tick_interval",
        label: "Check every",
        kind: "text",
        hint: `Duration such as 30s, 5m, or 1h30m. Default ${config.pulse.default_tick_interval}.`,
      },
    ],
  };
}
