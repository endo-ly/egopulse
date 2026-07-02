import { Badge } from "./Badge";

export interface ChatTabProps {
  sessionLabel: string;
  channel: string;
  messageCount: number;
  readOnly: boolean;
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
}: ChatTabProps) {
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
      <div className="timeline" />
      <div className="composer" />
    </div>
  );
}
