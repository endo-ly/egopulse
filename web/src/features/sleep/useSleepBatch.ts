import { useCallback, useEffect, useRef, useState } from "react";
import { fetchSleepAgents, fetchSleepRunDetail, fetchSleepRuns } from "../../shared/api/sleep";
import type { MemorySnapshot, SleepRun } from "../../shared/api/types";

export function useSleepBatch(authToken = "") {
  const [agents, setAgents] = useState<string[]>([]);
  const [selectedAgent, setSelectedAgent] = useState("");
  const [runs, setRuns] = useState<SleepRun[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedRun, setSelectedRun] = useState<SleepRun | null>(null);
  const [selectedSnapshots, setSelectedSnapshots] = useState<MemorySnapshot[]>([]);

  const refreshIdRef = useRef(0);
  const selectIdRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    async function loadAgents() {
      setLoading(true);
      setError(null);
      try {
        const agentList = await fetchSleepAgents(authToken);
        if (!cancelled) setAgents(agentList);
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : "Failed to fetch agents");
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    void loadAgents();
    return () => { cancelled = true; };
  }, [authToken]);

  const refreshRuns = useCallback(() => {
    const id = ++refreshIdRef.current;
    let cancelled = false;
    async function loadRuns() {
      setLoading(true);
      setError(null);
      try {
        const fetchedRuns = await fetchSleepRuns(selectedAgent || undefined, authToken);
        if (!cancelled && id === refreshIdRef.current) setRuns(fetchedRuns);
      } catch (err) {
        if (!cancelled && id === refreshIdRef.current)
          setError(err instanceof Error ? err.message : "Failed to fetch runs");
      } finally {
        if (!cancelled && id === refreshIdRef.current) setLoading(false);
      }
    }
    void loadRuns();
    return () => { cancelled = true; };
  }, [authToken, selectedAgent]);

  useEffect(() => {
    const cleanup = refreshRuns();
    return cleanup;
  }, [refreshRuns]);

  const selectRun = useCallback((run: SleepRun) => {
    const id = ++selectIdRef.current;
    async function loadDetail() {
      setLoading(true);
      setError(null);
      try {
        const detail = await fetchSleepRunDetail(run.id, authToken);
        if (id === selectIdRef.current) {
          setSelectedRun(detail.run);
          setSelectedSnapshots(detail.snapshots);
        }
      } catch (err) {
        if (id === selectIdRef.current)
          setError(err instanceof Error ? err.message : "Failed to fetch run detail");
      } finally {
        if (id === selectIdRef.current) setLoading(false);
      }
    }
    void loadDetail();
  }, [authToken]);

  const backToList = useCallback(() => {
    setSelectedRun(null);
    setSelectedSnapshots([]);
  }, []);

  return {
    agents, selectedAgent, setSelectedAgent,
    runs, loading, error,
    selectedRun, selectedSnapshots,
    selectRun, backToList, refreshRuns,
  };
}
