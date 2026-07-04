import { useMemo, useState } from "react";

import { computeLineDiff } from "../../shared/lib/diff";

type DiffViewerProps = {
  before: string;
  after: string;
  fileName: string;
};

export function DiffViewer({ before, after, fileName }: DiffViewerProps) {
  const [mode, setMode] = useState<"split" | "unified">(() =>
    typeof window !== "undefined" && window.innerWidth < 768 ? "unified" : "split",
  );

  const lines = useMemo(() => computeLineDiff(before, after), [before, after]);

  if (before === after) {
    return <p className="diff-no-changes">No changes in {fileName}</p>;
  }

  return (
    <div className="diff-container">
      <div className="diff-toolbar">
        <button
          type="button"
          className={mode === "split" ? "diff-mode-active" : "diff-mode-button"}
          onClick={() => setMode("split")}
        >
          Split
        </button>
        <button
          type="button"
          className={mode === "unified" ? "diff-mode-active" : "diff-mode-button"}
          onClick={() => setMode("unified")}
        >
          Unified
        </button>
      </div>

      {mode === "split" ? (
        <div className="diff-split">
          <div className="diff-column">
            <div className="diff-column-header">Before</div>
            {lines.map((line, i) => (
              <DiffLineSplit key={i} line={line} side="before" />
            ))}
          </div>
          <div className="diff-column">
            <div className="diff-column-header">After</div>
            {lines.map((line, i) => (
              <DiffLineSplit key={i} line={line} side="after" />
            ))}
          </div>
        </div>
      ) : (
        <div className="diff-unified">
          {lines.map((line, i) => (
            <DiffLineUnified key={i} line={line} />
          ))}
        </div>
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
