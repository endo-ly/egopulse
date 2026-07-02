import { useRef, useEffect, useState, type ReactNode } from "react";

export interface TimelineProps {
  children?: ReactNode;
}

const FOLLOW_THRESHOLD_RATIO = 0.1;

export function Timeline({ children }: TimelineProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [showJumpButton, setShowJumpButton] = useState(false);

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
    checkNearBottom();
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (!showJumpButton) {
      el.scrollTop = el.scrollHeight;
    }
  });

  const handleScroll = () => checkNearBottom();

  const jumpToLatest = () => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    setShowJumpButton(false);
  };

  return (
    <div
      className="timeline"
      ref={scrollRef}
      onScroll={handleScroll}
    >
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
