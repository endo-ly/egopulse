import { StatusBadge } from "./StatusBadge";
import { MessageBubble } from "./MessageBubble";
import { Composer } from "./Composer";
import type { FormEvent } from "react";
import type { MessageItem } from "../types";

type ChatPanelProps = {
  selectedLabel: string;
  wsState: string;
  authEnabled: boolean;
  status: { tone: "idle" | "ok" | "error"; text: string };
  messages: MessageItem[];
  messageEndRef: React.RefObject<HTMLDivElement | null>;  draft: string;
  setDraft: (value: string) => void;
  onSend: (event: FormEvent) => void;
  onToggleSidebar: () => void;
};

export function ChatPanel({
  selectedLabel,
  wsState,
  authEnabled,
  status,
  messages,
  messageEndRef,
  draft,
  setDraft,
  onSend,
  onToggleSidebar,
}: ChatPanelProps) {
  return (
    <main className="main-panel">
      <header className="chat-header">
        <button className="hamburger-button" onClick={onToggleSidebar}>
          <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <line x1="3" y1="5" x2="17" y2="5" />
            <line x1="3" y1="10" x2="17" y2="10" />
            <line x1="3" y1="15" x2="17" y2="15" />
          </svg>
        </button>
        <div>
          <h2>{selectedLabel || "Select a session"}</h2>
          <p>
            Gateway: {wsState}
            {authEnabled ? " / auth enabled" : ""}
          </p>
        </div>
        <StatusBadge tone={status.tone} text={status.text} />
      </header>

      <section className="timeline">
        {messages.map((message) => (
          <MessageBubble key={message.id} message={message} />
        ))}
        <div ref={messageEndRef as React.RefObject<HTMLDivElement>} />
      </section>

      <Composer draft={draft} setDraft={setDraft} onSubmit={onSend} />
    </main>
  );
}
