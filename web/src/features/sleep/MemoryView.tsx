import { useState } from "react";

import { fetchAgentMemory } from "../../shared/api/sleep";
import { useServerState } from "../../shared/hooks/useServerState";
import { EmptyState } from "../../shared/ui/EmptyState";
import { Spinner } from "../../shared/ui/Spinner";

type MemoryViewProps = {
  agentId: string;
  authToken: string;
};

const MEMORY_FILES = ["episodic", "semantic", "prospective"] as const;

const EMPTY_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

/** Shows the agent's current published long-term memory files. */
export function MemoryView({ agentId, authToken }: MemoryViewProps) {
  const [selectedFile, setSelectedFile] = useState<(typeof MEMORY_FILES)[number]>("episodic");
  const memoryState = useServerState(
    `sleep-memory:${agentId}`,
    () => fetchAgentMemory(agentId, authToken),
  );

  if (memoryState.loading && memoryState.data === undefined) {
    return (
      <div className="memory-view-loading">
        <Spinner />
      </div>
    );
  }

  if (memoryState.error || memoryState.data === undefined) {
    return (
      <div className="run-error" role="alert">
        {memoryState.error?.message ?? "Memory could not be loaded"}
      </div>
    );
  }

  const memory = memoryState.data;
  if (Object.values(memory).every((content) => content === "")) {
    return (
      <EmptyState
        icon={EMPTY_ICON}
        title="No memory yet"
        description="Long-term memory appears here once the first sleep batch has distilled conversations."
      />
    );
  }

  const content = memory[selectedFile] ?? "";

  return (
    <div className="memory-view">
      <div className="tab-switch" role="tablist" aria-label="Memory files">
        {MEMORY_FILES.map((file) => (
          <button
            key={file}
            type="button"
            role="tab"
            aria-selected={file === selectedFile}
            className="tab-switch-item"
            onClick={() => setSelectedFile(file)}
          >
            {file}
          </button>
        ))}
      </div>
      {content === "" ? (
        <p className="memory-empty-file">The {selectedFile} memory file is empty.</p>
      ) : (
        <pre className="memory-content">{content}</pre>
      )}
    </div>
  );
}
