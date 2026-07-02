import { useState, type ReactNode } from "react";
import { Badge } from "./Badge";
import { Card } from "./Card";
import { EmptyState } from "./EmptyState";

export interface SessionEntry {
  session_key: string;
  label: string;
  channel: string;
  agent_id: string;
  last_message_preview: string;
  last_message_time: number;
}

export interface SessionsSectionProps {
  sessions: SessionEntry[];
  selectedAgent: string;
  selectedSession: string;
  onSelectSession: (key: string) => void;
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

export function SessionsSection({
  sessions,
  selectedAgent,
  selectedSession,
  onSelectSession,
}: SessionsSectionProps) {
  const [channelFilter, setChannelFilter] = useState<ChannelFilter>("all");

  const agentSessions = sessions.filter((s) => s.agent_id === selectedAgent);
  const visible = agentSessions
    .filter((s) => channelFilter === "all" || s.channel === channelFilter)
    .sort((a, b) => b.last_message_time - a.last_message_time);

  return (
    <div className="sessions-section">
      <div className="sessions-header">
        <h2 className="section-title">SESSIONS</h2>
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
      </div>

      {visible.length === 0 ? (
        agentSessions.length === 0 ? (
          <EmptyState
            icon={NO_SESSIONS_ICON}
            title="No sessions"
            description="No sessions yet. Start a new conversation."
          />
        ) : (
          <EmptyState
            icon={NO_SESSIONS_ICON}
            title="No matches"
            description={`No ${CHANNEL_LABELS[channelFilter]} sessions for this agent.`}
          />
        )
      ) : (
        <ul className="sessions-list">
          {visible.map((s) => (
            <li key={s.session_key}>
              <Card
                active={selectedSession === s.session_key}
                onClick={() => onSelectSession(s.session_key)}
              >
                <div className="session-item">
                  <span className="session-label">{s.label}</span>
                  <Badge kind="channel">{s.channel}</Badge>
                  <span className="session-preview">{s.last_message_preview}</span>
                </div>
              </Card>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
