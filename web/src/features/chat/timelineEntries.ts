import type { ChatMessage } from "../../shared/api/types";

/** The activation behind one delivered Pulse output. */
export interface PulseTraceData {
  intentionId: string | null;
  /** Input, narration, and tool rows in timeline order. */
  entries: ChatMessage[];
}

/** One top-level timeline row. */
export interface TimelineEntry {
  message: ChatMessage;
  /** Present on a Pulse output: its folded activation trace. */
  pulseTrace?: PulseTraceData;
}

/**
 * Folds each Pulse activation's trace under its delivered output, so the
 * timeline reads like the channel the output was delivered to. Trace rows
 * whose output is not loaded stay as ordinary rows.
 */
export function buildTimelineEntries(messages: ChatMessage[]): TimelineEntry[] {
  const outputs = new Map<string, ChatMessage>();
  for (const message of messages) {
    if (message.pulse?.role === "output") outputs.set(message.pulse.run_id, message);
  }
  if (outputs.size === 0) return messages.map((message) => ({ message }));

  const traces = new Map<string, ChatMessage[]>();
  const entries: TimelineEntry[] = [];
  for (const message of messages) {
    const pulse = message.pulse;
    if (pulse && pulse.role !== "output" && outputs.has(pulse.run_id)) {
      const trace = traces.get(pulse.run_id) ?? [];
      trace.push(message);
      traces.set(pulse.run_id, trace);
      continue;
    }
    entries.push({ message });
  }
  return entries.map((entry) => {
    const pulse = entry.message.pulse;
    if (pulse?.role !== "output") return entry;
    return {
      message: entry.message,
      pulseTrace: {
        intentionId: pulse.intention_id,
        entries: traces.get(pulse.run_id) ?? [],
      },
    };
  });
}
