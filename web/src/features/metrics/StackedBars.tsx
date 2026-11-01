import { useState } from "react";

import { bucketLabel, bucketTitle, formatCount } from "./metricsFormat";

export interface StackSeries {
  key: string;
  label: string;
  color: string;
}

export interface StackedBarsProps {
  buckets: { start: string; values: Record<string, number> }[];
  series: StackSeries[];
  bucket: "hour" | "day";
  timeZone: string;
  /** Accessible name of the chart. */
  label: string;
}

const HEIGHT = 180;
const AXIS_GAP = 22;
const SEGMENT_GAP = 2;
/** Width of the tick label column left of the plot; matches the CSS padding. */
const TICK_GUTTER = 40;

/** Gridline values: up to four even steps, the last one reaching `max`. */
function niceScale(max: number): number[] {
  if (max <= 0) return [0];
  const magnitude = 10 ** Math.floor(Math.log10(max));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * magnitude).find((s) => s * 4 >= max) ?? magnitude * 10;
  const ticks: number[] = [];
  for (let value = step; ticks.length < 4; value += step) {
    ticks.push(value);
    if (value >= max) break;
  }
  return ticks;
}

/**
 * Vertical stacked bars over time. Every bucket is hoverable (the hit area
 * spans the full column) and shows its per-series breakdown.
 */
export function StackedBars({ buckets, series, bucket, timeZone, label }: StackedBarsProps) {
  const [hovered, setHovered] = useState<number | null>(null);
  const totals = buckets.map((b) => series.reduce((sum, s) => sum + (b.values[s.key] ?? 0), 0));
  const ticks = niceScale(Math.max(0, ...totals));
  const top = ticks[ticks.length - 1] || 1;
  const columns = buckets.length || 1;
  const labelEvery = Math.ceil(columns / 8);
  const plotHeight = HEIGHT - AXIS_GAP;
  const scale = (value: number) => (value / top) * plotHeight;
  const active = hovered === null ? null : buckets[hovered];

  return (
    <div className="stacked-bars" onMouseLeave={() => setHovered(null)}>
      <svg
        className="stacked-bars-svg"
        viewBox={`0 0 ${columns * 10} ${HEIGHT}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={label}
      >
        {ticks.map((tick) => (
          <line
            key={tick}
            className="chart-grid"
            x1={0}
            x2={columns * 10}
            y1={plotHeight - scale(tick)}
            y2={plotHeight - scale(tick)}
            vectorEffect="non-scaling-stroke"
          />
        ))}
        {buckets.map((b, index) => {
          let y = plotHeight;
          const visible = series.filter((s) => (b.values[s.key] ?? 0) > 0);
          return (
            <g key={b.start} opacity={hovered === null || hovered === index ? 1 : 0.45}>
              {visible.map((s, segment) => {
                const h = scale(b.values[s.key] ?? 0);
                // A surface gap separates stacked segments; the plot is drawn
                // at 1 unit per CSS pixel vertically.
                const gap = segment > 0 && h > SEGMENT_GAP * 2 ? SEGMENT_GAP : 0;
                y -= h;
                return (
                  <rect
                    key={s.key}
                    x={index * 10 + 1.5}
                    y={y}
                    width={7}
                    height={Math.max(0, h - gap)}
                    rx={segment === visible.length - 1 ? 1 : 0}
                    fill={s.color}
                  />
                );
              })}
              <rect
                className="chart-hit"
                x={index * 10}
                y={0}
                width={10}
                height={plotHeight}
                onMouseEnter={() => setHovered(index)}
                onClick={() => setHovered(index)}
              />
            </g>
          );
        })}
      </svg>
      <div className="stacked-bars-ticks" aria-hidden="true">
        {ticks.map((tick) => (
          <span key={tick} style={{ bottom: `${((AXIS_GAP + scale(tick)) / HEIGHT) * 100}%` }}>
            {formatCount(tick)}
          </span>
        ))}
      </div>
      <div className="stacked-bars-axis" aria-hidden="true">
        {buckets.map((b, index) => (
          <span key={b.start}>{index % labelEvery === 0 ? bucketLabel(b.start, bucket, timeZone) : ""}</span>
        ))}
      </div>
      {active && hovered !== null && (
        <div
          className="chart-tooltip"
          role="status"
          style={{
            // The plot starts after the tick gutter (the container's left padding).
            left: `calc(${TICK_GUTTER}px + (100% - ${TICK_GUTTER}px) * ${(hovered + 0.5) / columns})`,
            transform: `translateX(${hovered / columns > 0.6 ? "-100%" : hovered / columns < 0.25 ? "0" : "-50%"})`,
          }}
        >
          <div className="chart-tooltip-title">{bucketTitle(active.start, bucket, timeZone)}</div>
          <div className="chart-tooltip-total">{formatCount(totals[hovered])} tokens</div>
          {series
            .filter((s) => (active.values[s.key] ?? 0) > 0)
            .map((s) => (
              <div key={s.key} className="chart-tooltip-row">
                <span className="legend-swatch" style={{ background: s.color }} />
                <span className="chart-tooltip-name">{s.label}</span>
                <span className="chart-tooltip-value">{formatCount(active.values[s.key] ?? 0)}</span>
              </div>
            ))}
        </div>
      )}
    </div>
  );
}
