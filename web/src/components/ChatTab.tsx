import { Badge } from "./Badge";
import { Timeline } from "./Timeline";
import { MessageBubble } from "./MessageBubble";
import { Composer } from "./Composer";
import { ReadOnlyBanner } from "./ReadOnlyBanner";
import type { ChatMessage } from "../types";

export interface ChatTabProps {
  sessionLabel: string;
  channel: string;
  messageCount: number;
  readOnly: boolean;
  messages?: ChatMessage[];
  onSend?: (text: string) => void;
  storageKey?: string;
}

function channelLabel(channel: string): string {
  const map: Record<string, string> = {
    web: "Web",
    discord: "Discord",
    telegram: "Telegram",
    cli: "CLI",
    tui: "TUI",
    voice: "Voice",
  };
  return map[channel] ?? channel;
}

export function ChatTab({
  sessionLabel,
  channel,
  messageCount,
  readOnly,
  messages = [],
  onSend,
  storageKey,
}: ChatTabProps) {
  const searchTarget = messages.map((m) => m.content).join("\n");

  return (
    <div className="chat-tab">
      <header className="chat-header">
        <div className="chat-header-info">
          <span className="chat-header-label">{sessionLabel}</span>
          <Badge kind="channel">{channelLabel(channel)}</Badge>
          <span className="chat-header-meta">
            {readOnly
              ? `${channelLabel(channel)} session · ${messageCount} messages · read-only`
              : `${messageCount} messages`}
          </span>
        </div>
      </header>
      <Timeline searchTarget={searchTarget}>
        {messages.map((m) => (
          <MessageBubble key={m.id} message={m} />
        ))}
      </Timeline>
      <div className="composer">
        {readOnly ? (
          <ReadOnlyBanner channel={channel} />
        ) : (
          <Composer onSubmit={onSend ?? (() => {})} storageKey={storageKey} />
        )}
      </div>
    </div>
  );
}
