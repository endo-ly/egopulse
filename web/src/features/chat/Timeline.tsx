import { useRef, useEffect, useState, type ReactNode } from "react";

export interface TimelineProps {
  children?: ReactNode;
  /** Message indices of the current search matches (drives match highlight). */
  searchMatches?: number[];
  /** Index within searchMatches of the active match. */
  activeMatchIndex?: number;
}

const FOLLOW_THRESHOLD_RATIO = 0.1;

export function Timeline({
  children,
  searchMatches,
  activeMatchIndex = 0,
}: TimelineProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const messagesRef = useRef<HTMLDivElement>(null);
  const [showJumpButton, setShowJumpButton] = useState(false);

  const checkNearBottom = () => {
    const el = scrollRef.current;
    if (!el) return;
    const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    const threshold = el.clientHeight * FOLLOW_THRESHOLD_RATIO;
    setShowJumpButton(distFromBottom > threshold);
  };

  // Initial mount: pin to the bottom.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    checkNearBottom();
  }, []);

  // Auto-follow while the user is near the bottom. A ResizeObserver keeps the
  // view pinned as messages stream in or arrive, and stays out of the way once
  // the user scrolls up to read history (followingRef becomes false).
  const followingRef = useRef(true);
  followingRef.current = !showJumpButton;
  useEffect(() => {
    const el = scrollRef.current;
    const target = messagesRef.current;
    if (!el || !target || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (followingRef.current) el.scrollTop = el.scrollHeight;
    });
    observer.observe(target);
    return () => observer.disconnect();
  }, []);

  const handleScroll = () => checkNearBottom();

  const jumpToLatest = () => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    setShowJumpButton(false);
  };

  // Scroll the active search match into view and flash it.
  const messageIndex = searchMatches?.[activeMatchIndex];
  useEffect(() => {
    if (messageIndex == null) return;
    const container = messagesRef.current;
    if (!container) return;
    const targetChild = container.children[messageIndex] as
      | HTMLElement
      | undefined;
    if (!targetChild) return;
    if (typeof targetChild.scrollIntoView === "function") {
      targetChild.scrollIntoView({ behavior: "smooth", block: "center" });
    }
    targetChild.classList.add("search-highlight");
    const timeout = setTimeout(
      () => targetChild.classList.remove("search-highlight"),
      1500,
    );
    return () => clearTimeout(timeout);
  }, [messageIndex, activeMatchIndex]);

  return (
    <div className="timeline" ref={scrollRef} onScroll={handleScroll}>
      <div className="timeline-messages" ref={messagesRef}>
        {children}
      </div>
      {showJumpButton && (
        <button type="button" className="jump-to-latest" onClick={jumpToLatest}>
          Jump to latest
        </button>
      )}
    </div>
  );
}
