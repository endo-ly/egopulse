import { StatusDot } from "./StatusDot";
import { type HealthStatus, healthTone } from "./Sidebar";

export type TabId = "chat" | "sleep" | "pulse" | "metrics" | "config";

export interface TopBarProps {
  activeTab: TabId;
  onTabChange: (tab: TabId) => void;
  onOpenPalette: () => void;
  healthStatus: HealthStatus;
  healthSummary?: string;
}

interface TabDef {
  id: TabId;
  label: string;
  disabled: boolean;
}

const TABS: TabDef[] = [
  { id: "chat", label: "Chat", disabled: false },
  { id: "sleep", label: "Sleep", disabled: true },
  { id: "pulse", label: "Pulse", disabled: true },
  { id: "metrics", label: "Metrics", disabled: true },
  { id: "config", label: "Config", disabled: true },
];

export function TopBar({
  activeTab,
  onTabChange,
  onOpenPalette,
  healthStatus,
  healthSummary,
}: TopBarProps) {
  return (
    <div className="topbar-content">
      <button
        type="button"
        className="palette-trigger"
        onClick={onOpenPalette}
        aria-label="Open command palette"
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
        <span className="palette-trigger-label">Search or jump…</span>
        <kbd className="palette-trigger-kbd">⌘K</kbd>
      </button>

      <nav className="tabs" aria-label="Primary navigation">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            className={`tab ${activeTab === tab.id ? "active" : ""}`}
            disabled={tab.disabled}
            aria-current={activeTab === tab.id ? "page" : undefined}
            onClick={() => onTabChange(tab.id)}
          >
            {tab.label}
          </button>
        ))}
      </nav>

      <div className="health-badge" title="Runtime health">
        <StatusDot tone={healthTone(healthStatus)} />
        <span className="health-badge-text">
          {healthSummary ?? healthStatus}
        </span>
      </div>
    </div>
  );
}
