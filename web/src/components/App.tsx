import { useState, type ReactNode } from "react";
import { useMediaQuery } from "../hooks/useMediaQuery";

export interface AppShellProps {
  sidebar?: ReactNode;
  topbar?: ReactNode;
  main?: ReactNode;
}

const MOBILE_QUERY = "(max-width: 639px)";

export function App({ sidebar, topbar, main }: AppShellProps) {
  const isMobile = useMediaQuery(MOBILE_QUERY);
  const [userOpened, setUserOpened] = useState(false);
  const sidebarOpen = isMobile ? userOpened : true;

  const toggleSidebar = () => setUserOpened((open) => !open);
  const closeSidebar = () => setUserOpened(false);

  return (
    <div className="app-shell">
      <aside className={`sidebar ${sidebarOpen ? "open" : "closed"}`}>{sidebar}</aside>
      {isMobile && sidebarOpen && (
        <div
          className="sidebar-backdrop"
          onClick={closeSidebar}
          aria-hidden="true"
        />
      )}
      <header className="topbar">
        {isMobile && (
          <button
            type="button"
            className="hamburger-btn"
            aria-label="Toggle sidebar"
            aria-expanded={sidebarOpen}
            onClick={toggleSidebar}
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
        )}
        {topbar}
      </header>
      <main className="main">{main}</main>
    </div>
  );
}
