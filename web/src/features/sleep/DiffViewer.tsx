import { useMemo, useState } from "react";

import { computeLineDiff } from "../../shared/lib/diff";
import { useMediaQuery } from "../../shared/hooks/useMediaQuery";

type DiffViewerProps = {
  before: string;
  after: string;
  fileName: string;
};

const MAX_VISIBLE_LINES = 500;

export function DiffViewer({ before, after, fileName }: DiffViewerProps) {
  const compactViewport = useMediaQuery("(max-width: 1023px)");
  const [modeOverride, setModeOverride] = useState<"split" | "unified" | null>(null);
  const mode = modeOverride ?? (compactViewport ? "unified" : "split");

  const lines = useMemo(() => computeLineDiff(before, after), [before, after]);
  const [showAll, setShowAll] = useState(false);
  const visibleLines = showAll ? lines : lines.slice(0, MAX_VISIBLE_LINES);

  if (before === after) {
    return <p className="diff-no-changes">No changes in {fileName}</p>;
  }

  return (
    <div className="diff-container">
      <div className="diff-toolbar" role="group" aria-label="Diff mode">
        <button
          type="button"
          className={mode === "split" ? "diff-mode-active" : "diff-mode-button"}
          onClick={() => setModeOverride("split")}
        >
          Split
        </button>
        <button
          type="button"
          className={mode === "unified" ? "diff-mode-active" : "diff-mode-button"}
          onClick={() => setModeOverride("unified")}
        >
          Unified
        </button>
      </div>

      {mode === "split" ? (
        <div className="diff-split">
          <div className="diff-column">
            <div className="diff-column-header">Before</div>
            {visibleLines.map((line, i) => (
              <DiffLineSplit key={i} line={line} side="before" />
            ))}
          </div>
          <div className="diff-column">
            <div className="diff-column-header">After</div>
            {visibleLines.map((line, i) => (
              <DiffLineSplit key={i} line={line} side="after" />
            ))}
          </div>
        </div>
      ) : (
        <div className="diff-unified">
          {visibleLines.map((line, i) => (
            <DiffLineUnified key={i} line={line} />
          ))}
        </div>
      )}

      {lines.length > MAX_VISIBLE_LINES && !showAll && (
        <button type="button" className="diff-show-all" onClick={() => setShowAll(true)}>
          Show all {lines.length} lines
        </button>
      )}
    </div>
  );
}

function DiffLineSplit({
  line,
  side,
}: {
  line: ReturnType<typeof computeLineDiff>[number];
  side: "before" | "after";
}) {
  if (line.type === "unchanged") {
    return (
      <div className="diff-line-unchanged">
        {side === "before" ? line.before : line.after}
      </div>
    );
  }

  const belongsToThisSide =
    (side === "before" && line.type === "remove") ||
    (side === "after" && line.type === "add");

  if (!belongsToThisSide) {
    return <div className="diff-line-placeholder" />;
  }

  return (
    <div className={`diff-line-${line.type}`}>
      {line.content}
    </div>
  );
}

function DiffLineUnified({
  line,
}: {
  line: ReturnType<typeof computeLineDiff>[number];
}) {
  if (line.type === "unchanged") {
    return (
      <div className="diff-line-unchanged">
        {" "}
        {line.before}
      </div>
    );
  }

  const prefix = line.type === "add" ? "+" : "-";
  return (
    <div className={`diff-line-${line.type}`}>
      {prefix} {line.content}
    </div>
  );
}
