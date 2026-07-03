import { useState, useEffect, useRef, type KeyboardEvent } from "react";
import type { AgentEntry } from "./AgentsSection";
import type { SessionEntry } from "./SessionsSection";
import type { TabId } from "./TopBar";

export interface PaletteItem {
  id: string;
  label: string;
  section: string;
  description?: string;
  disabled?: boolean;
  onSelect: () => void;
}

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  agents: AgentEntry[];
  sessions: SessionEntry[];
  selectedAgent: string;
  onNavigate: (tab: TabId) => void;
  onSelectAgent: (id: string) => void;
  onSelectSession: (key: string) => void;
  onNewSession: () => void;
  onRefresh: () => void;
}

const STORAGE_KEY = "egopulse.paletteHistory";

function loadHistory(): PaletteItem[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as Array<{ id: string; label: string; section: string }>;
    return parsed.map((entry) => ({
      ...entry,
      onSelect: () => {},
    }));
  } catch {
    return [];
  }
}

function pushHistory(item: { id: string; label: string; section: string }) {
  try {
    const existing = loadHistory()
      .filter((h) => h.id !== item.id)
      .slice(0, 4);
    existing.unshift({ ...item, onSelect: () => {} });
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify(
        existing.map((h) => ({ id: h.id, label: h.label, section: h.section })),
      ),
    );
  } catch {
    return;
  }
}

const TAB_LABELS: Record<TabId, string> = {
  chat: "Chat",
  sleep: "Sleep",
  pulse: "Pulse",
  metrics: "Metrics",
  config: "Config",
};

const DISABLED_TABS: TabId[] = ["sleep", "pulse", "metrics", "config"];

export function CommandPalette({
  open,
  onClose,
  agents,
  sessions,
  selectedAgent,
  onNavigate,
  onSelectAgent,
  onSelectSession,
  onNewSession,
  onRefresh,
}: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setQuery("");
      setActiveIndex(0);
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveIndex((i) => Math.min(i + 1, Math.max(items.length - 1, 0)));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveIndex((i) => Math.max(i - 1, 0));
      }
    };
    globalThis.addEventListener("keydown", handler);
    return () => globalThis.removeEventListener("keydown", handler);
  });

  if (!open) return null;

  const quickActions: PaletteItem[] = [
    {
      id: "qa-new-session",
      label: "New Session",
      section: "Quick Actions",
      onSelect: () => {
        pushHistory({ id: "qa-new-session", label: "New Session", section: "Quick Actions" });
        onNewSession();
        onClose();
      },
    },
    {
      id: "qa-refresh",
      label: "Refresh current tab",
      section: "Quick Actions",
      onSelect: () => {
        pushHistory({ id: "qa-refresh", label: "Refresh current tab", section: "Quick Actions" });
        onRefresh();
        onClose();
      },
    },
  ];

  const navigation: PaletteItem[] = (
    Object.keys(TAB_LABELS) as TabId[]
  ).map((tab) => ({
    id: `nav-${tab}`,
    label: `Go to ${TAB_LABELS[tab]}`,
    section: "Navigation",
    disabled: DISABLED_TABS.includes(tab),
    onSelect: () => {
      pushHistory({ id: `nav-${tab}`, label: `Go to ${TAB_LABELS[tab]}`, section: "Navigation" });
      onNavigate(tab);
      onClose();
    },
  }));

  const agentItems: PaletteItem[] = agents
    .filter((a) => a.id !== selectedAgent)
    .map((a) => ({
      id: `agent-${a.id}`,
      label: a.label,
      section: "Agents",
      description: a.is_default ? "default" : undefined,
      onSelect: () => {
        pushHistory({ id: `agent-${a.id}`, label: a.label, section: "Agents" });
        onSelectAgent(a.id);
        onClose();
      },
    }));

  const sessionItems: PaletteItem[] = sessions
    .filter((s) => s.agent_id === selectedAgent)
    .slice(0, 5)
    .map((s) => ({
      id: `session-${s.session_key}`,
      label: s.label,
      section: "Sessions",
      description: s.channel,
      onSelect: () => {
        pushHistory({ id: `session-${s.session_key}`, label: s.label, section: "Sessions" });
        onSelectSession(s.session_key);
        onClose();
      },
    }));

  const recentItems = loadHistory().map((item) => {
    const original = [quickActions[0], quickActions[1], ...navigation, ...agentItems, ...sessionItems]
      .find((live) => live.id === item.id);
    return {
      ...item,
      section: "Recent",
      onSelect: original?.onSelect ?? (() => {}),
    };
  });

  const sleepPulseItems: PaletteItem[] = [];

  const allItems = [
    ...recentItems,
    ...quickActions,
    ...navigation,
    ...agentItems,
    ...sessionItems,
    ...sleepPulseItems,
  ];

  const items = query
    ? allItems.filter(
        (item) =>
          item.label.toLowerCase().includes(query.toLowerCase()) ||
          item.section.toLowerCase().includes(query.toLowerCase()),
      )
    : allItems;

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && items[activeIndex] && !items[activeIndex].disabled) {
      e.preventDefault();
      items[activeIndex].onSelect();
    }
  };

  const sections = items.reduce<Map<string, PaletteItem[]>>((acc, item) => {
    const list = acc.get(item.section) ?? [];
    list.push(item);
    acc.set(item.section, list);
    return acc;
  }, new Map());

  if (!sections.has("Sleep & Pulse Runs") && !query) {
    sections.set("Sleep & Pulse Runs", []);
  }

  let runningIndex = 0;

  return (
    <div
      className="palette-overlay"
      onClick={onClose}
    >
      <div
        className="palette-panel"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Command Palette"
      >
        <input
          ref={inputRef}
          className="palette-input"
          type="text"
          placeholder="Search or jump…"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActiveIndex(0);
          }}
          onKeyDown={handleKeyDown}
        />
        <div className="palette-list">
          {Array.from(sections.entries()).map(([sectionName, sectionItems]) => (
            <div key={sectionName} className="palette-section">
              <div className="palette-section-title">{sectionName}</div>
              {sectionItems.map((item) => {
                const idx = runningIndex++;
                return (
                  <button
                    key={item.id}
                    type="button"
                    className={`palette-item ${idx === activeIndex ? "active" : ""}`}
                    disabled={item.disabled}
                    onClick={item.onSelect}
                    onMouseEnter={() => setActiveIndex(idx)}
                  >
                    <span className="palette-item-label">{item.label}</span>
                    {item.description && (
                      <span className="palette-item-desc">{item.description}</span>
                    )}
                  </button>
                );
              })}
            </div>
          ))}
          {items.length === 0 && (
            <div className="palette-empty">No results</div>
          )}
        </div>
      </div>
    </div>
  );
}
