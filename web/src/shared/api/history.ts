import type { ChatMessage } from "./types";
import { apiFetch } from "./client";

export async function fetchHistory(
  authToken: string,
  sessionKey: string,
): Promise<ChatMessage[]> {
  const params = new URLSearchParams({ session_key: sessionKey });
  const data = await apiFetch<{ ok: boolean; messages: ChatMessage[] }>(
    `/api/history?${params.toString()}`,
    authToken,
  );
  return data.messages;
}
