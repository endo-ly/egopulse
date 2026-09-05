import { Timeline } from "./Timeline";
import { MessageBubble } from "./MessageBubble";
import { ToolCard } from "./ToolCard";
import { Composer } from "./Composer";
import { ReadOnlyBanner } from "./ReadOnlyBanner";
import type { ChatMessage, ToolEventData } from "../../shared/api/types";

export interface ChatTabProps {
  channel: string;
  readOnly: boolean;
  messages?: ChatMessage[];
  onSend?: (text: string) => void;
  storageKey?: string;
  /** Jump request from the command palette: scroll to and flash the message. */
  jumpRequest?: { index: number; seq: number };
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
}: ChatTabProps) {
  return (
    <div className="chat-tab">
      <Timeline jumpRequest={jumpRequest}>
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
