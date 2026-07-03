import type { StatusTone } from "../shared/ui/StatusDot";

export type HealthStatus = "ok" | "degraded" | "down";

export function healthTone(status: HealthStatus): StatusTone {
  switch (status) {
    case "ok":
      return "live";
    case "down":
      return "error";
    case "degraded":
      return "idle";
  }
}
