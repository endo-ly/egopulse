import { useRef, useEffect, useState, type ReactNode, type KeyboardEvent } from "react";

export interface TimelineProps {
  children?: ReactNode;
  searchTarget?: string;
}

const FOLLOW_THRESHOLD_RATIO = 0.1;

export function Timeline({ children, searchTarget }: TimelineProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [showJumpButton, setShowJumpButton] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [matchIndex, setMatchIndex] = useState(0);

  const checkNearBottom = () => {
    const el = scrollRef.current;
    if (!el) return;
    const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    const threshold = el.clientHeight * FOLLOW_THRESHOLD_RATIO;
    setShowJumpButton(distFromBottom > threshold);
  };

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    checkNearBottom();
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (!showJumpButton) {
      el.scrollTop = el.scrollHeight;
    }
  });

  useEffect(() => {
    const handler = (e: globalThis.KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        setSearchOpen(true);
      }
    };
    globalThis.addEventListener("keydown", handler);
    return () => globalThis.removeEventListener("keydown", handler);
  }, []);

  const handleScroll = () => checkNearBottom();

  const jumpToLatest = () => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    setShowJumpButton(false);
  };

  const matches: number[] = [];
  if (searchQuery && searchTarget) {
    const lower = searchTarget.toLowerCase();
    const q = searchQuery.toLowerCase();
    let idx = lower.indexOf(q);
    while (idx !== -1) {
      matches.push(idx);
      idx = lower.indexOf(q, idx + 1);
    }
  }

  const handleSearchKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) {
        setMatchIndex((i) => (i <= 0 ? matches.length - 1 : i - 1));
      } else {
        setMatchIndex((i) => (i + 1) % Math.max(matches.length, 1));
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      setSearchOpen(false);
      setSearchQuery("");
    }
  };

  useEffect(() => {
    if (!searchQuery || matches.length === 0 || !searchOpen) return;
    const el = scrollRef.current;
    if (!el || !searchTarget) return;
    const matchPos = matches[matchIndex];
    if (matchPos == null) return;
    const lower = searchTarget.toLowerCase();
    const beforeMatch = lower.substring(0, matchPos);
    const lineNum = beforeMatch.split("\n").length;
    const childArray = el.querySelectorAll(":scope > *");
    const targetChild = childArray[lineNum] as HTMLElement | undefined;
    if (targetChild && typeof targetChild.scrollIntoView === "function") {
      targetChild.scrollIntoView({ behavior: "smooth", block: "center" });
      targetChild.classList.add("search-highlight");
      const timeout = setTimeout(() => targetChild.classList.remove("search-highlight"), 1500);
      return () => clearTimeout(timeout);
    }
  }, [matchIndex, searchQuery, matches, searchTarget, searchOpen]);

  return (
    <div
      className="timeline"
      ref={scrollRef}
      onScroll={handleScroll}
    >
      {searchOpen && (
        <div className="timeline-search-bar">
          <input
            type="text"
            className="timeline-search-input"
            placeholder="Search messages…"
            value={searchQuery}
            onChange={(e) => {
              setSearchQuery(e.target.value);
              setMatchIndex(0);
            }}
            onKeyDown={handleSearchKey}
            autoFocus
          />
          {searchQuery && (
            <span className="timeline-search-count">
              {matches.length > 0 ? `${matchIndex + 1} / ${matches.length}` : "0 / 0"}
            </span>
          )}
          <button
            type="button"
            className="timeline-search-close"
            onClick={() => {
              setSearchOpen(false);
              setSearchQuery("");
            }}
          >
            ✕
          </button>
        </div>
      )}
      {children}
      {showJumpButton && (
        <button
          type="button"
          className="jump-to-latest"
          onClick={jumpToLatest}
        >
          Jump to latest
        </button>
      )}
    </div>
  );
}
