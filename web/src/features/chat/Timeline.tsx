import {
  useRef,
  useEffect,
  useLayoutEffect,
  useState,
  type ReactNode,
} from "react";

export interface TimelineProps {
  children?: ReactNode;
  /** Jump request: scroll to and flash the message at `index`; `seq` re-triggers repeat jumps. */
  jumpRequest?: { index: number; seq: number };
  /** Called when the user scrolls near the top of the loaded history. */
  onReachTop?: () => void;
  /** True once the loaded history reaches the first committed message. */
  atHistoryStart?: boolean;
  /** Increments after older messages are prepended. */
  prependSeq?: number;
}

const FOLLOW_THRESHOLD_RATIO = 0.1;
const TOP_TRIGGER_PX = 240;

export function Timeline({
  children,
  jumpRequest,
  onReachTop,
  atHistoryStart = false,
  prependSeq = 0,
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

  const onReachTopRef = useRef(onReachTop);
  onReachTopRef.current = onReachTop;

  // Initial mount: pin to the bottom.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    checkNearBottom();
  }, []);

  // Auto-follow while the user is near the bottom. The same observer fills
  // the viewport when a page is too short to produce a scrollbar (e.g. a
  // tool-heavy page whose rows are mostly hidden previews).
  const followingRef = useRef(true);
  followingRef.current = !showJumpButton;
  useEffect(() => {
    const el = scrollRef.current;
    const target = messagesRef.current;
    if (!el || !target || typeof ResizeObserver === "undefined") return;
    const fillIfShort = () => {
      if (el.clientHeight === 0) return;
      if (el.scrollHeight <= el.clientHeight + TOP_TRIGGER_PX) {
        onReachTopRef.current?.();
      }
    };
    const observer = new ResizeObserver(() => {
      if (followingRef.current) el.scrollTop = el.scrollHeight;
      fillIfShort();
    });
    observer.observe(target);
    observer.observe(el);
    fillIfShort();
    return () => observer.disconnect();
  }, []);

  // Fallback for environments without ResizeObserver.
  useEffect(() => {
    if (typeof ResizeObserver !== "undefined") return;
    const el = scrollRef.current;
    if (!el || el.clientHeight === 0) return;
    if (el.scrollHeight <= el.clientHeight + TOP_TRIGGER_PX) {
      onReachTop?.();
    }
  });

  // Remember the content height after every commit. The layout effect below
  // reads the previous value before this passive effect runs for a prepend.
  const previousScrollHeightRef = useRef(0);
  useEffect(() => {
    const el = scrollRef.current;
    if (el) previousScrollHeightRef.current = el.scrollHeight;
  });

  const lastPrependSeqRef = useRef(prependSeq);
  useLayoutEffect(() => {
    if (prependSeq === lastPrependSeqRef.current) return;
    lastPrependSeqRef.current = prependSeq;
    const el = scrollRef.current;
    if (!el) return;
    const delta = el.scrollHeight - previousScrollHeightRef.current;
    if (delta <= 0) return;
    // Keep the message the user was reading at the same viewport offset.
    const maxScroll = Math.max(0, el.scrollHeight - el.clientHeight);
    el.scrollTop = Math.min(el.scrollTop + delta, maxScroll);
    checkNearBottom();
  }, [prependSeq]);

  const handleScroll = () => {
    checkNearBottom();
    const el = scrollRef.current;
    if (!el) return;
    if (el.scrollTop <= TOP_TRIGGER_PX) onReachTop?.();
  };

  const jumpToLatest = () => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    setShowJumpButton(false);
  };

  // Scroll the requested message into view and flash it.
  useEffect(() => {
    if (!jumpRequest) return;
    const container = messagesRef.current;
    if (!container) return;
    const targetChild = container.children[jumpRequest.index] as
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
  }, [jumpRequest]);

  return (
    <div className="timeline" ref={scrollRef} onScroll={handleScroll}>
      {atHistoryStart && (
        <div className="timeline-history-start">Beginning of conversation</div>
      )}
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
