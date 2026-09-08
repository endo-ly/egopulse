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
  authToken?: string;
  /** Called after an agent avatar upload/removal so lists can refresh. */
  onAvatarChanged?: () => void;
}

const noop = () => {};
const MOBILE_QUERY = "(max-width: 639px)";
const SWIPE_MIN_PX = 56;

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
  authToken,
  onAvatarChanged,
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
      } else if (e.key === "Escape" && isMobile) {
        setUserOpened(false);
      }
    };
    globalThis.addEventListener("keydown", handler);
    return () => globalThis.removeEventListener("keydown", handler);
  }, [onOpenPalette, isMobile]);

  useEffect(() => {
    if (!isMobile) return;
    let startX = 0;
    let startY = 0;
    const onTouchStart = (event: TouchEvent) => {
      const touch = event.touches[0];
      startX = touch.clientX;
      startY = touch.clientY;
    };
    const onTouchEnd = (event: TouchEvent) => {
      const touch = event.changedTouches[0];
      const dx = touch.clientX - startX;
      const dy = touch.clientY - startY;
      if (Math.abs(dx) < SWIPE_MIN_PX || Math.abs(dx) < Math.abs(dy)) return;
      setUserOpened(dx > 0);
    };
    document.addEventListener("touchstart", onTouchStart, { passive: true });
    document.addEventListener("touchend", onTouchEnd, { passive: true });
    return () => {
      document.removeEventListener("touchstart", onTouchStart);
      document.removeEventListener("touchend", onTouchEnd);
    };
  }, [isMobile]);

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

  // Mobile overlay dismisses on navigation (layout.md §4.5).
  const dismissOverlayOnMobile = () => {
    if (isMobile) setUserOpened(false);
  };
  const handleTabChange = (tab: TabId) => {
    onTabChange(tab);
    dismissOverlayOnMobile();
  };
  const handleSelectAgent = (id: string) => {
    onSelectAgent(id);
    dismissOverlayOnMobile();
  };
  const handleSelectSession = (key: string) => {
    onSelectSession(key);
    dismissOverlayOnMobile();
  };
  const handleNewSession = () => {
    onNewSession();
    dismissOverlayOnMobile();
  };

  const showCollapsed = !isMobile && sidebarCollapsed;

  return (
    <div className={`app-shell ${showCollapsed ? "sidebar-collapsed" : ""}`}>
      <aside className={`sidebar ${sidebarOpen ? "open" : "closed"} ${showCollapsed ? "collapsed" : ""}`}>
        <Sidebar
          activeTab={activeTab}
          onTabChange={handleTabChange}
          onOpenPalette={onOpenPalette}
          healthStatus={healthStatus}
          collapsed={!isMobile && sidebarCollapsed}
          onToggleCollapse={isMobile ? undefined : toggleSidebarCollapse}
          agents={
            <AgentsSection
              agents={agents}
              selectedAgent={selectedAgent}
              onSelectAgent={handleSelectAgent}
              authToken={authToken}
              onAvatarChanged={onAvatarChanged}
            />
          }
          sessions={
            <SessionsSection
              sessions={sessions}
              selectedAgent={selectedAgent}
              selectedSession={selectedSession}
              onSelectSession={handleSelectSession}
              onNewSession={handleNewSession}
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
