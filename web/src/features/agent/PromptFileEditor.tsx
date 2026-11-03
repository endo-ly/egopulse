import { useEffect, useMemo, useState } from "react";

import type { PromptFileScope } from "../../shared/api/types";
import { countLineChanges } from "../../shared/lib/diff";
import { setUnsavedChanges } from "../../shared/lib/unsaved";
import { Button } from "../../shared/ui/Button";
import { DiffViewer } from "../../shared/ui/DiffViewer";
import { MarkdownRenderer } from "../../shared/ui/MarkdownRenderer";

export interface PromptFileEditorProps {
  /** Unique per layer, for the unsaved-changes guard. */
  formId: string;
  /** Trimmed content shown when editing started. */
  original: string;
  scope: PromptFileScope;
  saving: boolean;
  conflict: boolean;
  /** A non-conflict failure, e.g. a network error. */
  error: string | null;
  onSave: (content: string) => void;
  onCancel: () => void;
  onReload: () => void;
}

/** Inline Markdown editor with live preview and a before-save diff. */
export function PromptFileEditor({
  formId,
  original,
  scope,
  saving,
  conflict,
  error,
  onSave,
  onCancel,
  onReload,
}: PromptFileEditorProps) {
  const [draft, setDraft] = useState(original);
  const [mode, setMode] = useState<"write" | "preview">("write");
  const [diffOpen, setDiffOpen] = useState(true);
  const dirty = draft !== original;
  const counts = useMemo(() => countLineChanges(original, draft), [original, draft]);

  useEffect(() => {
    setUnsavedChanges(formId, dirty);
    return () => setUnsavedChanges(formId, false);
  }, [formId, dirty]);

  return (
    <div className="doc-card-editor">
      {scope === "global" && (
        <p className="doc-card-editor-note">
          This is a shared global file. Saving it can affect other agents.
        </p>
      )}

      <div className="doc-card-editor-tabs" role="group" aria-label="Editor mode">
        <button
          type="button"
          className="doc-card-action"
          aria-pressed={mode === "write"}
          onClick={() => setMode("write")}
        >
          Write
        </button>
        <button
          type="button"
          className="doc-card-action"
          aria-pressed={mode === "preview"}
          onClick={() => setMode("preview")}
        >
          Preview
        </button>
      </div>

      {mode === "write" ? (
        <textarea
          className="doc-card-editor-textarea"
          aria-label="Content"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          spellCheck={false}
        />
      ) : (
        <div className="doc-card-content doc-card-editor-preview">
          {draft.trim() === "" ? (
            <p className="doc-card-empty">Nothing to preview.</p>
          ) : (
            <MarkdownRenderer content={draft} />
          )}
        </div>
      )}

      {dirty && (
        <details
          className="doc-card-editor-diff"
          open={diffOpen}
          onToggle={(event) => setDiffOpen(event.currentTarget.open)}
        >
          <summary>
            Changes{" "}
            <span className="tab-switch-count">
              <span className="count-add">+{counts.add}</span>{" "}
              <span className="count-remove">−{counts.remove}</span>
            </span>
          </summary>
          <DiffViewer before={original} after={draft} fileName="this file" />
        </details>
      )}

      {conflict && (
        <p className="doc-card-editor-error" role="alert">
          This file changed elsewhere. Reload the latest content before saving again.
        </p>
      )}
      {error && (
        <p className="doc-card-editor-error" role="alert">
          {error}
        </p>
      )}

      <div className="doc-card-editor-actions">
        {conflict && (
          <Button variant="secondary" onClick={onReload}>
            Reload
          </Button>
        )}
        <Button variant="secondary" onClick={onCancel} disabled={saving}>
          Cancel
        </Button>
        <Button
          variant="primary"
          busy={saving}
          disabled={!dirty || conflict}
          onClick={() => onSave(draft)}
        >
          Save
        </Button>
      </div>
    </div>
  );
}
