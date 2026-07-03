export type StatusTone = "live" | "idle" | "error";

export interface StatusDotProps {
  tone: StatusTone;
}

export function StatusDot({ tone }: StatusDotProps) {
  return <span className={`dot-${tone}`} />;
}
