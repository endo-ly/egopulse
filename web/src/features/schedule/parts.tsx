import type { ReactNode } from "react";

import type { ScheduleRun } from "../../app/router";
import { MOON_ICON, PULSE_ICON } from "../../shared/ui/icons";
import type { StatusTone } from "../../shared/ui/StatusDot";

/** Pulse and Sleep are distinct mechanisms, each with its own glyph and color. */
export function LaneGlyph({ kind }: { kind: ScheduleRun["kind"] }) {
  return (
    <span className="lane-glyph" data-lane={kind} aria-hidden="true">
      {kind === "pulse" ? PULSE_ICON : MOON_ICON}
    </span>
  );
}

/** A run's outcome as a colored label. */
export function OutcomeLabel({ tone, children }: { tone: StatusTone; children: string }) {
  return (
    <span className="outcome" data-tone={tone}>
      {children}
    </span>
  );
}

/** Joins meta items with a separator, skipping empty ones. */
export function MetaLine({ items }: { items: (ReactNode | null | false)[] }) {
  const shown = items.filter((item): item is ReactNode => item !== null && item !== false);
  return (
    <p className="row-meta">
      {shown.flatMap((item, index) =>
        index === 0 ? [item] : [<span key={`sep-${index}`} className="row-meta-sep">·</span>, item],
      )}
    </p>
  );
}
