import type { SessionEntry } from "../api/types";
import { formatRelativeTime } from "./format";

const UNTITLED = "Untitled session";

/** Primary label for a session: its name, else the last message preview. */
export function sessionTitle(session: SessionEntry): string {
  const name = session.name?.trim();
  if (name) return name;
  const preview = session.last_message_preview.trim();
  return preview || UNTITLED;
}

/** Secondary line: the preview when the row is already identified by a name. */
export function sessionSubtitle(session: SessionEntry): string {
  const name = session.name?.trim();
  if (!name) return "";
  return session.last_message_preview.trim();
}

/** Compact age of the last message, or an empty string when there is none. */
export function sessionTime(session: SessionEntry, now = Date.now()): string {
  const timestamp = session.last_message_time;
  if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return "";
  return formatRelativeTime(date.toISOString(), now);
}
