import { useEffect, useRef, useState } from "react";

import { api, nowIso, sessionKeyNow } from "../api";
import type { MessageItem, SessionItem } from "../types";

type UseSessionsArgs = {
  authTokenRef: React.MutableRefObject<string>;
};

type UseSessionsResult = {
  sessions: SessionItem[];
  selectedSession: string;
  selectedSessionRef: React.MutableRefObject<string>;
  messages: MessageItem[];
  messageEndRef: React.RefObject<HTMLDivElement | null>;
  setSelectedSession: React.Dispatch<React.SetStateAction<string>>;
  setMessages: React.Dispatch<React.SetStateAction<MessageItem[]>>;
  refreshSessions: (preferredKey?: string) => Promise<void>;
  loadHistory: (sessionKey: string) => Promise<void>;
  handleNewSession: () => void;
};

export function useSessions({ authTokenRef }: UseSessionsArgs): UseSessionsResult {
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [selectedSession, setSelectedSession] = useState("");
  const [messages, setMessages] = useState<MessageItem[]>([]);
  const messageEndRef = useRef<HTMLDivElement | null>(null);
  const selectedSessionRef = useRef("");

  useEffect(() => {
    selectedSessionRef.current = selectedSession;
  }, [selectedSession]);

  useEffect(() => {
    messageEndRef.current?.scrollIntoView({ block: "end" });
  }, [messages]);

  async function refreshSessions(preferredKey?: string) {
    const payload = await api<{ ok: boolean; sessions: SessionItem[] }>(
      "/api/sessions",
      authTokenRef.current,
    );
    setSessions(payload.sessions);

    const nextKey =
      preferredKey ||
      selectedSessionRef.current ||
      payload.sessions[0]?.session_key ||
      sessionKeyNow();
    selectedSessionRef.current = nextKey;
    setSelectedSession(nextKey);
    await loadHistory(nextKey);
  }

  async function loadHistory(sessionKey: string) {
    const payload = await api<{ ok: boolean; messages: MessageItem[] }>(
      `/api/history?session_key=${encodeURIComponent(sessionKey)}`,
      authTokenRef.current,
    );
    setMessages(payload.messages);
  }

  function handleNewSession() {
    const key = sessionKeyNow();
    selectedSessionRef.current = key;
    setSelectedSession(key);
    setMessages([]);
    setSessions((prev) => [
      {
        session_key: key,
        label: key,
        chat_id: 0,
        channel: "web",
        last_message_time: nowIso(),
        last_message_preview: null,
      },
      ...prev,
    ]);
  }

  return {
    sessions,
    selectedSession,
    selectedSessionRef,
    messages,
    messageEndRef,
    setSelectedSession,
    setMessages,
    refreshSessions,
    loadHistory,
    handleNewSession,
  };
}
