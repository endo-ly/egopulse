export type StatusTone = "live" | "idle" | "error" | "success" | "warning";

export interface StatusDotProps {
  tone: StatusTone;
  className?: string;
}

export function StatusDot({ tone, className }: StatusDotProps) {
  return (
    <span className={className ? `dot-${tone} ${className}` : `dot-${tone}`} />
  );
}
