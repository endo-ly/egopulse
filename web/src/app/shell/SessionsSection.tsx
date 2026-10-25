import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { Badge } from "../../shared/ui/Badge";
import { Card } from "../../shared/ui/Card";
import { EmptyState } from "../../shared/ui/EmptyState";
import { channelLabel } from "../../shared/lib/format";

import {
  sessionSubtitle,
  sessionTime,
  sessionTitle,
} from "../../shared/lib/session";
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

interface SessionRowProps {
  session: SessionEntry;
  active: boolean;
  unread: boolean;
  onSelect: () => void;
  onUpdateSession?: (chatId: number, patch: SessionPatch) => void;
}

type MenuPosition = { x: number; y: number };
type CopyState = "idle" | "copied" | "failed";

const MENU_WIDTH = 168;
const MENU_MARGIN = 8;

function clampMenuPosition({ x, y }: MenuPosition): MenuPosition {
  const width = globalThis.innerWidth ?? 0;
  const height = globalThis.innerHeight ?? 0;
  const maxX = width > 0 ? width - MENU_WIDTH - MENU_MARGIN : x;
  const maxY = height > 0 ? height - MENU_MARGIN : y;
  return {
    x: Math.max(MENU_MARGIN, Math.min(x, maxX)),
    y: Math.max(MENU_MARGIN, Math.min(y, maxY)),
  };
}

function SessionRow({
  session,
  active,
  unread,
  onSelect,
  onUpdateSession,
}: SessionRowProps) {
  const [menu, setMenu] = useState<MenuPosition | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState(session.name ?? "");
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const cancelRenameRef = useRef(false);

  useEffect(() => {
    if (renaming) return;
    setDraft(session.name ?? "");
  }, [renaming, session.name]);

  useEffect(() => {
    if (copyState === "idle") return;
    const timer = setTimeout(
      () => setCopyState("idle"),
      copyState === "copied" ? 1500 : 2500,
    );
    return () => clearTimeout(timer);
  }, [copyState]);

  // The menu is portalled to the body so the scrollable sidebar cannot clip it,
  // and so sticky/transformed ancestors do not become its containing block.
  useEffect(() => {
    if (!menu) return;
    menuRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, [menu]);

  useLayoutEffect(() => {
    if (!menu) return;
    const rect = menuRef.current?.getBoundingClientRect();
    if (!rect || (rect.width === 0 && rect.height === 0)) return;
    const width = globalThis.innerWidth ?? 0;
    const height = globalThis.innerHeight ?? 0;
    const maxX = Math.max(MENU_MARGIN, width - rect.width - MENU_MARGIN);
    const maxY = Math.max(MENU_MARGIN, height - rect.height - MENU_MARGIN);
    if (menu.x > maxX || menu.y > maxY) {
      setMenu({ x: Math.min(menu.x, maxX), y: Math.min(menu.y, maxY) });
    }
  }, [menu]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onMouseDown = (event: globalThis.MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) {
        close();
        return;
      }
      if (menuRef.current?.contains(target)) return;
      if (triggerRef.current?.contains(target)) return;
      close();
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      close();
      triggerRef.current?.focus();
    };
    document.addEventListener("mousedown", onMouseDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    globalThis.addEventListener("scroll", close, true);
    globalThis.addEventListener("resize", close);
    return () => {
      document.removeEventListener("mousedown", onMouseDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
      globalThis.removeEventListener("scroll", close, true);
      globalThis.removeEventListener("resize", close);
    };
  }, [menu]);

  const subtitle = sessionSubtitle(session);
  const time = sessionTime(session);
  const sessionId = `chat:${session.chat_id}`;

  const startRename = () => {
    cancelRenameRef.current = false;
    setRenaming(true);
  };

  const commitRename = () => {
    if (cancelRenameRef.current) {
      cancelRenameRef.current = false;
      setDraft(session.name ?? "");
      setRenaming(false);
      return;
    }
    const next = draft.trim();
    setRenaming(false);
    if (!onUpdateSession) return;
    if (next === (session.name ?? "")) return;
    onUpdateSession(session.chat_id, { name: next === "" ? null : next });
  };

  const openMenuAt = (x: number, y: number) => {
    if (!onUpdateSession) return;
    setMenu(clampMenuPosition({ x, y }));
  };

  const handleContextMenu = (event: MouseEvent<HTMLLIElement>) => {
    if (!onUpdateSession || renaming) return;
    event.preventDefault();
    event.stopPropagation();
    openMenuAt(event.clientX, event.clientY);
  };

  const toggleMenu = () => {
    if (menu) {
      setMenu(null);
      return;
    }
    const rect = triggerRef.current?.getBoundingClientRect();
    openMenuAt(
      rect ? rect.right - MENU_WIDTH : 0,
      rect ? rect.bottom + 4 : 0,
    );
  };

  const copySessionId = () => {
    setMenu(null);
    const finish = (copied: boolean) => {
      setCopyState(copied ? "copied" : "failed");
      triggerRef.current?.focus();
    };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(sessionId).then(
        () => finish(true),
        () => finish(false),
      );
      return;
    }
    const textarea = document.createElement("textarea");
    textarea.value = sessionId;
    textarea.readOnly = true;
    textarea.style.position = "fixed";
    textarea.style.top = "0";
    textarea.style.left = "0";
    textarea.style.opacity = "0";
    document.body.appendChild(textarea);
    textarea.select();
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } catch {
      copied = false;
    } finally {
      textarea.remove();
    }
    finish(copied);
  };

  return (
    <li onContextMenu={handleContextMenu}>
      <Card active={active} onClick={renaming ? undefined : onSelect}>
        <div className={`session-item${unread ? " unread" : ""}`}>
          <Badge kind="channel">{channelLabel(session.channel)}</Badge>
          {renaming ? (
            <input
              className="session-rename-input"
              value={draft}
              autoFocus
              maxLength={200}
              aria-label="Session name"
              onChange={(event) => setDraft(event.target.value)}
              onClick={(event) => event.stopPropagation()}
              onBlur={commitRename}
              onKeyDown={(event) => {
                event.stopPropagation();
                if (event.key === "Enter") {
                  event.currentTarget.blur();
                }
                if (event.key === "Escape") {
                  cancelRenameRef.current = true;
                  event.currentTarget.blur();
                }
              }}
            />
          ) : (
            <span className="session-text">
              <span className="session-title-row">
                <span className="session-title">{sessionTitle(session)}</span>
                {time ? (
                  <span
                    className="session-time"
                    title={new Date(session.last_message_time).toLocaleString()}
                  >
                    {time}
                  </span>
                ) : null}
              </span>
              {subtitle ? <span className="session-preview">{subtitle}</span> : null}
            </span>
          )}
          {copyState !== "idle" ? (
            <span className="session-copied" role="status">
              {copyState === "copied" ? "Copied" : "Copy failed"}
            </span>
          ) : null}
          {onUpdateSession ? (
            <div className="session-actions">
              <button
                ref={triggerRef}
                type="button"
                className="session-menu-btn"
                aria-label="Session actions"
                aria-haspopup="menu"
                aria-expanded={menu !== null}
                onClick={(event) => {
                  event.stopPropagation();
                  toggleMenu();
                }}
              >
                ⋯
              </button>
            </div>
          ) : null}
        </div>
      </Card>
      {menu && onUpdateSession
        ? createPortal(
            <div
              ref={menuRef}
              className="session-menu"
              role="menu"
              aria-label="Session actions"
              style={{ top: menu.y, left: menu.x }}
              onContextMenu={(event) => event.preventDefault()}
            >
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setMenu(null);
                  startRename();
                }}
              >
                Rename
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setMenu(null);
                  onUpdateSession(session.chat_id, { pinned: !session.pinned });
                }}
              >
                {session.pinned ? "Unpin" : "Pin"}
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setMenu(null);
                  onUpdateSession(session.chat_id, { archived: !session.archived });
                }}
              >
                {session.archived ? "Restore" : "Archive"}
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  void copySessionId();
                }}
              >
                Copy session ID
              </button>
            </div>,
            document.body,
          )
        : null}
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
        <>
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
              maxLength={200}
              placeholder="Session name (optional)"
              aria-label="New session name"
              aria-describedby="session-name-hint"
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
          <p id="session-name-hint" className="sessions-create-hint">
            The name is saved after the first message.
          </p>
        </>
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
            <div className="sessions-group">
              <h3 className="sessions-group-title">ACTIVE</h3>
              <ul className="sessions-list">{active.map(renderRow)}</ul>
            </div>
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
