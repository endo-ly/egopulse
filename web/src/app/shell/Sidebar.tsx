import type { ReactNode } from "react";
import { Button } from "../../shared/ui/Button";
import { StatusDot } from "../../shared/ui/StatusDot";
import { healthTone, type HealthStatus } from "../runtimeStatus";

export interface SidebarProps {
  onNewSession: () => void;
  agents?: ReactNode;
  sessions?: ReactNode;
  healthStatus?: HealthStatus;
  activeTurns?: number;
  version?: string;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
}

export function Sidebar({
  onNewSession,
  agents,
  sessions,
  healthStatus = "ok",
  activeTurns = 0,
  version = "0.1.0",
  collapsed = false,
  onToggleCollapse,
}: SidebarProps) {
  return (
    <nav className={`sidebar-nav ${collapsed ? "collapsed" : ""}`} aria-label="Sidebar">
      <div className="sidebar-brand">
        {!collapsed && (
          <>
            <span className="sidebar-brand-name">EgoPulse</span>
            <span className="sidebar-brand-version">v{version}</span>
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

      {!collapsed && (
        <div className="sidebar-body">
          {agents}
          {sessions}
        </div>
      )}

      <div className="sidebar-footer">
        <Button
          variant="secondary"
          className="new-session-btn"
          onClick={onNewSession}
          aria-label={collapsed ? "New Session" : undefined}
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
            <line x1="12" y1="5" x2="12" y2="19" />
            <line x1="5" y1="12" x2="19" y2="12" />
          </svg>
          {!collapsed && "New Session"}
        </Button>
        <div className="sidebar-runtime-status" title="Runtime health">
          <StatusDot tone={healthTone(healthStatus)} />
          {!collapsed && (
            <span className="sidebar-runtime-text">
              {healthStatus} · {activeTurns} turn{activeTurns === 1 ? "" : "s"} live
            </span>
          )}
        </div>
      </div>
    </nav>
  );
}
