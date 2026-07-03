export type BadgeKind = "channel" | "status" | "trigger";

export interface BadgeProps {
  kind: BadgeKind;
  children: string;
}

export function Badge({ kind, children }: BadgeProps) {
  return <span className={`badge-${kind}`}>{children}</span>;
}
