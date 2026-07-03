import { useSleepBatch } from "../hooks/useSleepBatch";
import { RunList } from "./RunList";
import { RunDetail } from "./RunDetail";

export function SleepBatchPanel() {
  const {
    agents,
    selectedAgent,
    setSelectedAgent,
    runs,
    loading,
    error,
    selectedRun,
    selectedSnapshots,
    selectRun,
    backToList,
    refreshRuns,
  } = useSleepBatch();

  return (
    <div className="sleep-batch-panel">
      <header className="sleep-batch-header">
        <h2 className="m-0">Sleep Batch Audit</h2>
        <button
          type="button"
          className="secondary-button"
          onClick={refreshRuns}
          disabled={loading}
        >
          ↻ Refresh
        </button>
      </header>

      {error && <div className="run-error">{error}</div>}

      {selectedRun ? (
        <RunDetail
          run={selectedRun}
          snapshots={selectedSnapshots}
          onBack={backToList}
        />
      ) : (
        <RunList
          runs={runs}
          agents={agents}
          selectedAgent={selectedAgent}
          onSelectAgent={setSelectedAgent}
          onSelectRun={selectRun}
        />
      )}
    </div>
  );
}
