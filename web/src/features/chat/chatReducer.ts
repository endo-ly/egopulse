import type { ChatMessage, ToolEventData } from "../../shared/api/types";

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
  toolEvents: Record<string, ToolEventData>;
  runId: string | null;
  error: string | null;
}

export function initialChatState(): ChatState {
  return { messages: [], toolEvents: {}, runId: null, error: null };
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
  name: string;
  callId?: string;
  input?: unknown;
}

export interface ToolResultPayload {
  name: string;
  is_error: boolean;
  duration_ms: number;
  callId?: string;
}

export function reduceToolStart(
  state: ChatState,
  runId: string,
  payload: ToolStartPayload,
): ChatState {
  const key = payload.callId
    ? `${runId}:${payload.callId}`
    : `${runId}:${payload.name}`;
  return {
    ...state,
    toolEvents: {
      ...state.toolEvents,
      [key]: { name: payload.name, state: "pending", input: payload.input },
    },
  };
}

export function reduceToolResult(
  state: ChatState,
  runId: string,
  payload: ToolResultPayload,
): ChatState {
  const key = payload.callId
    ? `${runId}:${payload.callId}`
    : `${runId}:${payload.name}`;
  const existing = state.toolEvents[key];
  return {
    ...state,
    toolEvents: {
      ...state.toolEvents,
      [key]: {
        name: payload.name,
        state: payload.is_error ? "error" : "success",
        is_error: payload.is_error,
        duration_ms: payload.duration_ms,
        input: existing?.input,
      },
    },
  };
}
