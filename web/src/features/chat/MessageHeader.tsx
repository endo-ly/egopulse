import type { ChatMessage } from "../../shared/api/types";

interface MessageHeaderProps {
  senderId: string;
  senderKind: ChatMessage["sender_kind"];
  timestamp?: string;
  /** agent id → avatar object URL; assistant rows use it when present. */
  agentAvatars?: Record<string, string>;
}

function senderLabel(
  senderId: string,
  senderKind: ChatMessage["sender_kind"],
): string {
  if (senderKind === "user") return "You";
  return senderId;
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

function avatarLetter(senderId: string): string {
  return (senderId[0] ?? "?").toUpperCase();
}

/** Avatar, sender label, and optional timestamp shared by every timeline row. */
export function MessageHeader({
  senderId,
  senderKind,
  timestamp,
  agentAvatars,
}: MessageHeaderProps) {
  const avatarSrc =
    senderKind === "assistant" ? agentAvatars?.[senderId] : undefined;

  return (
    <div className="message-header">
      {senderKind !== "user" && (
        <div className="message-avatar">
          {avatarSrc ? (
            <img src={avatarSrc} alt="" />
          ) : (
            avatarLetter(senderId)
          )}
        </div>
      )}
      <span className="message-sender">
        {senderLabel(senderId, senderKind)}
      </span>
      {timestamp && (
        <span className="message-time" title={timestamp}>
          {formatTimestamp(timestamp)}
        </span>
      )}
    </div>
  );
}
