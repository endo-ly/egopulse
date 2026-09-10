import type { ChatMessage } from "../../shared/api/types";

export interface ChatEventPayload {
  runId: string;
  sessionKey: string;
  seq: number;
  state: "delta" | "done" | "error";
  terminal?: boolean;
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

/** Client-side ids (`draft:` streaming, `local:` optimistic, `tool:` cards). */
export function isLiveMessageId(id: string): boolean {
  return (
    id.startsWith("draft:") || id.startsWith("local:") || id.startsWith("tool:")
  );
}

function optimisticUserMessageId(requestId: string): string {
  return `local:${requestId}`;
}

export interface OptimisticUserMessage {
  requestId: string;
  text: string;
}

/** Shows the sent text immediately; history or the echo replaces it later. */
export function reduceOptimisticUserMessage(
  state: ChatState,
  message: OptimisticUserMessage,
): ChatState {
  const id = optimisticUserMessageId(message.requestId);
  if (state.messages.some((m) => m.id === id)) return state;
  return {
    ...state,
    messages: [
      ...state.messages,
      {
        id,
        sender_id: "user",
        sender_kind: "user",
        content: message.text,
        timestamp: new Date().toISOString(),
        message_kind: "message",
      },
    ],
  };
}

/** Withdraws the optimistic message, e.g. when sending failed. */
export function reduceDiscardOptimisticUserMessage(
  state: ChatState,
  requestId: string,
): ChatState {  const id = optimisticUserMessageId(requestId);
  if (!state.messages.some((m) => m.id === id)) return state;
  return {
    ...state,
    messages: state.messages.filter((m) => m.id !== id),
  };
}

export interface RunAcceptedPayload {
  runId: string;
  agentId: string;
}

/**
 * Shows an empty assistant draft as soon as the server accepts the run, so
 * the user sees the assistant's turn begin before the first delta lands.
 */
export function reduceRunAccepted(
  state: ChatState,
  payload: RunAcceptedPayload,
): ChatState {
  const draftId = `draft:${payload.runId}`;
  if (state.messages.some((message) => message.id === draftId)) return state;
  return {
    ...state,
    messages: [
      ...state.messages,
      {
        id: draftId,
        sender_id: payload.agentId,
        sender_kind: "assistant" as const,
        content: "",
        timestamp: new Date().toISOString(),
        message_kind: "message",
      },
    ],
  };
}

/**
 * Applies a streaming chat event. `agentId` — the session's agent — stamps the
 * assistant sender so live bubbles resolve the same avatar as persisted ones.
 */
export function reduceChatEvent(
  state: ChatState,
  event: ChatEventPayload,
  agentId: string,
): ChatState {
  const draftId = `draft:${event.runId}`;

  switch (event.state) {
    case "delta": {
      const chunk = extractText(event);
      if (!chunk) return state;

      let messages = state.messages;
      const existing = messages.find((m) => m.id === draftId);
      if (existing) {
        messages = messages.map((m) =>
          m.id === draftId
            ? { ...m, content: m.content + chunk }
            : m,
        );
      } else {
        messages = [
          ...messages,
          {
            id: draftId,
            sender_id: agentId,
            sender_kind: "assistant" as const,
            content: chunk,
            timestamp: new Date().toISOString(),
            message_kind: "message",
          },
        ];
      }
      return { ...state, messages, runId: event.runId, error: null };
    }

    case "done": {
      const finalText = extractText(event);
      // Locals stay until history actually delivers their copy; the merge
      // reconciles them against newly arrived entries.
      let messages = state.messages;
      const existing = messages.find((m) => m.id === draftId);
      if (existing) {
        if (!finalText && !existing.content) {
          // An accepted run finished without ever producing text: no
          // persisted answer will back the placeholder, so drop it instead
          // of leaving an empty bubble.
          messages = messages.filter((m) => m.id !== draftId);
          return { ...state, messages, error: null };
        }
        const sealedId = sealedAssistantDraftId(messages, draftId);
        messages = messages.map((m) =>
          m.id === draftId
            ? { ...m, id: sealedId, content: finalText || m.content, sender_id: agentId }
            : m,
        );
      } else if (finalText) {
        const sealedId = sealedAssistantDraftId(messages, draftId);
        messages = [
          ...messages,
          {
            id: sealedId,
            sender_id: agentId,
            sender_kind: "assistant" as const,
            content: finalText,
            timestamp: new Date().toISOString(),
            message_kind: "message",
          },
        ];
      }
      return { ...state, messages, error: null };
    }

    case "error": {
      // A terminal error ends the run's transcript. An empty placeholder
      // would linger forever, and a partial draft would keep the streaming
      // cursor blinking, so empty drafts are dropped and partial ones are
      // sealed; the generated text itself is kept.
      const existing = state.messages.find((m) => m.id === draftId);
      let messages = state.messages;
      if (existing && event.terminal !== false) {
        if (existing.content === "") {
          messages = messages.filter((m) => m.id !== draftId);
        } else {
          const sealedId = sealedAssistantDraftId(messages, draftId);
          messages = messages.map((m) =>
            m.id === draftId ? { ...m, id: sealedId, sender_id: agentId } : m,
          );
        }
      }
      return { ...state, runId: event.runId, messages, error: event.errorMessage ?? "unknown error" };
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

function sealedAssistantDraftId(messages: ChatMessage[], draftId: string): string {
  const firstId = `${draftId}:done`;
  if (!messages.some((message) => message.id === firstId)) return firstId;

  let segment = 2;
  while (messages.some((message) => message.id === `${draftId}:segment:${segment}:done`)) {
    segment += 1;
  }
  return `${draftId}:segment:${segment}:done`;
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

export interface UserInputPayload {
  messageId: string;
  senderId: string;
  text: string;
  timestamp: string;
}

export function reduceUserInput(
  state: ChatState,
  payload: UserInputPayload,
): ChatState {
  if (state.messages.some((message) => message.id === payload.messageId)) {
    return state;
  }

  // The server echo supersedes one optimistic message with the same text.
  let withoutLocal = state.messages;
  const localIndex = withoutLocal.findIndex(
    (message) =>
      message.sender_kind === "user" &&
      message.content === payload.text &&
      message.id.startsWith("local:"),
  );
  if (localIndex >= 0) {
    withoutLocal = [
      ...withoutLocal.slice(0, localIndex),
      ...withoutLocal.slice(localIndex + 1),
    ];
  }

  const draftId = state.runId ? `draft:${state.runId}` : null;
  const messages = draftId && withoutLocal.some((message) => message.id === draftId)
    ? withoutLocal.map((message) =>
        message.id === draftId
          ? { ...message, id: sealedAssistantDraftId(withoutLocal, draftId) }
          : message,
      )
    : withoutLocal;

  return {
    ...state,
    messages: [
      ...messages,
      {
        id: payload.messageId,
        sender_id: payload.senderId,
        sender_kind: "user",
        content: payload.text,
        timestamp: payload.timestamp,
        message_kind: "message",
      },
    ],
  };
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
