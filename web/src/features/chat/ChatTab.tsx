import { Timeline } from "./Timeline";
import { MessageBubble } from "./MessageBubble";
import { ToolCard } from "./ToolCard";
import { Composer } from "./Composer";
import { ReadOnlyBanner } from "./ReadOnlyBanner";
import { Spinner } from "../../shared/ui/Spinner";
import type { ChatMessage, ToolEventData } from "../../shared/api/types";

export interface ChatTabProps {
  channel: string;
  readOnly: boolean;
  messages?: ChatMessage[];
  onSend?: (text: string, draftId: string) => Promise<boolean>;
  storageKey?: string;
  /** Jump request from the command palette: scroll to and flash the message. */
  jumpRequest?: { index: number; seq: number };
  /** agent id → avatar object URL; absent entries fall back to the letter. */
  agentAvatars?: Record<string, string>;
  /** Assistant progress as UI state: rendered as a typing indicator row. */
  waitingForAssistant?: boolean;
  /** Loads the next older history page. */
  onLoadOlder?: () => void;
  /** Retries a failed older-page request. */
  onRetryOlder?: () => void;
  loadingOlder?: boolean;
  olderError?: string | null;
  /** True once the loaded history reaches the first committed message. */
  atHistoryStart?: boolean;
  /** Bumped after older messages are prepended. */
  olderLoadSeq?: number;
}

function parseToolEvent(message: ChatMessage): ToolEventData | null {
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

export function ChatTab({
  channel,
  readOnly,
  messages = [],
  onSend,
  storageKey,
  jumpRequest,
  agentAvatars,
  waitingForAssistant = false,
  onLoadOlder,
  onRetryOlder,
  loadingOlder = false,
  olderError = null,
  atHistoryStart = false,
  olderLoadSeq = 0,
}: ChatTabProps) {
  return (
    <div className="chat-tab">
      <Timeline
        jumpRequest={jumpRequest}
        onReachTop={onLoadOlder}
        atHistoryStart={atHistoryStart}
        prependSeq={olderLoadSeq}
      >
        {messages.map((m) => {
          const toolEvent = parseToolEvent(m);
          if (toolEvent) {
            return (
              <div key={m.id} className="message-row bubble-tool">
                <ToolCard event={toolEvent} />
              </div>
            );
          }
          return <MessageBubble key={m.id} message={m} agentAvatars={agentAvatars} />;
        })}
        {waitingForAssistant && (
          <div className="message-row bubble-assistant">
            <div className="message-body">
              <span className="thinking-dots" aria-label="assistant is responding">
                <span />
                <span />
                <span />
              </span>
            </div>
          </div>
        )}
      </Timeline>
      {loadingOlder && (
        <div className="timeline-older-loading" role="status" aria-live="polite">
          <Spinner size="sm" />
          <span>Loading earlier messages…</span>
        </div>
      )}
      {olderError && !loadingOlder && (
        <div className="timeline-older-error" role="alert">
          <span>{olderError}</span>
          {onRetryOlder && (
            <button type="button" onClick={onRetryOlder}>
              Retry
            </button>
          )}
        </div>
      )}
      <div className="composer">
        {readOnly ? (
          <ReadOnlyBanner channel={channel} />
        ) : (
          <Composer
            onSubmit={onSend ?? (() => Promise.resolve(true))}
            storageKey={storageKey}
          />
        )}
      </div>
    </div>
  );
}
