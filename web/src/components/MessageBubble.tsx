import type { ChatMessage } from "../types";

export interface MessageBubbleProps {
  message: ChatMessage;
}

function senderLabel(message: ChatMessage): string {
  if (message.sender_kind === "user") return "You";
  if (message.message_kind === "pulse_notification") return "Pulse";
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
  if (message.sender_kind === "user") return "U";
  if (message.message_kind === "pulse_notification") return "P";
  return (message.sender_id[0] ?? "?").toUpperCase();
}

export function MessageBubble({ message }: MessageBubbleProps) {
  const cls = `message-row bubble-${message.sender_kind}`;
  const isDraft =
    message.id.startsWith("draft:") && !message.id.endsWith(":done");

  return (
    <div className={cls}>
      <div className="message-header">
        <div className="message-avatar">{avatarLetter(message)}</div>
        <span className="message-sender">{senderLabel(message)}</span>
        <span
          className="message-time"
          title={message.timestamp}
        >
          {formatTimestamp(message.timestamp)}
        </span>
        {message.message_kind === "pulse_notification" && (
          <span className="pulse-badge">Pulse</span>
        )}
      </div>
      <div className="message-body">
        {message.content}
        {isDraft && <span className="streaming-cursor" />}
      </div>
    </div>
  );
}
