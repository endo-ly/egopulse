import { FormEvent, useRef, useState } from "react";

import {
  api,
  AuthRequiredError,
  buildDraftId,
  makeId,
  nowIso,
  parseSseFrames,
  sessionKeyNow,
} from "../api";
import type { MessageItem, UiStatus } from "../types";

type UseStreamArgs = {
  authTokenRef: React.MutableRefObject<string>;
  selectedSessionRef: React.MutableRefObject<string>;
  setSelectedSession: (key: string) => void;
  setMessages: React.Dispatch<React.SetStateAction<MessageItem[]>>;
  onAuthRequired: () => void;
  refreshSessions: (preferredKey?: string) => Promise<void>;
  withAuthHandling: (action: () => Promise<void>) => Promise<void>;
};

type UseStreamResult = {
  draft: string;
  setDraft: React.Dispatch<React.SetStateAction<string>>;
  status: UiStatus;
  setStatus: React.Dispatch<React.SetStateAction<UiStatus>>;
  handleSend: (event: FormEvent) => Promise<void>;
};

export function useStream({
  authTokenRef,
  selectedSessionRef,
  setSelectedSession,
  setMessages,
  onAuthRequired,
  refreshSessions,
  withAuthHandling,
}: UseStreamArgs): UseStreamResult {
  const [draft, setDraft] = useState("");
  const [status, setStatus] = useState<UiStatus>({
    tone: "idle",
    text: "Ready",
  });
  const sendAbortRef = useRef<AbortController | null>(null);

  async function handleSend(event: FormEvent) {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;

    const sessionKey = selectedSessionRef.current || sessionKeyNow();
    if (!selectedSessionRef.current) {
      selectedSessionRef.current = sessionKey;
      setSelectedSession(sessionKey);
    }

    const userMessage: MessageItem = {
      id: makeId("message"),
      sender_name: "web-user",
      content: text,
      is_from_bot: false,
      timestamp: nowIso(),
    };

    setMessages((prev) => [...prev, userMessage]);
    setDraft("");
    setStatus({ tone: "idle", text: "Waiting for response…" });

    sendAbortRef.current?.abort();
    const abortController = new AbortController();
    sendAbortRef.current = abortController;

    try {
      const sendResponse = await api<{
        ok: boolean;
        run_id: string;
        session_key: string;
      }>("/api/send_stream", authTokenRef.current, {
        method: "POST",
        body: JSON.stringify({ session_key: sessionKey, message: text }),
        signal: abortController.signal,
      });

      if (!sendResponse.run_id) {
        throw new Error("missing run_id");
      }

      const runId = sendResponse.run_id;
      const draftId = buildDraftId(runId);
      const query = new URLSearchParams({ run_id: runId });
      const streamResponse = await fetch(`/api/stream?${query.toString()}`, {
        method: "GET",
        cache: "no-store",
        headers: authTokenRef.current.trim()
          ? { Authorization: `Bearer ${authTokenRef.current.trim()}` }
          : {},
        signal: abortController.signal,
      });

      if (!streamResponse.ok) {
        if (streamResponse.status === 401) {
          throw new AuthRequiredError("Authentication required");
        }
        const raw = await streamResponse.text().catch(() => "");
        throw new Error(raw || `HTTP ${streamResponse.status}`);
      }

      for await (const streamEvent of parseSseFrames(
        streamResponse,
        abortController.signal,
      )) {
        const payload = streamEvent.payload;

        if (streamEvent.event === "replay_meta") continue;

        if (streamEvent.event === "status") {
          const message =
            typeof payload.message === "string" ? payload.message : "";
          if (message) {
            setStatus({ tone: "idle", text: message });
          }
          continue;
        }

        if (streamEvent.event === "delta") {
          const delta = typeof payload.delta === "string" ? payload.delta : "";
          if (!delta) continue;
          setMessages((prev) => {
            const existing = prev.find((item) => item.id === draftId);
            if (existing) {
              return prev.map((item) =>
                item.id === draftId
                  ? { ...item, content: item.content + delta }
                  : item,
              );
            }
            return [
              ...prev,
              {
                id: draftId,
                sender_name: "egopulse",
                content: delta,
                is_from_bot: true,
                timestamp: nowIso(),
              },
            ];
          });
          continue;
        }

        if (streamEvent.event === "error") {
          const errorMessage =
            typeof payload.error === "string" ? payload.error : "stream error";
          throw new Error(errorMessage);
        }

        if (streamEvent.event === "done") {
          const responseText =
            typeof payload.response === "string" ? payload.response : "";
          if (responseText) {
            setMessages((prev) => {
              const existing = prev.find((item) => item.id === draftId);
              if (existing) {
                return prev.map((item) =>
                  item.id === draftId
                    ? { ...item, id: `${draftId}:done`, content: responseText }
                    : item,
                );
              }
              return [
                ...prev,
                {
                  id: `${draftId}:done`,
                  sender_name: "egopulse",
                  content: responseText,
                  is_from_bot: true,
                  timestamp: nowIso(),
                },
              ];
            });
          } else {
            setMessages((prev) =>
              prev.map((item) =>
                item.id === draftId
                  ? { ...item, id: `${draftId}:done` }
                  : item,
              ),
            );
          }
          setStatus({ tone: "ok", text: "Response received" });
          break;
        }
      }
    } catch (error) {
      if (error instanceof AuthRequiredError) {
        onAuthRequired();
      }
      setStatus({
        tone: "error",
        text: error instanceof Error ? error.message : "Failed to send message",
      });
    } finally {
      void withAuthHandling(async () => {
        await refreshSessions(sessionKey);
      });
    }
  }

  return { draft, setDraft, status, setStatus, handleSend };
}
