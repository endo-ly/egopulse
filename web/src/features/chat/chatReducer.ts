import type { ChatMessage } from "../../shared/api/types";

interface ChatEventBase {
  runId: string;
  sessionKey: string;
  seq: number;
  terminal?: boolean;
  message?: {
    role: string;
    content: Array<{ type: string; text: string }>;
  };
  errorMessage?: string;
}

export type ChatEventPayload =
  | (ChatEventBase & { state: "delta" })
  | (ChatEventBase & {
      state: "done" | "error";
      /**
       * Persisted id of the emitting Turn's input message. Adopts the
       * oldest optimistic user bubble of this run. `null` when the run has
       * no Turn (slash commands): live entries are kept and history
       * converges on refetch.
       */
      userMessageId: string | null;
      /**
       * Persisted id of the emitting Turn's final message. Adopts this
       * run's sealed assistant draft. `null` when the Turn produced no
       * final message: a sealed partial draft is kept as-is, never dropped.
       */
      assistantMessageId: string | null;
    });

export interface ChatState {
  messages: ChatMessage[];
  runId: string | null;
  error: string | null;
}

export function initialChatState(): ChatState {
  return { messages: [], runId: null, error: null };
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

/** Tags the optimistic message with its accepted run for later adoption. */
export function reduceTagLocalRun(
  state: ChatState,
  message: { requestId: string; runId: string },
): ChatState {
  const id = optimisticUserMessageId(message.requestId);
  if (!state.messages.some((m) => m.id === id)) return state;
  return {
    ...state,
    messages: state.messages.map((m) =>
      m.id === id ? { ...m, runId: message.runId } : m,
    ),
  };
}

export interface RunMessageIdAdoption {
  runId: string;
  userMessageId: string | null;
  assistantMessageId: string | null;
}

/**
 * Adopts the run's persisted input id onto the oldest optimistic user
 * bubble of this run, so the history merge drops it by id. Only bubbles
 * tagged with this run (or never tagged) qualify: ids unknown to the
 * server (`null`) or belonging to another run never steal entries.
 */
export function reduceAdoptUserMessage(
  state: ChatState,
  ids: RunMessageIdAdoption,
): ChatState {
  const adopted = ids.userMessageId;
  if (adopted === null) return state;
  const index = state.messages.findIndex(
    (message) =>
      message.id.startsWith("local:") &&
      (message.runId === undefined || message.runId === ids.runId),
  );
  if (index < 0) return state;
  return {
    ...state,
    messages: state.messages.map((message, current) =>
      current === index ? { ...message, id: adopted } : message,
    ),
  };
}

/**
 * Adopts the run's persisted final id onto this run's sealed assistant
 * draft, so the history merge drops it by id. Tool previews and result
 * summaries are never adopted: the server only reports the final id.
 * A `null` id keeps the sealed partial draft untouched.
 *
 * When several sealed segments exist, the last one wins: earlier segments
 * were adopted at their ToolStart, so the trailing sealed entry is the
 * true final answer.
 */
export function reduceAdoptAssistantMessage(
  state: ChatState,
  ids: RunMessageIdAdoption,
): ChatState {
  const adopted = ids.assistantMessageId;
  if (adopted === null) return state;
  const draftId = `draft:${ids.runId}`;
  let index = -1;
  for (let current = 0; current < state.messages.length; current++) {
    const id = state.messages[current].id;
    if (
      (id === draftId || id.startsWith(`${draftId}:`)) &&
      id.includes(":done")
    ) {
      index = current;
    }
  }
  if (index < 0) return state;
  return {
    ...state,
    messages: state.messages.map((message, current) =>
      current === index ? { ...message, id: adopted } : message,
    ),
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
      let messages = state.messages;
      const existing = messages.find((m) => m.id === draftId);
      if (existing) {
        if (!finalText && !existing.content) {
          // An accepted run finished without ever producing text: no
          // persisted answer will back the placeholder, so drop it instead
          // of leaving an empty bubble.
          messages = messages.filter((m) => m.id !== draftId);
        } else {
          const sealedId = sealedAssistantDraftId(messages, draftId);
          messages = messages.map((m) =>
            m.id === draftId
              ? { ...m, id: sealedId, content: finalText || m.content, sender_id: agentId }
              : m,
          );
        }
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
      const ids: RunMessageIdAdoption = {
        runId: event.runId,
        userMessageId: event.userMessageId ?? null,
        assistantMessageId: event.assistantMessageId ?? null,
      };
      return reduceAdoptAssistantMessage(
        reduceAdoptUserMessage({ ...state, messages }, ids),
        ids,
      );
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
      // A failed turn still persisted the user message, so adopt it. The
      // assistant draft adopts the final id only when the Turn persisted
      // one (failure after the final write); otherwise the sealed partial
      // output is kept instead of dropped.
      const ids: RunMessageIdAdoption = {
        runId: event.runId,
        userMessageId: event.userMessageId ?? null,
        assistantMessageId: event.assistantMessageId ?? null,
      };
      return reduceAdoptAssistantMessage(
        reduceAdoptUserMessage(
          { ...state, runId: event.runId, messages, error: event.errorMessage ?? "unknown error" },
          ids,
        ),
        ids,
      );
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

/**
 * Joins persisted history with live transport messages.
 *
 * Live entries carry either persisted ids (adopted from terminal events or
 * the server echo) or client-side streaming ids (`draft:`); history always
 * wins on id equality, so a finished run converges to the persisted rows.
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
  /**
   * Persisted id of the assistant message that issued this tool call (the
   * narration segment streamed so far). Injected by the gateway alongside
   * `runId`.
   */
  assistantMessageId: string;
}

export interface ToolResultPayload {
  callId: string;
  name: string;
  isError: boolean;
  preview: string;
  durationMs: number;
}

export interface UserInputPayload {
  runId: string;
  /**
   * Client-issued request id behind the commit, when the staged key carries
   * one. Identifies the exact optimistic bubble (`local:{requestId}`) even
   * when several identical texts are in flight. `null` falls back to
   * run-scoped FIFO.
   */
  requestId: string | null;
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

  // The committed follow-up supersedes exactly one optimistic bubble. A
  // linked request id matches by identity, so identical texts can never
  // consume each other and arrival order does not matter. Unlinked commits
  // fall back to the oldest bubble of this run: the server commits
  // follow-ups in send order. Bubbles from other runs never qualify.
  const requestId = payload.requestId ?? null;
  let withoutLocal = state.messages;
  const exactIndex =
    requestId === null
      ? -1
      : withoutLocal.findIndex(
          (message) => message.id === `local:${requestId}`,
        );
  const localIndex =
    exactIndex >= 0
      ? exactIndex
      : withoutLocal.findIndex(
          (message) =>
            message.id.startsWith("local:") &&
            (message.runId === undefined || message.runId === payload.runId),
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
  // The narration streamed so far is now persisted under the issuing
  // assistant message id: adopt it onto the newest unadopted assistant
  // entry (the streaming draft, else the latest sealed segment) so every
  // narration segment maps 1:1 and later deltas start a fresh draft.
  const draftId = `draft:${payload.runId}`;
  let messages = state.messages;
  const parentId =
    typeof payload.assistantMessageId === "string"
      ? payload.assistantMessageId
      : null;
  if (parentId !== null) {
    const streamingIndex = messages.findIndex(
      (message) => message.id === draftId,
    );
    if (streamingIndex >= 0) {
      messages = messages.map((message, current) =>
        current === streamingIndex ? { ...message, id: parentId } : message,
      );
    } else {
      for (let current = messages.length - 1; current >= 0; current--) {
        const id = messages[current].id;
        if (
          (id === draftId || id.startsWith(`${draftId}:`)) &&
          id.includes(":done")
        ) {
          messages = messages.map((message, index) =>
            index === current ? { ...message, id: parentId } : message,
          );
          break;
        }
      }
    }
  }
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
  return { ...state, messages: upsertToolMessage(messages, message) };
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
