import type { ChatMessage } from "../../shared/api/types";

export interface ChatEventPayload {
  runId: string;
  sessionKey: string;
  seq: number;
  state: "delta" | "done" | "error";
  message?: {
    role: string;
    content: Array<{ type: string; text: string }>;
  };
  errorMessage?: string;
}

export interface ChatState {
  messages: ChatMessage[];
  runId: string | null;
  error: string | null;
}

export function initialChatState(): ChatState {
  return { messages: [], runId: null, error: null };
}

export function reduceChatEvent(state: ChatState, event: ChatEventPayload): ChatState {
  const draftId = `draft:${event.runId}`;

  switch (event.state) {
    case "delta": {
      const chunk = extractText(event);
      if (!chunk) return state;

      let messages = state.messages;
      const existing = messages.find((m) => m.id === draftId);
      if (existing) {
        messages = messages.map((m) =>
          m.id === draftId ? { ...m, content: m.content + chunk } : m,
        );
      } else {
        messages = [
          ...messages,
          {
            id: draftId,
            sender_id: "assistant",
            sender_kind: "assistant" as const,
            content: chunk,
            timestamp: new Date().toISOString(),
            message_kind: "message",
          },
        ];
      }
      return { ...state, messages, runId: event.runId };
    }

    case "done": {
      const finalText = extractText(event);
      let messages = state.messages;
      const existing = messages.find((m) => m.id === draftId);
      if (existing) {
        messages = messages.map((m) =>
          m.id === draftId
            ? { ...m, id: `${draftId}:done`, content: finalText || m.content }
            : m,
        );
      } else if (finalText) {
        messages = [
          ...messages,
          {
            id: `${draftId}:done`,
            sender_id: "assistant",
            sender_kind: "assistant" as const,
            content: finalText,
            timestamp: new Date().toISOString(),
            message_kind: "message",
          },
        ];
      }
      return { ...state, messages };
    }

    case "error": {
      return { ...state, runId: event.runId, error: event.errorMessage ?? "unknown error" };
    }
  }
}

function extractText(event: ChatEventPayload): string {
  if (!event.message?.content) return "";
  return event.message.content
    .filter((c) => c.type === "text")
    .map((c) => c.text)
    .join("");
}

export interface StatusEventPayload {
  message: string;
}

export function parseStatusEvent(payload: StatusEventPayload): string | null {
  const match = payload.message.match(/iteration (\d+)/);
  return match ? match[1] : null;
}

export interface ToolStartPayload {
  callId: string;
  name: string;
  input?: unknown;
}

export interface ToolResultPayload {
  callId: string;
  name: string;
  isError: boolean;
  preview: string;
  durationMs: number;
}

interface ToolContent {
  tool: string;
  status: "pending" | "success" | "error";
  input?: unknown;
  result?: string;
  durationMs?: number;
}

function toolMessageId(callId: string): string {
  return `tool:${callId}`;
}

function encodeToolContent(fields: ToolContent): string {
  return JSON.stringify({
    tool: fields.tool,
    status: fields.status,
    input: fields.input ?? null,
    result: fields.result,
    duration_ms: fields.durationMs,
  });
}

function decodeToolInput(content: string): unknown {
  try {
    return (JSON.parse(content) as { input?: unknown }).input;
  } catch {
    return undefined;
  }
}

function upsertToolMessage(
  messages: ChatMessage[],
  message: ChatMessage,
): ChatMessage[] {
  return messages.some((m) => m.id === message.id)
    ? messages.map((m) => (m.id === message.id ? message : m))
    : [...messages, message];
}

export function reduceToolStart(
  state: ChatState,
  payload: ToolStartPayload,
): ChatState {
  const message: ChatMessage = {
    id: toolMessageId(payload.callId),
    sender_id: "assistant",
    sender_kind: "tool",
    content: encodeToolContent({
      tool: payload.name,
      status: "pending",
      input: payload.input,
    }),
    timestamp: new Date().toISOString(),
    message_kind: "tool_call",
  };
  return { ...state, messages: upsertToolMessage(state.messages, message) };
}

export function reduceToolResult(
  state: ChatState,
  payload: ToolResultPayload,
): ChatState {
  const existing = state.messages.find(
    (m) => m.id === toolMessageId(payload.callId),
  );
  const message: ChatMessage = {
    id: toolMessageId(payload.callId),
    sender_id: "assistant",
    sender_kind: "tool",
    content: encodeToolContent({
      tool: payload.name,
      status: payload.isError ? "error" : "success",
      input: existing ? decodeToolInput(existing.content) : undefined,
      result: payload.preview,
      durationMs: payload.durationMs,
    }),
    timestamp: new Date().toISOString(),
    message_kind: "tool_call",
  };
  return { ...state, messages: upsertToolMessage(state.messages, message) };
}
