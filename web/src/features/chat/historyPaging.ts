import type { ChatMessage } from "../../shared/api/types";

/**
 * Joins one newest history page into the already loaded messages.
 *
 * Entries keep their loaded positions; the page only updates content by id
 * and appends ids that are not loaded yet. This makes a sliding newest-page
 * window additive instead of destructive.
 */
export function mergeHistoryMessages(
  loaded: ChatMessage[],
  incoming: ChatMessage[],
): ChatMessage[] {
  if (incoming.length === 0) return loaded;
  if (loaded.length === 0) return incoming;

  const byId = new Map(incoming.map((message) => [message.id, message]));
  const merged = loaded.map((message) => byId.get(message.id) ?? message);
  const seen = new Set(loaded.map((message) => message.id));
  for (const message of incoming) {
    if (seen.has(message.id)) continue;
    merged.push(message);
    seen.add(message.id);
  }
  return merged;
}

/** Prepends an older page, dropping entries that are already loaded. */
export function prependHistoryMessages(
  loaded: ChatMessage[],
  older: ChatMessage[],
): ChatMessage[] {
  if (older.length === 0) return loaded;
  const seen = new Set(loaded.map((message) => message.id));
  const prepend: ChatMessage[] = [];
  for (const message of older) {
    if (seen.has(message.id)) continue;
    prepend.push(message);
    seen.add(message.id);
  }
  if (prepend.length === 0) return loaded;
  return [...prepend, ...loaded];
}

export interface GapBounds {
  /** Exclusive cursor for the first gap page. */
  from: number;
  /** Highest seq already covered by the loaded history. */
  downTo: number;
}

/**
 * Detects a jump between the loaded history and a refetched newest page.
 *
 * A newest-page window normally overlaps the loaded rows. When it starts
 * above them, new commits pushed the window forward faster than the page
 * size and the missing seq range has to be fetched before merging.
 */
export function gapBounds(
  loadedNewestSeq: number | null,
  pageNextBeforeSeq: number | null,
): GapBounds | null {
  if (loadedNewestSeq === null || pageNextBeforeSeq === null) return null;
  if (pageNextBeforeSeq <= loadedNewestSeq + 1) return null;
  return { from: pageNextBeforeSeq, downTo: loadedNewestSeq };
}
