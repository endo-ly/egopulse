import { useState } from "react";

import {
  PromptFileConflictError,
  fetchPromptFile,
  savePromptFile,
} from "../../shared/api/agents";
import type { PromptFileKind, PromptFileLayer, PromptFileScope } from "../../shared/api/types";
import { invalidateQuery, setQueryData } from "../../shared/hooks/useServerState";
import { confirmDiscardUnsaved } from "../../shared/lib/unsaved";
import { Button } from "../../shared/ui/Button";
import { QueryState } from "../../shared/ui/QueryState";
import { agentQueryKey, useAgentQuery } from "./agentQuery";
import { DocumentCard } from "./DocumentCard";
import { PromptFileEditor } from "./PromptFileEditor";

export interface PromptFileViewProps {
  agentId: string;
  authToken: string;
  kind: PromptFileKind;
}

/** Shows every file layer of one prompt file kind and whether it is applied. */
export function PromptFileView({ agentId, authToken, kind }: PromptFileViewProps) {
  // Secret-mode instructions are fetched only on explicit request so that
  // merely opening the screen never displays them.
  const [revealed, setRevealed] = useState(kind !== "secret");

  if (!revealed) {
    return (
      <div className="prompt-file-reveal">
        <p>Secret-mode instructions are hidden.</p>
        <Button variant="secondary" onClick={() => setRevealed(true)}>
          Reveal
        </Button>
      </div>
    );
  }
  return <PromptFileLayers agentId={agentId} authToken={authToken} kind={kind} />;
}

function PromptFileLayers({ agentId, authToken, kind }: PromptFileViewProps) {
  const layersState = useAgentQuery(authToken, ["file", agentId, kind], () =>
    fetchPromptFile(agentId, kind, authToken),
  );
  const [editing, setEditing] = useState<PromptFileScope | null>(null);
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const startEditing = (scope: PromptFileScope) => {
    setEditing(scope);
    setConflict(false);
    setError(null);
  };

  const cancelEditing = () => {
    setEditing(null);
    setConflict(false);
    setError(null);
  };

  const handleSave = async (layers: PromptFileLayer[], content: string) => {
    if (saving || editing === null) return;
    const layer = layers.find((candidate) => candidate.scope === editing);
    if (!layer) return;

    setSaving(true);
    setConflict(false);
    setError(null);
    try {
      const next = await savePromptFile(
        agentId,
        kind,
        editing,
        content,
        layer.hash,
        authToken,
      );
      setQueryData(agentQueryKey(authToken, ["file", agentId, kind]), next);
      // The sidebar summary (path, count, confidence) comes from the profile.
      invalidateQuery(agentQueryKey(authToken, ["profile", agentId]));
      setEditing(null);
    } catch (failure) {
      if (failure instanceof PromptFileConflictError) {
        setConflict(true);
      } else {
        setError(failure instanceof Error ? failure.message : String(failure));
      }
    } finally {
      setSaving(false);
    }
  };

  const handleReload = () => {
    if (!confirmDiscardUnsaved()) return;
    invalidateQuery(agentQueryKey(authToken, ["file", agentId, kind]));
    setEditing(null);
    setConflict(false);
    setError(null);
  };

  return (
    <QueryState state={layersState}>
      {(layers) => (
        <div className="doc-card-stack">
          {layers.map((layer) => {
            const isEditing = editing === layer.scope;
            return (
              <DocumentCard
                key={layer.path}
                eyebrow={layer.scope}
                path={layer.path}
                status={isEditing ? "Editing" : layerStatus(layer)}
                tone={layer.applied ? "applied" : "inactive"}
                content={isEditing ? null : layer.content}
                actions={
                  editing === null ? (
                    <button
                      type="button"
                      className="doc-card-action"
                      onClick={() => startEditing(layer.scope)}
                    >
                      Edit
                    </button>
                  ) : undefined
                }
              >
                {isEditing ? (
                  <PromptFileEditor
                    key={layer.path}
                    formId={`agent-file:${agentId}:${kind}:${layer.scope}`}
                    original={layer.content ?? ""}
                    scope={layer.scope}
                    saving={saving}
                    conflict={conflict}
                    error={error}
                    onSave={(content) => void handleSave(layers, content)}
                    onCancel={cancelEditing}
                    onReload={handleReload}
                  />
                ) : null}
              </DocumentCard>
            );
          })}
        </div>
      )}
    </QueryState>
  );
}

function layerStatus(layer: PromptFileLayer): string {
  if (layer.content !== null) return layer.applied ? "Applied" : "Not used";
  return layer.exists ? "Empty" : "Not created";
}
