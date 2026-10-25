import type { SessionEntry } from "./types";
import { apiFetch } from "./client";
import { randomId } from "../lib/random";

interface SessionPayload {
  session_key: string;
  name: string | null;
  chat_id: number;
  channel: string;
  agent_id: string;
  last_message_time: string;
  last_message_preview: string | null;
  archived: boolean;
  pinned: boolean;
}

/** Metadata the Web UI can change for one session. */
export interface SessionPatch {
  name?: string | null;
  archived?: boolean;
  pinned?: boolean;
}

export async function fetchSessions(authToken: string): Promise<SessionEntry[]> {
  const data = await apiFetch<{ ok: boolean; sessions: SessionPayload[] }>(
    "/api/sessions",
    authToken,
  );
  return data.sessions.map((session) => ({
    session_key: session.session_key,
    chat_id: session.chat_id,
    name: session.name ?? null,
    channel: session.channel,
    agent_id: session.agent_id,
    last_message_preview: session.last_message_preview ?? "",
    last_message_time: Date.parse(session.last_message_time) || 0,
    archived: session.archived,
    pinned: session.pinned,
  }));
}

/** Updates the display name, archive flag, or pin flag of one session. */
export async function updateSession(
  authToken: string,
  chatId: number,
  patch: SessionPatch,
): Promise<void> {
  await apiFetch<{ ok: boolean }>(`/api/sessions/${chatId}`, authToken, {
    method: "PATCH",
    body: JSON.stringify(patch),
  });
}

/** Generates a unique draft key for a new web session. */
export function createSessionKey(now = new Date(), id = randomId()): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  const timestamp = [
    now.getFullYear(),
    pad(now.getMonth() + 1),
    pad(now.getDate()),
    pad(now.getHours()),
    pad(now.getMinutes()),
    pad(now.getSeconds()),
  ].join("");
  return `session-${timestamp}-${id}`;
}
