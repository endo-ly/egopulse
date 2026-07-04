import { Badge } from "../../shared/ui/Badge";
import { Timeline } from "./Timeline";
import { MessageBubble } from "./MessageBubble";
import { ToolCard } from "./ToolCard";
import { Composer } from "./Composer";
import { ReadOnlyBanner } from "./ReadOnlyBanner";
import type { ChatMessage, ToolEventData } from "../../shared/api/types";

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

function parseToolEvent(message: ChatMessage): ToolEventData | null {
  if (message.sender_kind !== "tool") return null;
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
    return {
      name: raw.tool,
      state: isError ? "error" : "success",
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
        {messages.map((m) => {
          const toolEvent = parseToolEvent(m);
          if (toolEvent) {
            return (
              <div key={m.id} className="message-row bubble-tool">
                <ToolCard event={toolEvent} />
              </div>
            );
          }
          return <MessageBubble key={m.id} message={m} />;
        })}
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
