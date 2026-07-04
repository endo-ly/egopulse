import { useEffect, useRef, useState } from "react";

export type ToastTone = "info" | "success" | "error" | "warning";

export interface ToastProps {
  tone: ToastTone;
  message: string;
  onClose: () => void;
}

const DURATION_MS: Record<ToastTone, number> = {
  info: 4000,
  success: 4000,
  warning: 6000,
  error: 8000,
};

export function Toast({ tone, message, onClose }: ToastProps) {
  const duration = DURATION_MS[tone];
  const [progress, setProgress] = useState(100);
  const pausedRef = useRef(false);
  const rafRef = useRef<number>(0);
  const startRef = useRef<number>(0);
  const elapsedRef = useRef(0);

  useEffect(() => {
    const tick = (now: number) => {
      if (!pausedRef.current) {
        const elapsed = elapsedRef.current + (now - startRef.current);
        const remaining = Math.max(0, duration - elapsed);
        setProgress((remaining / duration) * 100);
        if (remaining <= 0) {
          onClose();
          return;
        }
      }
      rafRef.current = requestAnimationFrame(tick);
    };

    startRef.current = performance.now();
    rafRef.current = requestAnimationFrame(tick);

    return () => cancelAnimationFrame(rafRef.current);
  }, [duration, onClose]);

  const handleMouseEnter = () => {
    if (!pausedRef.current) {
      elapsedRef.current += performance.now() - startRef.current;
      pausedRef.current = true;
    }
  };

  const handleMouseLeave = () => {
    if (pausedRef.current) {
      startRef.current = performance.now();
      pausedRef.current = false;
    }
  };

  const role = tone === "error" || tone === "warning" ? "alert" : "status";

  return (
    <div
      className={`toast toast-${tone}`}
      role={role}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      <span className="toast-message">{message}</span>
      <button className="toast-close" onClick={onClose} aria-label="Close notification">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
      <div className="toast-progress">
        <div className="toast-progress-bar" style={{ width: `${progress}%` }} />
      </div>
    </div>
  );
}
