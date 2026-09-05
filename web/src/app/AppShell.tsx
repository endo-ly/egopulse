import { useState, useEffect, type ReactNode } from "react";
import { useMediaQuery } from "../shared/hooks/useMediaQuery";
import { Sidebar } from "./shell/Sidebar";
import { MobileBar } from "./shell/MobileBar";
import { AgentsSection } from "./shell/AgentsSection";
import { SessionsSection } from "./shell/SessionsSection";
import type { HealthStatus } from "./runtimeStatus";
import type { TabId } from "./navigation";
import type { AgentEntry, SessionEntry } from "../shared/api/types";

export interface AppProps {
  agents?: AgentEntry[];
  sessions?: SessionEntry[];
  selectedAgent?: string;
  selectedSession?: string;
  activeTab?: TabId;
  healthStatus?: HealthStatus;
  onSelectAgent?: (id: string) => void;
  onSelectSession?: (key: string) => void;
  onTabChange?: (tab: TabId) => void;
  onOpenPalette?: () => void;
  onNewSession?: () => void;
  main?: ReactNode;
}

const noop = () => {};
const MOBILE_QUERY = "(max-width: 639px)";

export function App({
  agents = [],
  sessions = [],
  selectedAgent = "",
  selectedSession = "",
  activeTab = "chat",
  healthStatus = "ok",
  onSelectAgent = noop,
  onSelectSession = noop,
  onTabChange = noop,
  onOpenPalette = noop,
  onNewSession = noop,
  main,
}: AppProps) {
  const isMobile = useMediaQuery(MOBILE_QUERY);
  const [userOpened, setUserOpened] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    try {
      return new URLSearchParams(globalThis.location.search).get("sidebar") === "collapsed";
    } catch {
      return false;
    }
  });
  const sidebarOpen = isMobile ? userOpened : true;

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        onOpenPalette();
      }
    };
    globalThis.addEventListener("keydown", handler);
    return () => globalThis.removeEventListener("keydown", handler);
  }, [onOpenPalette]);

  const toggleSidebarCollapse = () => {
    setSidebarCollapsed((prev) => {
      const next = !prev;
      try {
        const url = new URL(globalThis.location.href);
        if (next) {
          url.searchParams.set("sidebar", "collapsed");
        } else {
          url.searchParams.delete("sidebar");
        }
        globalThis.history.replaceState(null, "", url.toString());
      } catch {
        return next;
      }
      return next;
    });
  };

  const toggleSidebar = () => setUserOpened((open) => !open);
  const closeSidebar = () => setUserOpened(false);

  const showCollapsed = !isMobile && sidebarCollapsed;

  return (
    <div className={`app-shell ${showCollapsed ? "sidebar-collapsed" : ""}`}>
      <aside className={`sidebar ${sidebarOpen ? "open" : "closed"} ${showCollapsed ? "collapsed" : ""}`}>
        <Sidebar
          activeTab={activeTab}
          onTabChange={onTabChange}
          onOpenPalette={onOpenPalette}
          healthStatus={healthStatus}
          collapsed={!isMobile && sidebarCollapsed}
          onToggleCollapse={isMobile ? undefined : toggleSidebarCollapse}
          agents={
            <AgentsSection
              agents={agents}
              selectedAgent={selectedAgent}
              onSelectAgent={onSelectAgent}
            />
          }
          sessions={
            <SessionsSection
              sessions={sessions}
              selectedAgent={selectedAgent}
              selectedSession={selectedSession}
              onSelectSession={onSelectSession}
              onNewSession={onNewSession}
            />
          }
        />
      </aside>
      {isMobile && sidebarOpen && (
        <div
          className="sidebar-backdrop"
          onClick={closeSidebar}
          aria-hidden="true"
        />
      )}
      {isMobile && (
        <header className="topbar">
          <MobileBar
            activeTab={activeTab}
            onTabChange={onTabChange}
            onOpenPalette={onOpenPalette}
            onToggleSidebar={toggleSidebar}
            sidebarOpen={sidebarOpen}
            healthStatus={healthStatus}
          />
        </header>
      )}
      <main className="main">{main}</main>
    </div>
  );
}
