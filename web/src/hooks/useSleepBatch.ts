import { useCallback, useEffect, useRef, useState } from "react";

import { fetchAgents, fetchRunDetail, fetchSleepRuns } from "../api";
import type { MemorySnapshot, SleepRun } from "../types";

type UseSleepBatchResult = {
  agents: string[];
  selectedAgent: string;
  setSelectedAgent: (agent: string) => void;
  runs: SleepRun[];
  loading: boolean;
  error: string | null;
  refreshRuns: () => void;
  selectedRun: SleepRun | null;
  selectedSnapshots: MemorySnapshot[];
  selectRun: (run: SleepRun) => void;
  backToList: () => void;
};

export function useSleepBatch(
  authTokenRef: React.MutableRefObject<string>,
): UseSleepBatchResult {
  const [agents, setAgents] = useState<string[]>([]);
  const [selectedAgent, setSelectedAgent] = useState("");
  const [runs, setRuns] = useState<SleepRun[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedRun, setSelectedRun] = useState<SleepRun | null>(null);
  const [selectedSnapshots, setSelectedSnapshots] = useState<MemorySnapshot[]>(
    [],
  );

  const refreshIdRef = useRef(0);
  const selectIdRef = useRef(0);

  useEffect(() => {
    let cancelled = false;

    async function loadAgents() {
      setLoading(true);
      setError(null);
      try {
        const agentList = await fetchAgents(authTokenRef.current);
        if (!cancelled) {
          setAgents(agentList);
        }
      } catch (err) {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "Failed to fetch agents",
          );
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void loadAgents();

    return () => {
      cancelled = true;
    };
  }, [authTokenRef]);

  const refreshRuns = useCallback(() => {
    const id = ++refreshIdRef.current;
    let cancelled = false;

    async function loadRuns() {
      setLoading(true);
      setError(null);
      try {
        const fetchedRuns = await fetchSleepRuns(
          authTokenRef.current,
          selectedAgent || undefined,
        );
        if (!cancelled && id === refreshIdRef.current) {
          setRuns(fetchedRuns);
        }
      } catch (err) {
        if (!cancelled && id === refreshIdRef.current) {
          setError(
            err instanceof Error ? err.message : "Failed to fetch runs",
          );
        }
      } finally {
        if (!cancelled && id === refreshIdRef.current) {
          setLoading(false);
        }
      }
    }

    void loadRuns();

    return () => {
      cancelled = true;
    };
  }, [authTokenRef, selectedAgent]);

  useEffect(() => {
    const cleanup = refreshRuns();
    return cleanup;
  }, [refreshRuns]);

  const selectRun = useCallback(
    (run: SleepRun) => {
      const id = ++selectIdRef.current;

      async function loadDetail() {
        setLoading(true);
        setError(null);
        try {
          const detail = await fetchRunDetail(authTokenRef.current, run.id);
          if (id === selectIdRef.current) {
            setSelectedRun(detail.run);
            setSelectedSnapshots(detail.snapshots);
          }
        } catch (err) {
          if (id === selectIdRef.current) {
            setError(
              err instanceof Error ? err.message : "Failed to fetch run detail",
            );
          }
        } finally {
          if (id === selectIdRef.current) {
            setLoading(false);
          }
        }
      }

      void loadDetail();
    },
    [authTokenRef],
  );

  const backToList = useCallback(() => {
    setSelectedRun(null);
    setSelectedSnapshots([]);
  }, []);

  return {
    agents,
    selectedAgent,
    setSelectedAgent,
    runs,
    loading,
    error,
    refreshRuns,
    selectedRun,
    selectedSnapshots,
    selectRun,
    backToList,
  };
}
