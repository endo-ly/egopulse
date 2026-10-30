import { useState } from "react";

import { fetchPromptFile } from "../../shared/api/agents";
import type { PromptFileKind, PromptFileLayer } from "../../shared/api/types";
import { Button } from "../../shared/ui/Button";
import { useAgentQuery } from "./agentQuery";
import { DocumentCard } from "./DocumentCard";
import { QueryState } from "./QueryState";

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

  return (
    <QueryState state={layersState}>
      {(layers) => (
        <div className="doc-card-stack">
          {layers.map((layer) => (
            <DocumentCard
              key={layer.path}
              eyebrow={layer.scope}
              path={layer.path}
              status={layerStatus(layer)}
              tone={layer.applied ? "applied" : "inactive"}
              content={layer.content}
            />
          ))}
        </div>
      )}
    </QueryState>
  );
}

function layerStatus(layer: PromptFileLayer): string {
  if (layer.applied) return "Applied";
  return layer.content === null ? "Not created" : "Not used";
}
