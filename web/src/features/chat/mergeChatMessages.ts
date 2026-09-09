import { useEffect, useRef } from "react";
import type { ChatMessage } from "../../shared/api/types";
import { isLiveMessageId } from "./chatReducer";

function contentKey(message: ChatMessage): string {
  return `${message.sender_kind}\n${message.message_kind}\n${message.content}`;
}

/**
 * Joins persisted history with live transport messages without duplicates.
 *
 * Transport messages carry client-side ids (`draft:` streaming text,
 * `local:` optimistic input, `tool:` cards) while history carries persisted
 * ids, so the two sources overlap after every refetch:
 *
 * - an id present in history wins (server echo, persisted tool cards);
 * - optimistic locals and sealed drafts are reconciled only against
 *   `freshIds`, i.e. history entries that arrived since the last render.
 *   Matching against the whole history would hide a fresh message behind an
 *   older identical one (e.g. resending the same text);
 * - streaming drafts are always kept — their content is still growing.
 */
export function mergeChatMessages(
  history: ChatMessage[],
  live: ChatMessage[],
  freshIds: ReadonlySet<string>,
): ChatMessage[] {
  if (live.length === 0) return history;
  const historyIds = new Set(history.map((message) => message.id));
  const freshByContent = new Map<string, number>();
  for (const message of history) {
    if (!freshIds.has(message.id)) continue;
    const key = contentKey(message);
    freshByContent.set(key, (freshByContent.get(key) ?? 0) + 1);
  }
  const consumeFresh = (message: ChatMessage): boolean => {
    const key = contentKey(message);
    const remaining = freshByContent.get(key) ?? 0;
    if (remaining <= 0) return false;
    freshByContent.set(key, remaining - 1);
    return true;
  };
  const kept = live.filter((message) => {
    if (historyIds.has(message.id)) return false;
    if (!isLiveMessageId(message.id)) return true;
    if (message.id.startsWith("draft:") && !message.id.includes(":done")) {
      return true;
    }
    return !consumeFresh(message);
  });
  return [...history, ...kept];
}

interface SeenHistory {
  sessionKey: string;
  ids: Set<string>;
}

/**
 * Merges history with live messages, treating only newly arrived history
 * entries as reconciliation candidates. Seen ids reset on session switch.
 */
export function useMergedChatMessages(
  sessionKey: string,
  history: ChatMessage[],
  live: ChatMessage[],
): ChatMessage[] {
  // Mount (and session switch) initializes everything as seen — only
  // later arrivals reconcile. StrictMode-safe: the ref is written only on
  // init/switch, so double renders agree.
  const seenRef = useRef<SeenHistory | null>(null);
  if (seenRef.current === null || seenRef.current.sessionKey !== sessionKey) {
    seenRef.current = {
      sessionKey,
      ids: new Set(history.map((message) => message.id)),
    };
  }
  const seen: ReadonlySet<string> = seenRef.current?.ids ?? new Set();
  // Ref writes do not trigger renders. Recompute from the current seen set on
  // every render so a fresh id cannot remain eligible during a later
  // history-stable, live-only update.
  const freshIds = new Set(
    history.filter((message) => !seen.has(message.id)).map((message) => message.id),
  );
  useEffect(() => {
    const previous =
      seenRef.current?.sessionKey === sessionKey ? seenRef.current.ids : [];
    seenRef.current = {
      sessionKey,
      ids: new Set([...previous, ...history.map((message) => message.id)]),
    };
  }, [sessionKey, history]);
  return mergeChatMessages(history, live, freshIds);
}
