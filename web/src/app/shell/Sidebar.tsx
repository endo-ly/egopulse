import type { ReactNode } from "react";
import { StatusDot } from "../../shared/ui/StatusDot";
import { healthTone, type HealthStatus } from "../runtimeStatus";
import { NAV_TABS, type NavTab } from "../navigation";
import type { TabId } from "../navigation";

export interface SidebarProps {
  activeTab: TabId;
  onTabChange: (tab: TabId) => void;
  onOpenPalette: () => void;
  agents?: ReactNode;
  sessions?: ReactNode;
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
const SLEEP_ICON_PATH = "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z";
const PULSE_ICON_PATH = "M22 12h-4l-3 9L9 3l-3 9H2";
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

/** Operational views only; Config is a meta-level utility and lives in the footer. */
const navTabs = NAV_TABS.filter((tab) => tab.id !== "config");
const configTab = NAV_TABS.find((tab) => tab.id === "config");

function tabIcon(id: TabId): ReactNode {
  if (id === "chat") return navIcon(CHAT_ICON_PATH);
  if (id === "sleep") return navIcon(SLEEP_ICON_PATH);
  if (id === "pulse") return navIcon(PULSE_ICON_PATH);
  if (id === "metrics") return navIcon(METRICS_ICON_PATH);
  return navIcon(CONFIG_ICON_PATH);
}

export function Sidebar({
  activeTab,
  onTabChange,
  onOpenPalette,
  agents,
  sessions,
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
        {navTabs.map((tab) => (
          <button
            key={tab.id}
            type="button"
            className={`nav-row ${activeTab === tab.id ? "active" : ""}`}
            aria-current={activeTab === tab.id ? "page" : undefined}
            disabled={tab.disabled}
            title={tabTitle(tab)}
            onClick={() => onTabChange(tab.id)}
          >
            <span className="nav-row-icon">{tabIcon(tab.id)}</span>
            <span className="nav-row-label">{tab.label}</span>
          </button>
        ))}
      </div>

      {!collapsed && (
        <div className="sidebar-body">
          {agents}
          {sessions}
        </div>
      )}

      <div className="sidebar-footer">
        <div className="sidebar-runtime-status" title="Runtime health">
          <StatusDot tone={healthTone(healthStatus)} />
          {!collapsed && (
            <span className="sidebar-runtime-text">{healthStatus}</span>
          )}
        </div>
        {configTab && (
          <button
            type="button"
            className="config-btn"
            disabled={configTab.disabled}
            aria-label="Config"
            title={configTab.disabled ? "Config — coming soon" : "Config"}
            onClick={() => onTabChange(configTab.id)}
          >
            {tabIcon(configTab.id)}
          </button>
        )}
      </div>
    </nav>
  );
}
