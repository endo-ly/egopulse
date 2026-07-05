import { useMemo, useState, type KeyboardEvent } from "react";
import { Badge } from "../../shared/ui/Badge";
import { Timeline } from "./Timeline";
import { MessageBubble } from "./MessageBubble";
import { ToolCard } from "./ToolCard";
import { Composer } from "./Composer";
import { ReadOnlyBanner } from "./ReadOnlyBanner";
import { channelLabel } from "../../shared/lib/format";
import type { ChatMessage, ToolEventData } from "../../shared/api/types";

export interface ChatTabProps {
  sessionLabel: string;
  channel: string;
  readOnly: boolean;
  messages?: ChatMessage[];
  onSend?: (text: string) => void;
  storageKey?: string;
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
  sessionLabel,
  channel,
  readOnly,
  messages = [],
  onSend,
  storageKey,
}: ChatTabProps) {
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [matchIndex, setMatchIndex] = useState(0);

  const searchTarget = useMemo(() => messages.map((m) => m.content), [messages]);
  const searchMatches = useMemo(() => {
    if (!searchQuery) return [];
    const q = searchQuery.toLowerCase();
    const result: number[] = [];
    searchTarget.forEach((content, messageIndex) => {
      const lower = content.toLowerCase();
      let idx = lower.indexOf(q);
      while (idx !== -1) {
        result.push(messageIndex);
        idx = lower.indexOf(q, idx + q.length);
      }
    });
    return result;
  }, [searchQuery, searchTarget]);

  const openSearch = () => {
    setSearchOpen(true);
    setSearchQuery("");
    setMatchIndex(0);
  };
  const closeSearch = () => setSearchOpen(false);

  const handleSearchKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (searchMatches.length === 0) return;
      if (e.shiftKey) {
        setMatchIndex((i) => (i <= 0 ? searchMatches.length - 1 : i - 1));
      } else {
        setMatchIndex((i) => (i + 1) % searchMatches.length);
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
    }
  };

  return (
    <div className="chat-tab">
      <header className="chat-header">
        <div className="chat-header-info">
          <span className="chat-header-label">{sessionLabel}</span>
          <Badge kind="channel">{channelLabel(channel)}</Badge>
          {readOnly && (
            <span className="chat-header-meta">Read-only</span>
          )}
        </div>
        {searchOpen ? (
          <div className="chat-search">
            <svg
              className="chat-search-icon"
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              aria-hidden="true"
            >
              <circle cx="11" cy="11" r="7" />
              <line x1="21" y1="21" x2="16.65" y2="16.65" />
            </svg>
            <input
              type="text"
              className="chat-search-input"
              placeholder="Search…"
              value={searchQuery}
              onChange={(e) => {
                setSearchQuery(e.target.value);
                setMatchIndex(0);
              }}
              onKeyDown={handleSearchKey}
              autoFocus
            />
            {searchQuery && (
              <span className="chat-search-count">
                {searchMatches.length > 0
                  ? `${matchIndex + 1} / ${searchMatches.length}`
                  : "0 / 0"}
              </span>
            )}
            <button
              type="button"
              className="chat-search-close"
              onClick={closeSearch}
              aria-label="Close search"
            >
              ✕
            </button>
          </div>
        ) : (
          <button
            type="button"
            className="chat-search-btn btn-icon"
            onClick={openSearch}
            disabled={messages.length === 0}
            aria-label="Search messages"
            title="Search messages"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <circle cx="11" cy="11" r="7" />
              <line x1="21" y1="21" x2="16.65" y2="16.65" />
            </svg>
          </button>
        )}
      </header>
      <Timeline
        searchMatches={searchOpen ? searchMatches : undefined}
        activeMatchIndex={matchIndex}
      >
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
