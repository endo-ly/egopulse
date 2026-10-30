import type { ReactNode } from "react";
import { StatusDot } from "../../shared/ui/StatusDot";
import { healthTone, type HealthStatus } from "../runtimeStatus";
import { AGENT_TABS, GLOBAL_TABS, type NavTab, type TabId } from "../navigation";

export interface SidebarProps {
  activeTab: TabId;
  onTabChange: (tab: TabId) => void;
  onOpenPalette: () => void;
  agents?: ReactNode;
  /** Screen-specific section below the agent list (sessions, agent sections, …). */
  context?: ReactNode;
  healthStatus?: HealthStatus;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
}

function navIcon(path: string): ReactNode {
  return (
    <svg
      width="15"
      height="15"
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

const CHAT_ICON_PATH = "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z";
/** An ID card: the agent's identity, memory, and abilities. */
const AGENT_ICON_PATH =
  "M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM7 11a2 2 0 1 0 4 0a2 2 0 1 0-4 0M6 16.5c.6-1.2 1.7-2 3-2s2.4.8 3 2M14 10h4M14 14h3";
const SCHEDULE_ICON_PATH = "M8 2v4M16 2v4M3 10h18M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z";
const METRICS_ICON_PATH = "M18 20V10M12 20V4M6 20v-6";
const CONFIG_ICON_PATH =
  "M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6";

const SEARCH_ICON = (
  <svg
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
);

const TAB_ICON_PATHS: Record<TabId, string> = {
  chat: CHAT_ICON_PATH,
  agent: AGENT_ICON_PATH,
  schedule: SCHEDULE_ICON_PATH,
  metrics: METRICS_ICON_PATH,
  config: CONFIG_ICON_PATH,
};

export function Sidebar({
  activeTab,
  onTabChange,
  onOpenPalette,
  agents,
  context,
  healthStatus = "ok",
  collapsed = false,
  onToggleCollapse,
}: SidebarProps) {
  const tabTitle = (tab: NavTab): string | undefined => {
    if (tab.disabled) return `${tab.label} — coming soon`;
    return collapsed ? tab.label : undefined;
  };

  return (
    <nav className={`sidebar-nav ${collapsed ? "collapsed" : ""}`} aria-label="Sidebar">
      <div className="sidebar-brand">
        {collapsed ? (
          <span className="sidebar-brand-mark" aria-hidden="true">
            E
          </span>
        ) : (
          <>
            <span className="sidebar-brand-name">EgoPulse</span>
            <button
              type="button"
              className="brand-search-btn"
              onClick={onOpenPalette}
              aria-label="Open command palette"
              title="Search or jump… (⌘K)"
            >
              {SEARCH_ICON}
            </button>
          </>
        )}
        {onToggleCollapse && !collapsed && (
          <button
            type="button"
            className="sidebar-collapse-btn"
            aria-label="Collapse sidebar"
            onClick={onToggleCollapse}
          >
            ‹
          </button>
        )}
        {onToggleCollapse && collapsed && (
          <button
            type="button"
            className="sidebar-expand-btn"
            aria-label="Expand sidebar"
            onClick={onToggleCollapse}
          >
            ›
          </button>
        )}
      </div>

      <div className="sidebar-nav-list" aria-label="Primary navigation">
        {AGENT_TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            className={`nav-row ${activeTab === tab.id ? "active" : ""}`}
            aria-current={activeTab === tab.id ? "page" : undefined}
            disabled={tab.disabled}
            title={tabTitle(tab)}
            onClick={() => onTabChange(tab.id)}
          >
            <span className="nav-row-icon">{navIcon(TAB_ICON_PATHS[tab.id])}</span>
            <span className="nav-row-label">{tab.label}</span>
          </button>
        ))}
      </div>

      {!collapsed && (
        <div className="sidebar-body">
          {agents}
          {context}
        </div>
      )}

      <div className="sidebar-footer">
        <div className="sidebar-runtime-status" title="Runtime health">
          <StatusDot tone={healthTone(healthStatus)} />
          {!collapsed && (
            <span className="sidebar-runtime-text">{healthStatus}</span>
          )}
        </div>
        <div className="footer-tabs" aria-label="Global screens">
          {GLOBAL_TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              className={`nav-row footer-tab ${activeTab === tab.id ? "active" : ""}`}
              disabled={tab.disabled}
              aria-current={activeTab === tab.id ? "page" : undefined}
              title={tabTitle(tab)}
              onClick={() => onTabChange(tab.id)}
            >
              <span className="nav-row-icon">{navIcon(TAB_ICON_PATHS[tab.id])}</span>
              <span className="nav-row-label">{tab.label}</span>
            </button>
          ))}
        </div>
      </div>
    </nav>
  );
}
