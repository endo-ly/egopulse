import { StatusDot } from "../../shared/ui/StatusDot";
import { healthTone, type HealthStatus } from "../runtimeStatus";
import { NAV_TABS } from "../navigation";
import type { TabId } from "../navigation";

export interface MobileBarProps {
  activeTab: TabId;
  onTabChange: (tab: TabId) => void;
  onOpenPalette: () => void;
  onToggleSidebar: () => void;
  sidebarOpen: boolean;
  healthStatus: HealthStatus;
}

/** Slim mobile-only top bar: sidebar toggle, tab selector, palette trigger. */
export function MobileBar({
  activeTab,
  onTabChange,
  onOpenPalette,
  onToggleSidebar,
  sidebarOpen,
  healthStatus,
}: MobileBarProps) {
  return (
    <div className="mobilebar-content">
      <button
        type="button"
        className="hamburger-btn"
        aria-label="Toggle sidebar"
        aria-expanded={sidebarOpen}
        onClick={onToggleSidebar}
      >
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          aria-hidden="true"
        >
          <line x1="3" y1="6" x2="21" y2="6" />
          <line x1="3" y1="12" x2="21" y2="12" />
          <line x1="3" y1="18" x2="21" y2="18" />
        </svg>
      </button>
      <select
        className="tab-select"
        aria-label="Primary navigation"
        value={activeTab}
        onChange={(e) => onTabChange(e.target.value as TabId)}
      >
        {NAV_TABS.map((tab) => (
          <option key={tab.id} value={tab.id} disabled={tab.disabled}>
            {tab.label}
          </option>
        ))}
      </select>
      <div className="mobilebar-status">
        <StatusDot tone={healthTone(healthStatus)} />
      </div>
      <button
        type="button"
        className="topbar-palette-btn"
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
          aria-hidden="true"
        >
          <circle cx="11" cy="11" r="7" />
          <line x1="21" y1="21" x2="16.65" y2="16.65" />
        </svg>
      </button>
    </div>
  );
}
