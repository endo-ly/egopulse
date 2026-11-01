import type { RuntimeConfig } from "../../shared/api/config";
import type { McpStatusSummary } from "./mcpStatus";

export const CONFIG_SECTIONS = [
  "general",
  "agents",
  "providers",
  "channels",
  "sleep",
  "pulse",
  "webhooks",
  "backup",
  "web-fetch",
  "mcp",
  "yaml",
] as const;
export type ConfigSection = (typeof CONFIG_SECTIONS)[number];

export function isConfigSection(value: string): value is ConfigSection {
  return (CONFIG_SECTIONS as readonly string[]).includes(value);
}

/** The DOM id of a section on the one-page Config screen. */
export function configSectionElementId(section: ConfigSection): string {
  return `config-section-${section}`;
}

/** Scrolls the Config screen to a section, under the sticky section bar. */
export function scrollToConfigSection(section: ConfigSection): void {
  document.getElementById(configSectionElementId(section))?.scrollIntoView({ block: "start" });
}

export interface SectionPosition {
  id: ConfigSection;
  /** Top of the section relative to the viewport. */
  top: number;
}

/**
 * The section whose heading has most recently crossed `line`, in document
 * order. At the end of the page the last section wins even when it is too
 * short to reach the line.
 */
export function pickActiveSection(
  positions: readonly SectionPosition[],
  line: number,
  atBottom: boolean,
): ConfigSection {
  if (positions.length === 0) return CONFIG_SECTIONS[0];
  if (atBottom) return positions[positions.length - 1].id;
  let active = positions[0].id;
  for (const position of positions) {
    if (position.top > line) break;
    active = position.id;
  }
  return active;
}

export const SECTION_INFO: Record<ConfigSection, { label: string; description: string }> = {
  general: { label: "General", description: "Defaults every agent falls back to, and how conversation history is kept." },
  agents: { label: "Agents", description: "The model each agent runs on and the bots it answers through." },
  providers: { label: "Providers", description: "OpenAI-compatible endpoints, their models, and API keys." },
  channels: { label: "Channels", description: "Where conversations come in. Change these in the YAML file and restart." },
  sleep: { label: "Sleep", description: "When conversations are distilled into long-term memory, and with which model." },
  pulse: { label: "Pulse", description: "How often agents check whether an intention is due." },
  webhooks: { label: "Webhooks", description: "External events that make an agent act in a channel." },
  backup: { label: "Backup", description: "Scheduled SQLite backups and how many to keep." },
  "web-fetch": { label: "Web Fetch", description: "What the web_fetch tool may reach and how much it reads." },
  mcp: { label: "MCP", description: "External tool servers loaded from mcp.json and mcp.d/." },
  yaml: { label: "YAML file", description: "The config file as stored on disk, with secrets masked." },
};

export interface SectionSummary {
  text: string;
  tone?: "warning" | "danger";
}

function plural(count: number, one: string, many = `${one}s`): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** One-line state of each section for the sidebar. */
export function sectionSummary(
  section: ConfigSection,
  config: RuntimeConfig,
  mcp: McpStatusSummary | null,
): SectionSummary | null {
  switch (section) {
    case "general": {
      const agent = config.agents.find((entry) => entry.id === config.general.default_agent);
      return { text: `Default agent ${agent?.label ?? config.general.default_agent}` };
    }
    case "agents":
      return { text: plural(config.agents.length, "agent") };
    case "providers": {
      const missing = config.providers.filter((p) => !p.api_key.configured && !p.api_key_optional).length;
      return missing > 0
        ? { text: `${plural(missing, "missing API key", "missing API keys")}`, tone: "warning" }
        : { text: plural(config.providers.length, "provider") };
    }
    case "channels": {
      const on = (["web", "discord", "telegram", "voice"] as const)
        .filter((name) => config.channels[name].enabled)
        .map((name) => name.charAt(0).toUpperCase() + name.slice(1));
      return { text: on.length > 0 ? on.join(", ") : "None enabled" };
    }
    case "sleep":
      return { text: config.sleep.enabled && config.sleep.schedule ? `Daily at ${config.sleep.schedule}` : "Manual only" };
    case "pulse":
      return { text: `Every ${config.pulse.tick_interval}` };
    case "webhooks":
      return { text: config.webhooks.length > 0 ? plural(config.webhooks.length, "receiver") : "None" };
    case "backup":
      return {
        text: config.backup.enabled
          ? `Every ${plural(config.backup.interval_days, "day")} at ${config.backup.time}`
          : "Off",
        tone: config.backup.enabled ? undefined : "warning",
      };
    case "web-fetch": {
      const schemes = config.web_fetch.allowed_schemes;
      return { text: schemes.length === 1 && schemes[0] === "https" ? "HTTPS only" : schemes.join(", ").toUpperCase() };
    }
    case "mcp": {
      if (!mcp) return null;
      if (mcp.failed > 0) return { text: `${mcp.failed} of ${mcp.total} failed`, tone: "danger" };
      return { text: mcp.total > 0 ? plural(mcp.total, "server") : "None" };
    }
    case "yaml":
      return { text: `Revision ${config.revision}` };
  }
}
