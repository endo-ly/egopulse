import { memo } from "react";
import type { ChatMessage } from "../../shared/api/types";
import { MarkdownRenderer } from "../../shared/ui/MarkdownRenderer";
import { PulseTrace } from "./PulseTrace";
import type { PulseTraceData } from "./timelineEntries";

export interface MessageBubbleProps {
  message: ChatMessage;
  /** agent id → avatar object URL; assistant messages use it when present. */
  agentAvatars?: Record<string, string>;
  /** Folded activation trace when this message is a delivered Pulse output. */
  pulseTrace?: PulseTraceData;
}

function senderLabel(message: ChatMessage): string {
  if (message.sender_kind === "user") return "You";
  return message.sender_id;
}

function formatTimestamp(ts: string): string {
  const d = new Date(ts);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  if (sameDay) return `${hh}:${mm}`;
  const mo = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${mo}/${dd} ${hh}:${mm}`;
}

function avatarLetter(message: ChatMessage): string {
  return (message.sender_id[0] ?? "?").toUpperCase();
}

export const MessageBubble = memo(function MessageBubble({
  message,
  agentAvatars,
  pulseTrace,
}: MessageBubbleProps) {
  const cls = `message-row bubble-${message.sender_kind}`;
  const avatarSrc =
    message.sender_kind === "assistant"
      ? agentAvatars?.[message.sender_id]
      : undefined;

  return (
    <div className={cls}>
      <div className="message-header">
        {message.sender_kind !== "user" && (
          <div className="message-avatar">
            {avatarSrc ? (
              <img src={avatarSrc} alt="" />
            ) : (
              avatarLetter(message)
            )}
          </div>
        )}
        <span className="message-sender">{senderLabel(message)}</span>
        <span
          className="message-time"
          title={message.timestamp}
        >
          {formatTimestamp(message.timestamp)}
        </span>
      </div>
      {pulseTrace && <PulseTrace trace={pulseTrace} />}
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
