import type { ReactNode } from "react";
import { Button } from "./Button";
import { StatusDot, type StatusTone } from "./StatusDot";

export type HealthStatus = "ok" | "degraded" | "down";

export interface SidebarProps {
  onNewSession: () => void;
  agents?: ReactNode;
  sessions?: ReactNode;
  healthStatus?: HealthStatus;
  activeTurns?: number;
  version?: string;
}

export function healthTone(status: HealthStatus): StatusTone {
  switch (status) {
    case "ok":
      return "live";
    case "down":
      return "error";
    case "degraded":
      return "idle";
  }
}

export function Sidebar({
  onNewSession,
  agents,
  sessions,
  healthStatus = "ok",
  activeTurns = 0,
  version = "0.1.0",
}: SidebarProps) {
  return (
    <nav className="sidebar-nav" aria-label="Sidebar">
      <div className="sidebar-brand">
        <svg
          className="sidebar-brand-logo"
          width="32"
          height="32"
          viewBox="0 0 32 32"
          aria-hidden="true"
        >
          <path
            d="M16 3 L29 16 L16 29 L3 16 Z"
            fill="none"
            stroke="var(--color-accent)"
            strokeWidth="2"
          />
          <path
            d="M16 9 L23 16 L16 23 L9 16 Z"
            fill="var(--color-accent-2-soft)"
            stroke="var(--color-accent-2)"
            strokeWidth="1.5"
          />
        </svg>
        <span className="sidebar-brand-name">EgoPulse</span>
        <span className="sidebar-brand-version">v{version}</span>
      </div>

      <div className="sidebar-body">
        <section className="sidebar-section">{agents}</section>
        <section className="sidebar-section">{sessions}</section>
      </div>

      <div className="sidebar-footer">
        <Button
          variant="secondary"
          className="new-session-btn"
          onClick={onNewSession}
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
          New Session
        </Button>
        <div className="sidebar-runtime-status" title="Runtime health">
          <StatusDot tone={healthTone(healthStatus)} />
          <span className="sidebar-runtime-text">
            {healthStatus} · {activeTurns} turn{activeTurns === 1 ? "" : "s"} live
          </span>
        </div>
      </div>
    </nav>
  );
}
