import { Timeline } from "./Timeline";
import { MessageBubble } from "./MessageBubble";
import { MessageHeader } from "./MessageHeader";
import { ToolCard } from "./ToolCard";
import { AttachmentCard } from "./AttachmentCard";
import { Composer } from "./Composer";
import { ReadOnlyBanner } from "./ReadOnlyBanner";
import { Spinner } from "../../shared/ui/Spinner";
import { parseToolEvent, toolAttachments, toolCaption } from "./toolEvent";
import type { TimelineEntry } from "./timelineEntries";

export interface ChatTabProps {
  channel: string;
  readOnly: boolean;
  /** Session the timeline belongs to; changing it resets scroll position. */
  sessionKey?: string;
  /** The agent that owns the session, used by progress and tool rows. */
  agentId?: string;
  /** Top-level timeline rows, with Pulse traces folded under their outputs. */
  entries?: TimelineEntry[];
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
  /** Bearer token used to fetch attachment blobs. */
  authToken?: string;
}

/**
 * A progress or tool row belongs to the assistant's turn. When the preceding
 * row is already assistant-side, that row carries the turn's header.
 */
function startsAssistantTurn(entries: TimelineEntry[], index: number): boolean {
  if (index === 0) return true;
  const previous = entries[index - 1].message;
  return previous.sender_kind !== "assistant" && previous.sender_kind !== "tool";
}

export function ChatTab({
  channel,
  readOnly,
  sessionKey,
  agentId,
  entries = [],
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
  authToken = "",
}: ChatTabProps) {
  const waitingStartsTurn = startsAssistantTurn(entries, entries.length);

  return (
    <div className="chat-tab">
      <Timeline
        sessionKey={sessionKey}
        jumpRequest={jumpRequest}
        onReachTop={onLoadOlder}
        atHistoryStart={atHistoryStart}
        prependSeq={olderLoadSeq}
      >
        {entries.map(({ message: m, pulseTrace }, index) => {
          const toolEvent = parseToolEvent(m);
          if (toolEvent) {
            const attachments = toolAttachments(toolEvent);
            if (attachments.length > 0) {
              const caption = toolCaption(toolEvent);
              return (
                <div key={m.id} className="message-row bubble-tool">
                  {agentId && startsAssistantTurn(entries, index) && (
                    <MessageHeader
                      senderId={agentId}
                      senderKind="assistant"
                      timestamp={m.timestamp}
                      agentAvatars={agentAvatars}
                    />
                  )}
                  {attachments.map((attachment) => (
                    <AttachmentCard
                      key={`${m.id}:${attachment.id}`}
                      attachment={attachment}
                      caption={caption}
                      authToken={authToken}
                    />
                  ))}
                </div>
              );
            }
            return (
              <div key={m.id} className="message-row bubble-tool">
                {agentId && startsAssistantTurn(entries, index) && (
                  <MessageHeader
                    senderId={agentId}
                    senderKind="assistant"
                    timestamp={m.timestamp}
                    agentAvatars={agentAvatars}
                  />
                )}
                <ToolCard event={toolEvent} />
              </div>
            );
          }
          return (
            <MessageBubble
              key={m.id}
              message={m}
              agentAvatars={agentAvatars}
              pulseTrace={pulseTrace}
              authToken={authToken}
            />
          );
        })}
        {waitingForAssistant && (
          <div className="message-row bubble-assistant">
            {agentId && waitingStartsTurn && (
              <MessageHeader
                senderId={agentId}
                senderKind="assistant"
                agentAvatars={agentAvatars}
              />
            )}
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
