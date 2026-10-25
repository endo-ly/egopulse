import type { SessionEntry } from "../api/types";

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
