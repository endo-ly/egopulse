import type { ChatMessage, HistoryPage } from "./types";
import { apiFetch } from "./client";

/** Number of committed message rows requested per history page. */
export const HISTORY_PAGE_SIZE = 300;

interface HistoryPayload {
  ok: boolean;
  messages: ChatMessage[];
  next_before_seq: number | null;
  newest_seq: number | null;
}

export async function fetchHistory(
  authToken: string,
  sessionKey: string,
  beforeSeq?: number | null,
): Promise<HistoryPage> {
  const params = new URLSearchParams({
    session_key: sessionKey,
    limit: String(HISTORY_PAGE_SIZE),
  });
  if (beforeSeq != null) {
    params.set("before_seq", String(beforeSeq));
  }
  const data = await apiFetch<HistoryPayload>(
    `/api/history?${params.toString()}`,
    authToken,
  );
  return {
    messages: data.messages ?? [],
    nextBeforeSeq: data.next_before_seq ?? null,
    newestSeq: data.newest_seq ?? null,
  };
}
