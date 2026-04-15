import type { SessionItem } from "../types";

type SidebarProps = {
  version: string;
  sessions: SessionItem[];
  selectedSession: string;
  onNewSession: () => void;
  onSelectSession: (key: string) => void;
  onOpenSettings: () => void;
  isOpen: boolean;
  onToggle: () => void;
};

export function Sidebar({
  version,
  sessions,
  selectedSession,
  onNewSession,
  onSelectSession,
  onOpenSettings,
  isOpen,
  onToggle,
}: SidebarProps) {
  return (
    <aside
      className={`sidebar flex flex-col gap-3 p-5 border-r border-border bg-[rgba(4,8,18,0.85)] backdrop-blur-[20px] overflow-hidden${isOpen ? " open" : ""}`}
    >
      <div className="flex items-center gap-3">
        <img src="/icon.png" alt="EgoPulse" className="w-11 h-11 rounded-[14px]" />
        <div>
          <h1 className="m-0 text-lg leading-tight">EgoPulse</h1>
          <p className="mt-1 text-sm text-muted">{version ? `v${version}` : "Web"}</p>
        </div>
        <button className="sidebar-close ml-auto" onClick={onToggle}>
          <svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <line x1="4" y1="4" x2="14" y2="14" />
            <line x1="14" y1="4" x2="4" y2="14" />
          </svg>
        </button>
      </div>

      <button className="primary-button" onClick={onNewSession}>
        New Session
      </button>
      <button className="secondary-button" onClick={onOpenSettings}>
        Runtime Config
      </button>

      <div className="flex flex-1 min-h-0 flex-col gap-2.5">
        <div className="flex justify-between items-center">
          <h2 className="m-0 text-sm">Sessions</h2>
          <span className="inline-flex items-center justify-center min-w-[28px] h-7 rounded-full bg-panel-2 text-muted text-xs px-2">
            {sessions.length}
          </span>
        </div>
        <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-auto">
          {sessions.map((item) => (
            <button
              key={item.session_key}
              className={`session-item ${item.session_key === selectedSession ? "active" : ""}`}
              onClick={() => onSelectSession(item.session_key)}
            >
              <strong>{item.label}</strong>
              <span className="session-channel-badge">{item.channel}</span>
              <small>{item.last_message_preview || "No messages yet"}</small>
            </button>
          ))}
        </div>
      </div>
    </aside>
  );
}
