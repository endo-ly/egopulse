import type { ChatMessage } from "../../shared/api/types";

export interface ChatEventMessage {
  id: string;
  role: string;
  content: Array<{ type: string; text: string }>;
}

interface ChatEventBase {
  runId: string;
  sessionKey: string;
  seq: number;
  terminal?: boolean;
  errorMessage?: string;
}

/**
 * Streaming chat events. Message identity is stable end to end: the client
 * shows the user message under its canonical id from the start, and every
 * assistant message keeps the id assigned before its model iteration from
 * the first delta through the persisted final row. The reducer therefore
 * upserts by id only — it never guesses which entry an event belongs to.
 */
export type ChatEventPayload =
  | (ChatEventBase & { state: "delta"; message: ChatEventMessage })
  | (ChatEventBase & { state: "done"; message?: ChatEventMessage | null })
  | (ChatEventBase & { state: "error" });

export interface ChatState {
  messages: ChatMessage[];
  error: string | null;
  /**
   * Assistant progress as UI state, never as a fake message: set when the
   * server accepts a run (or discards a superseded message before its
   * retry), cleared by the first delta / tool_start / terminal done /
   * terminal error. A non-terminal done/error keeps it set — the next turn
   * on the same run is still on its way.
   */
  waitingForAssistant: boolean;
}

export function initialChatState(): ChatState {
  return { messages: [], error: null, waitingForAssistant: false };
}

export interface OptimisticUserMessage {
  messageId: string;
  text: string;
}

/** Shows the sent text immediately under its canonical id. */
export function reduceOptimisticUserMessage(
  state: ChatState,
  message: OptimisticUserMessage,
): ChatState {
  if (state.messages.some((m) => m.id === message.messageId)) return state;
  return {
    ...state,
    messages: [
      ...state.messages,
      {
        id: message.messageId,
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
export function reduceDiscardUserMessage(
  state: ChatState,
  messageId: string,
): ChatState {
  if (!state.messages.some((m) => m.id === messageId)) return state;
  return {
    ...state,
    messages: state.messages.filter((m) => m.id !== messageId),
  };
}

/**
 * Tags the owning run onto a live entry. Ownership metadata exists only so
 * a truncated replay can drop the entries of one run; it is never used to
 * decide which entry an event addresses.
 */
export function reduceTagMessageRun(
  state: ChatState,
  tag: { messageId: string; runId: string },
): ChatState {
  if (!state.messages.some((m) => m.id === tag.messageId)) return state;
  return {
    ...state,
    messages: state.messages.map((m) =>
      m.id === tag.messageId ? { ...m, runId: tag.runId } : m,
    ),
  };
}

/** The server accepted the run: show assistant progress until content lands. */
export function reduceRunAccepted(state: ChatState): ChatState {
  if (state.waitingForAssistant) return state;
  return { ...state, waitingForAssistant: true };
}

/**
 * Drops every live entry owned by one run, e.g. after a truncated replay
 * whose evicted prefix the stream can no longer rebuild. The live
 * subscription stays attached and fills forward; history converges by id.
 */
export function reduceDropRunMessages(
  state: ChatState,
  runId: string,
): ChatState {
  if (!state.messages.some((m) => m.runId === runId)) return state;
  return {
    ...state,
    messages: state.messages.filter((m) => m.runId !== runId),
  };
}

/** The run is gone (TTL expiry, restart): drop its entries and progress. */
export function reduceRunMissing(state: ChatState, runId: string): ChatState {
  return {
    ...reduceDropRunMessages(state, runId),
    waitingForAssistant: false,
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
  switch (event.state) {
    case "delta": {
      const chunk = extractText(event.message);
      if (!chunk) return state;
      const id = event.message.id;
      const existing = state.messages.find((m) => m.id === id);
      const messages = existing
        ? state.messages.map((m) =>
            m.id === id
              ? { ...m, content: m.content + chunk, runId: event.runId }
              : m,
          )
        : [
            ...state.messages,
            {
              id,
              sender_id: agentId,
              sender_kind: "assistant" as const,
              content: chunk,
              timestamp: new Date().toISOString(),
              message_kind: "message",
              runId: event.runId,
            },
          ];
      return { ...state, messages, error: null, waitingForAssistant: false };
    }

    case "done": {
      // Authoritative content replaces whatever streamed: a replayed done
      // converges to the persisted text instead of duplicating it.
      let messages = state.messages;
      if (event.message && extractText(event.message)) {
        const id = event.message.id;
        const content = extractText(event.message);
        messages = messages.some((m) => m.id === id)
          ? messages.map((m) =>
              m.id === id
                ? { ...m, content, runId: event.runId, sender_id: agentId }
                : m,
            )
          : [
              ...messages,
              {
                id,
                sender_id: agentId,
                sender_kind: "assistant" as const,
                content,
                timestamp: new Date().toISOString(),
                message_kind: "message",
                runId: event.runId,
              },
            ];
      }
      return {
        ...state,
        messages,
        waitingForAssistant:
          event.terminal === false ? state.waitingForAssistant : false,
      };
    }

    case "error": {
      // A failed turn resolves nothing: partial output keeps the id it
      // streamed under, and history converges by id on refetch.
      return {
        ...state,
        error: event.errorMessage ?? "unknown error",
        waitingForAssistant:
          event.terminal === false ? state.waitingForAssistant : false,
      };
    }
  }
}

function extractText(message: ChatEventMessage | undefined | null): string {
  if (!message?.content) return "";
  return message.content
    .filter((c) => c.type === "text")
    .map((c) => c.text)
    .join("");
}

/**
 * Joins persisted history with live transport messages.
 *
 * Live and history share the same message ids from the start, so history
 * wins on id equality and the run converges to the persisted rows.
 */
export function mergeChatMessages(
  history: ChatMessage[],
  live: ChatMessage[],
): ChatMessage[] {
  if (live.length === 0) return history;
  const historyIds = new Set(history.map((message) => message.id));
  return [...history, ...live.filter((message) => !historyIds.has(message.id))];
}

export interface StatusEventPayload {
  message: string;
}

export function parseStatusEvent(payload: StatusEventPayload): string | null {
  const match = payload.message.match(/iteration (\d+)/);
  return match ? match[1] : null;
}

export interface ToolStartPayload {
  runId: string;
  callId: string;
  name: string;
  input?: unknown;
}

export interface ToolResultPayload {
  runId: string;
  callId: string;
  name: string;
  isError: boolean;
  preview: string;
  durationMs: number;
}

export interface UserInputPayload {
  runId: string;
  /** Canonical client-issued id: identical to the optimistic entry. */
  messageId: string;
  senderId: string;
  text: string;
  timestamp: string;
}

export interface AssistantDiscardedPayload {
  runId: string;
  messageId: string;
}

export function reduceUserInput(
  state: ChatState,
  payload: UserInputPayload,
): ChatState {
  // The commit names the same canonical id the optimistic bubble already
  // carries, so this is an upsert: an existing entry only gains its run
  // ownership, and a cross-connection commit is inserted once.
  if (state.messages.some((message) => message.id === payload.messageId)) {
    return reduceTagMessageRun(state, {
      messageId: payload.messageId,
      runId: payload.runId,
    });
  }
  return {
    ...state,
    messages: [
      ...state.messages,
      {
        id: payload.messageId,
        sender_id: payload.senderId,
        sender_kind: "user",
        content: payload.text,
        timestamp: payload.timestamp,
        message_kind: "message",
        runId: payload.runId,
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
    ? messages.map((m) =>
        m.id === message.id ? { ...message, runId: message.runId ?? m.runId } : m,
      )
    : [...messages, message];
}

export function reduceToolStart(
  state: ChatState,
  payload: ToolStartPayload,
): ChatState {
  // Tool cards are addressed by call id; the narration keeps the stable
  // assistant id it streamed under, so nothing is renamed here.
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
    runId: payload.runId,
  };
  return {
    ...state,
    messages: upsertToolMessage(state.messages, message),
    waitingForAssistant: false,
  };
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
    runId: payload.runId,
  };
  return { ...state, messages: upsertToolMessage(state.messages, message) };
}

/**
 * Drops a streamed assistant message the loop discarded via retry. The
 * retry streams under a new id, so deleting by id cannot touch it. Progress
 * stays visible: the retry is still on its way.
 */
export function reduceAssistantDiscarded(
  state: ChatState,
  payload: AssistantDiscardedPayload,
): ChatState {
  if (!state.messages.some((m) => m.id === payload.messageId)) return state;
  return {
    ...state,
    messages: state.messages.filter((m) => m.id !== payload.messageId),
    waitingForAssistant: true,
  };
}
