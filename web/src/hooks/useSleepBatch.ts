import { useCallback, useEffect, useRef, useState } from "react";
import { fetchSleepAgents, fetchSleepRunDetail, fetchSleepRuns } from "../api";
import type { MemorySnapshot, SleepRun } from "../types";

export function useSleepBatch() {
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
        const agentList = await fetchSleepAgents();
        if (!cancelled) setAgents(agentList);
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : "Failed to fetch agents");
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    void loadAgents();
    return () => { cancelled = true; };
  }, []);

  const refreshRuns = useCallback(() => {
    const id = ++refreshIdRef.current;
    let cancelled = false;
    async function loadRuns() {
      setLoading(true);
      setError(null);
      try {
        const fetchedRuns = await fetchSleepRuns(selectedAgent || undefined);
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
  }, [selectedAgent]);

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
        const detail = await fetchSleepRunDetail(run.id);
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
  }, []);

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
