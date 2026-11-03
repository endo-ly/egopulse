import type { AttachmentInfo, ChatMessage, ToolEventData } from "../../shared/api/types";

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
      attachments?: unknown;
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
      attachments: parseAttachments(raw.attachments),
    };
  } catch {
    return null;
  }
}

/** Validates the attachment array persisted in a tool-result payload. */
function parseAttachments(value: unknown): AttachmentInfo[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const attachments: AttachmentInfo[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object") continue;
    const raw = item as Record<string, unknown>;
    if (
      typeof raw.id !== "string" ||
      typeof raw.filename !== "string" ||
      typeof raw.content_type !== "string" ||
      typeof raw.size_bytes !== "number"
    ) {
      continue;
    }
    attachments.push({
      id: raw.id,
      filename: raw.filename,
      content_type: raw.content_type,
      size_bytes: raw.size_bytes,
    });
  }
  return attachments.length > 0 ? attachments : undefined;
}

/** Returns the files a successful `send_attachment` delivered, if any. */
export function toolAttachments(event: ToolEventData): AttachmentInfo[] {
  if (event.name !== "send_attachment" || event.state !== "success") return [];
  return event.attachments ?? [];
}

/** Returns the optional caption the agent passed to `send_attachment`. */
export function toolCaption(event: ToolEventData): string | undefined {
  if (!event.input || typeof event.input !== "object") return undefined;
  const text = (event.input as Record<string, unknown>).text;
  return typeof text === "string" && text.trim() ? text.trim() : undefined;
}
