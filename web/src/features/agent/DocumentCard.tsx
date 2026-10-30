import { useState } from "react";

import { MarkdownRenderer } from "../../shared/ui/MarkdownRenderer";

type DocumentTone = "applied" | "inactive";

/** Line count above which a collapsible card starts folded. */
const FOLD_LINES = 16;

export interface DocumentCardProps {
  /** Small label before the path, e.g. the file's scope. */
  eyebrow?: string;
  path: string;
  /** Short status shown on the right of the header. */
  status?: string;
  tone?: DocumentTone;
  /** Null renders the header only (missing file). */
  content: string | null;
  /** Markdown files offer a rendered view; others are always raw text. */
  markdown?: boolean;
  /** Long content starts folded with a control to show all of it. */
  collapsible?: boolean;
}

/** A file shown as a card: path header, then rendered Markdown or raw text. */
export function DocumentCard({
  eyebrow,
  path,
  status,
  tone = "applied",
  content,
  markdown = true,
  collapsible = false,
}: DocumentCardProps) {
  const [raw, setRaw] = useState(false);
  const foldable = collapsible && content !== null && content.split("\n").length > FOLD_LINES;
  const [expanded, setExpanded] = useState(false);
  const folded = foldable && !expanded;

  return (
    <article className="doc-card" data-tone={tone}>
      <header className="doc-card-header">
        {eyebrow && <span className="doc-card-eyebrow">{eyebrow}</span>}
        <code className="doc-card-path">{path}</code>
        {status && <span className="doc-card-status">{status}</span>}
        {content && markdown && (
          <button
            type="button"
            className="doc-card-action"
            aria-pressed={raw}
            onClick={() => setRaw((value) => !value)}
          >
            Raw
          </button>
        )}
      </header>
      {content === "" && <p className="doc-card-empty">This file is empty.</p>}
      {content && (
        <div className={`doc-card-content ${folded ? "folded" : ""}`}>
          {markdown && !raw ? (
            <MarkdownRenderer content={content} />
          ) : (
            <pre className="doc-card-raw">{content}</pre>
          )}
        </div>
      )}
      {foldable && (
        <button
          type="button"
          className="doc-card-fold"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded ? "Show less" : "Show all"}
        </button>
      )}
    </article>
  );
}
