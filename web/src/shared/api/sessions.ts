import type { SessionEntry } from "./types";
import { apiFetch } from "./client";

interface SessionPayload {
  session_key: string;
  label: string;
  chat_id: number;
  channel: string;
  agent_id: string;
  last_message_time: string;
  last_message_preview: string | null;
}

export async function fetchSessions(authToken: string): Promise<SessionEntry[]> {
  const data = await apiFetch<{ ok: boolean; sessions: SessionPayload[] }>(
    "/api/sessions",
    authToken,
  );
  return data.sessions.map((session) => ({
    session_key: session.session_key,
    label: session.label,
    channel: session.channel,
    agent_id: session.agent_id,
    last_message_preview: session.last_message_preview ?? "",
    last_message_time: Date.parse(session.last_message_time) || 0,
  }));
}

export function createSessionKey(now = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return [
    `session-${now.getFullYear()}`,
    pad(now.getMonth() + 1),
    pad(now.getDate()),
    pad(now.getHours()),
    pad(now.getMinutes()),
    pad(now.getSeconds()),
  ].join("");
}
