import type { StatusTone } from "../../shared/ui/StatusDot";

/** Maps a sleep run/step status to the shared status color vocabulary. */
export function statusTone(status: string): StatusTone {
  switch (status) {
    case "running":
      return "live";
    case "success":
      return "success";
    case "partial_failure":
      return "warning";
    case "failed":
      return "error";
    default:
      return "idle";
  }
}

const STEP_LABELS: Record<string, string> = {
  event_extraction: "Event extraction",
  episodic_update: "Episodic update",
  semantic_update: "Semantic update",
  prospective_update: "Prospective update",
};

export function stepLabel(step: string): string {
  return STEP_LABELS[step] ?? step;
}

const STATUS_LABELS: Record<string, string> = {
  success: "Success",
  partial_failure: "Partial failure",
  failed: "Failed",
  skipped: "Skipped",
  running: "Running",
};

export function statusLabel(status: string): string {
  return STATUS_LABELS[status] ?? status;
}

export function triggerLabel(trigger: string): string {
  return trigger.charAt(0).toUpperCase() + trigger.slice(1);
}
