import { useCallback, useRef, useState } from "react";
import {
  initialChatState,
  reduceChatEvent,
  type ChatEventPayload,
  type ChatState,
} from "./chatReducer";

export interface UseChatTransportOptions {
  sessionKey: string;
  onDone?: () => void;
}

export function useChatTransport({ sessionKey, onDone }: UseChatTransportOptions) {
  const [state, setState] = useState<ChatState>(initialChatState);
  const [connectionState, setConnectionState] = useState<
    "connecting" | "open" | "closed"
  >("closed");
  const wsRef = useRef<WebSocket | null>(null);
  const runIdRef = useRef<string | null>(null);

  const handleMessage = useCallback(
    (raw: string) => {
      let parsed: { type?: string; event?: string; payload?: unknown };
      try {
        parsed = JSON.parse(raw);
      } catch {
        return;
      }

      if (parsed.type === "event" && parsed.event === "chat" && parsed.payload) {
        const event = parsed.payload as ChatEventPayload;
        setState((prev) => reduceChatEvent(prev, event));
        if (event.state === "done") {
          onDone?.();
        }
      }
    },
    [onDone],
  );

  const connect = useCallback(
    (url: string) => {
      if (wsRef.current) return;
      setConnectionState("connecting");
      const ws = new WebSocket(url);
      wsRef.current = ws;

      ws.onopen = () => setConnectionState("open");
      ws.onclose = () => {
        setConnectionState("closed");
        wsRef.current = null;
      };
      ws.onmessage = (e) => {
        if (typeof e.data === "string") handleMessage(e.data);
      };
    },
    [handleMessage],
  );

  const disconnect = useCallback(() => {
    wsRef.current?.close();
    wsRef.current = null;
    setConnectionState("closed");
  }, []);

  const sendMessage = useCallback(
    (text: string): string | null => {
      const ws = wsRef.current;
      if (!ws || ws.readyState !== WebSocket.OPEN) return null;

      const runId = crypto.randomUUID();
      runIdRef.current = runId;

      const msg = {
        type: "req",
        id: crypto.randomUUID(),
        method: "chat.send",
        params: {
          sessionKey,
          message: text,
        },
      };
      ws.send(JSON.stringify(msg));
      return runId;
    },
    [sessionKey],
  );

  return {
    state,
    connectionState,
    connect,
    disconnect,
    sendMessage,
    handleMessage,
  };
}
