import { memo } from "react";
import type { ChatMessage } from "../../shared/api/types";
import { MarkdownRenderer } from "../../shared/ui/MarkdownRenderer";
import { MessageHeader } from "./MessageHeader";
import { PulseTrace } from "./PulseTrace";
import type { PulseTraceData } from "./timelineEntries";

export interface MessageBubbleProps {
  message: ChatMessage;
  /** agent id → avatar object URL; assistant messages use it when present. */
  agentAvatars?: Record<string, string>;
  /** Folded activation trace when this message is a delivered Pulse output. */
  pulseTrace?: PulseTraceData;
  /** Bearer token used to fetch attachment blobs in a folded trace. */
  authToken?: string;
}

export const MessageBubble = memo(function MessageBubble({
  message,
  agentAvatars,
  pulseTrace,
  authToken,
}: MessageBubbleProps) {
  const cls = `message-row bubble-${message.sender_kind}`;

  return (
    <div className={cls}>
      <MessageHeader
        senderId={message.sender_id}
        senderKind={message.sender_kind}
        timestamp={message.timestamp}
        agentAvatars={agentAvatars}
      />
      {pulseTrace && <PulseTrace trace={pulseTrace} authToken={authToken} />}
      <div className="message-body">
        {message.sender_kind === "assistant" ? (
          <MarkdownRenderer content={message.content} />
        ) : (
          <>{message.content}</>
        )}
      </div>
    </div>
  );
});
