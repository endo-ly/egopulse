import type { ChatMessage, ToolEventData } from "../../shared/api/types";

/** Decodes a `tool_call` history row into tool card data; null for any other row. */
export function parseToolEvent(message: ChatMessage): ToolEventData | null {
  if (message.message_kind !== "tool_call") {
    return null;
  }
  try {
    const raw = JSON.parse(message.content) as {
      tool?: string;
      status?: string;
      result?: string;
      input?: unknown;
      duration_ms?: number;
    };
    if (typeof raw.tool !== "string") return null;
    const isError = raw.status === "error";
    const isPending = raw.status === "pending";
    return {
      name: raw.tool,
      state: isError ? "error" : isPending ? "pending" : "success",
      output: raw.result,
      is_error: isError,
      input: raw.input,
      duration_ms: raw.duration_ms,
    };
  } catch {
    return null;
  }
}
