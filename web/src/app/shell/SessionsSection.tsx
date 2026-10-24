import { useEffect, useState, type ReactNode } from "react";
import { Badge } from "../../shared/ui/Badge";
import { Card } from "../../shared/ui/Card";
import { EmptyState } from "../../shared/ui/EmptyState";
import { channelLabel } from "../../shared/lib/format";
import type { SessionEntry } from "../../shared/api/types";
import type { SessionPatch } from "../../shared/api/sessions";

export interface SessionsSectionProps {
  sessions: SessionEntry[];
  selectedAgent: string;
  selectedSession: string;
  onSelectSession: (key: string) => void;
  onNewSession?: (name?: string) => void;
  onUpdateSession?: (chatId: number, patch: SessionPatch) => void;
  /** Session keys with messages newer than the last view. */
  unreadSessionKeys?: ReadonlySet<string>;
}

const CHANNEL_FILTERS = [
  "all",
  "web",
  "discord",
  "telegram",
  "cli",
  "tui",
  "voice",
] as const;

type ChannelFilter = (typeof CHANNEL_FILTERS)[number];

const CHANNEL_LABELS: Record<ChannelFilter, string> = {
  all: "All",
  web: "Web",
  discord: "Discord",
  telegram: "Telegram",
  cli: "CLI",
  tui: "TUI",
  voice: "Voice",
};

const UNTITLED = "Untitled session";

function emptyIcon(path: string): ReactNode {
  return (
    <svg
      width="24"
      height="24"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={path} />
    </svg>
  );
}

const NO_SESSIONS_ICON = emptyIcon("M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z");

/** Primary label for a session row: its name, else the last message preview. */
export function sessionTitle(session: SessionEntry): string {
  const name = session.name?.trim();
  if (name) return name;
  const preview = session.last_message_preview.trim();
  return preview || UNTITLED;
}

/** Secondary line: the preview when the row is already identified by a name. */
function sessionSubtitle(session: SessionEntry): string {
  const name = session.name?.trim();
  if (!name) return "";
  return session.last_message_preview.trim();
}

interface SessionRowProps {
  session: SessionEntry;
  active: boolean;
  unread: boolean;
  onSelect: () => void;
  onUpdateSession?: (chatId: number, patch: SessionPatch) => void;
}

function SessionRow({
  session,
  active,
  unread,
  onSelect,
  onUpdateSession,
}: SessionRowProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState(session.name ?? "");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setDraft(session.name ?? "");
  }, [session.name]);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);

  const subtitle = sessionSubtitle(session);

  const commitRename = () => {
    const next = draft.trim();
    setRenaming(false);
    if (!onUpdateSession) return;
    if (next === (session.name ?? "")) return;
    onUpdateSession(session.chat_id, { name: next === "" ? null : next });
  };

  const copySessionId = async () => {
    setMenuOpen(false);
    const sessionId = `chat:${session.chat_id}`;
    try {
      await navigator.clipboard.writeText(sessionId);
      setCopied(true);
    } catch {
      setCopied(false);
      setRenaming(false);
      window.prompt("Session ID", sessionId);
    }
  };

  return (
    <li>
      <Card active={active} onClick={renaming ? undefined : onSelect}>
        <div className={`session-item${unread ? " unread" : ""}`}>
          <Badge kind="channel">{channelLabel(session.channel)}</Badge>
          {renaming ? (
            <input
              className="session-rename-input"
              value={draft}
              autoFocus
              aria-label="Session name"
              onChange={(event) => setDraft(event.target.value)}
              onClick={(event) => event.stopPropagation()}
              onBlur={commitRename}
              onKeyDown={(event) => {
                event.stopPropagation();
                if (event.key === "Enter") commitRename();
                if (event.key === "Escape") {
                  setDraft(session.name ?? "");
                  setRenaming(false);
                }
              }}
            />
          ) : (
            <span className="session-text">
              <span className="session-title">{sessionTitle(session)}</span>
              {subtitle ? <span className="session-preview">{subtitle}</span> : null}
            </span>
          )}
          {copied ? <span className="session-copied">Copied</span> : null}
          {onUpdateSession ? (
            <div className="session-actions">
              <button
                type="button"
                className="session-menu-btn"
                aria-label="Session actions"
                aria-expanded={menuOpen}
                onClick={(event) => {
                  event.stopPropagation();
                  setMenuOpen((open) => !open);
                }}
              >
                ⋯
              </button>
              {menuOpen ? (
                <div className="session-menu" role="menu">
                  <button
                    type="button"
                    role="menuitem"
                    onClick={(event) => {
                      event.stopPropagation();
                      setMenuOpen(false);
                      setRenaming(true);
                    }}
                  >
                    Rename
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={(event) => {
                      event.stopPropagation();
                      setMenuOpen(false);
                      onUpdateSession(session.chat_id, { pinned: !session.pinned });
                    }}
                  >
                    {session.pinned ? "Unpin" : "Pin"}
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={(event) => {
                      event.stopPropagation();
                      setMenuOpen(false);
                      onUpdateSession(session.chat_id, { archived: !session.archived });
                    }}
                  >
                    {session.archived ? "Restore" : "Archive"}
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={(event) => {
                      event.stopPropagation();
                      void copySessionId();
                    }}
                  >
                    Copy session ID
                  </button>
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
      </Card>
    </li>
  );
}

export function SessionsSection({
  sessions,
  selectedAgent,
  selectedSession,
  onSelectSession,
  onNewSession,
  onUpdateSession,
  unreadSessionKeys,
}: SessionsSectionProps) {
  const [channelFilter, setChannelFilter] = useState<ChannelFilter>("all");
  const [archivedOpen, setArchivedOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");

  const agentSessions = sessions
    .filter((s) => s.agent_id === selectedAgent)
    .filter((s) => channelFilter === "all" || s.channel === channelFilter);

  // Archived sessions take precedence over the pin flag so a pinned session can
  // be archived without losing its pin state.
  const pinned = agentSessions.filter((s) => !s.archived && s.pinned);
  const active = agentSessions.filter((s) => !s.archived && !s.pinned);
  const archived = agentSessions.filter((s) => s.archived);

  const renderRow = (session: SessionEntry) => (
    <SessionRow
      key={session.session_key}
      session={session}
      active={selectedSession === session.session_key}
      unread={unreadSessionKeys?.has(session.session_key) ?? false}
      onSelect={() => onSelectSession(session.session_key)}
      onUpdateSession={onUpdateSession}
    />
  );

  return (
    <div className="sessions-section">
      <div className="sessions-header">
        <h2 className="section-title">SESSIONS</h2>
        <div className="sessions-header-actions">
          <select
            className="channel-filter"
            value={channelFilter}
            onChange={(e) => setChannelFilter(e.target.value as ChannelFilter)}
            aria-label="Filter sessions by channel"
          >
            {CHANNEL_FILTERS.map((c) => (
              <option key={c} value={c}>
                {CHANNEL_LABELS[c]}
              </option>
            ))}
          </select>
          {onNewSession && (
            <button
              type="button"
              className="sessions-new-btn"
              onClick={() => setCreating((open) => !open)}
              aria-label="New Session"
              title="New Session"
            >
              <svg
                width="15"
                height="15"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                strokeLinecap="round"
                aria-hidden="true"
              >
                <line x1="12" y1="5" x2="12" y2="19" />
                <line x1="5" y1="12" x2="19" y2="12" />
              </svg>
            </button>
          )}
        </div>
      </div>

      {creating && onNewSession ? (
        <form
          className="sessions-create-form"
          onSubmit={(event) => {
            event.preventDefault();
            const name = newName.trim();
            setCreating(false);
            setNewName("");
            onNewSession(name === "" ? undefined : name);
          }}
        >
          <input
            className="session-rename-input"
            value={newName}
            autoFocus
            placeholder="Session name (optional)"
            aria-label="New session name"
            onChange={(event) => setNewName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                setCreating(false);
                setNewName("");
              }
            }}
          />
          <button type="submit" className="sessions-create-submit">
            Create
          </button>
        </form>
      ) : null}

      {agentSessions.length === 0 ? (
        sessions.some((s) => s.agent_id === selectedAgent) ? (
          <EmptyState
            icon={NO_SESSIONS_ICON}
            title="No matches"
            description={`No ${CHANNEL_LABELS[channelFilter]} sessions for this agent.`}
          />
        ) : (
          <EmptyState
            icon={NO_SESSIONS_ICON}
            title="No sessions"
            description="No sessions yet. Start a new conversation."
          />
        )
      ) : (
        <>
          {pinned.length > 0 ? (
            <div className="sessions-group">
              <h3 className="sessions-group-title">PINNED</h3>
              <ul className="sessions-list">{pinned.map(renderRow)}</ul>
            </div>
          ) : null}
          {active.length > 0 ? (
            <ul className="sessions-list">{active.map(renderRow)}</ul>
          ) : null}
          {archived.length > 0 ? (
            <div className="sessions-group">
              <button
                type="button"
                className="sessions-group-toggle"
                aria-expanded={archivedOpen}
                onClick={() => setArchivedOpen((open) => !open)}
              >
                ARCHIVED ({archived.length})
              </button>
              {archivedOpen ? (
                <ul className="sessions-list">{archived.map(renderRow)}</ul>
              ) : null}
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}
