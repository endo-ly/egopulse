import type { RuntimeStatus } from "../../shared/api/metrics";

export interface HealthIssue {
  key: string;
  severity: "danger" | "warning";
  title: string;
  detail?: string;
}

/** Everything in the runtime status that needs the user's attention, worst first. */
export function healthIssues(status: RuntimeStatus): HealthIssue[] {
  const issues: HealthIssue[] = [];
  if (status.critical_task_failure) {
    issues.push({ key: "critical", severity: "danger", title: "A critical task stopped", detail: status.critical_task_failure });
  }
  if (!status.db.ok) {
    issues.push({ key: "db", severity: "danger", title: "Database is unavailable" });
  }
  if (status.shutdown_started) {
    issues.push({ key: "shutdown", severity: "warning", title: "Shutting down" });
  } else if (!status.accepting_inputs) {
    issues.push({ key: "inputs", severity: "warning", title: "Not accepting new messages" });
  }
  for (const [name, channel] of Object.entries(status.channels).sort(([a], [b]) => a.localeCompare(b))) {
    if (channel.state === "failed") {
      issues.push({
        key: `channel-${name}`,
        severity: "danger",
        title: `${name} channel failed`,
        detail: channel.last_error ?? undefined,
      });
    }
  }
  for (const server of status.mcp.failed) {
    issues.push({ key: `mcp-${server.name}`, severity: "danger", title: `MCP server ${server.name} is not connected`, detail: server.error });
  }
  if (!status.bash_sandbox.enabled) {
    issues.push({
      key: "sandbox",
      severity: "warning",
      title: "Bash runs without isolation",
      detail: status.bash_sandbox.reason,
    });
  }
  return issues;
}
