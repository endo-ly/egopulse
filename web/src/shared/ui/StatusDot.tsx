export type StatusTone = "live" | "idle" | "error" | "success" | "warning";

export interface StatusDotProps {
  tone: StatusTone;
}

export function StatusDot({ tone }: StatusDotProps) {
  return <span className={`dot-${tone}`} />;
}
