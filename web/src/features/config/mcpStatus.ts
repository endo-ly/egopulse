import type { RuntimeStatus } from "../../shared/api/metrics";

export interface McpStatusSummary {
  /** Servers the runtime tried to start (connected or failed). */
  total: number;
  failed: number;
  /** Server name → connection state for the server cards. */
  byName: Record<string, { connected: true; tools: number } | { connected: false; error: string }>;
}

export function summarizeMcp(status: RuntimeStatus | undefined): McpStatusSummary | null {
  if (!status) return null;
  const byName: McpStatusSummary["byName"] = {};
  for (const server of status.mcp.connected) byName[server.name] = { connected: true, tools: server.tools.length };
  for (const server of status.mcp.failed) byName[server.name] = { connected: false, error: server.error };
  return { total: status.mcp.connected.length + status.mcp.failed.length, failed: status.mcp.failed.length, byName };
}
