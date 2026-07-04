import { useState, useEffect, useRef, type KeyboardEvent } from "react";
import type { TabId } from "../navigation";
import type { AgentEntry, SessionEntry } from "../../shared/api/types";
import { buildPaletteItems, type PaletteItem } from "./buildPaletteItems";

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

  // Close on Escape from anywhere while open. A ref keeps the listener stable so
  // it is registered once per open instead of on every render.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  useEffect(() => {
    if (!open) return;
    const handler = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onCloseRef.current();
      }
    };
    globalThis.addEventListener("keydown", handler);
    return () => globalThis.removeEventListener("keydown", handler);
  }, [open]);

  if (!open) return null;

  const allItems = buildPaletteItems({
    agents,
    sessions,
    selectedAgent,
    actions: {
      close: onClose,
      navigate: onNavigate,
      selectAgent: onSelectAgent,
      selectSession: onSelectSession,
      newSession: onNewSession,
      refresh: onRefresh,
    },
  });

  const items = query
    ? allItems.filter(
        (item) =>
          item.label.toLowerCase().includes(query.toLowerCase()) ||
          item.section.toLowerCase().includes(query.toLowerCase()),
      )
    : allItems;

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((i) => Math.min(i + 1, Math.max(items.length - 1, 0)));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((i) => Math.max(i - 1, 0));
    } else if (
      e.key === "Enter" &&
      items[activeIndex] &&
      !items[activeIndex].disabled
    ) {
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
