import { useCallback, useEffect, useRef, useState } from "react";
import {
  initialChatState,
  reduceChatEvent,
  reduceToolResult,
  reduceToolStart,
  type ChatEventPayload,
  type ChatState,
  type ToolResultPayload,
  type ToolStartPayload,
} from "./chatReducer";
import { AuthRequiredError } from "../../shared/api/auth";
import { wsUrl } from "../../shared/api/ws";
import { invalidateQueries } from "../../shared/hooks/useServerState";

export interface UseChatTransportOptions {
  sessionKey: string;
  authToken: string;
  onDone?: () => void;
  onAuthRequired?: (message: string) => void;
  onError?: (message: string) => void;
  onSessionResolved?: (sessionKey: string) => void;
}

interface ResponseFrame {
  type: "res";
  id: string;
  ok: boolean;
  error?: { code?: string; message?: string };
}

interface EventFrame {
  type: "event";
  event: string;
  payload?: unknown;
}

type ServerFrame = ResponseFrame | EventFrame;

const RECONNECT_BASE_DELAY_MS = 1_000;
const RECONNECT_MAX_DELAY_MS = 30_000;

export function useChatTransport({
  sessionKey,
  authToken,
  onDone,
  onAuthRequired,
  onError,
  onSessionResolved,
}: UseChatTransportOptions) {
  const [state, setState] = useState<ChatState>(initialChatState);
  const [connectionState, setConnectionState] = useState<
    "connecting" | "open" | "closed"
  >("closed");
  const wsRef = useRef<WebSocket | null>(null);
  const connectPromiseRef = useRef<Promise<void> | null>(null);
  const connectResolveRef = useRef<(() => void) | null>(null);
  const connectRejectRef = useRef<((error: Error) => void) | null>(null);
  const sessionKeyRef = useRef(sessionKey);
  sessionKeyRef.current = sessionKey;
  const onSessionResolvedRef = useRef(onSessionResolved);
  onSessionResolvedRef.current = onSessionResolved;
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reconnectAttemptRef = useRef(0);
  // Tracks whether the current socket ever reached "open" so an unexpected
  // drop can be reported exactly once, instead of on every retry.
  const wasOpenRef = useRef(false);
  // Set when closing is deliberate (unmount, disconnect, auth rejection) so
  // the close handler does not schedule another reconnect.
  const intentionalCloseRef = useRef(false);
  const scheduleReconnectRef = useRef<() => void>(() => {});

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  useEffect(() => {
    setState(initialChatState());
  }, [sessionKey]);

  useEffect(() => () => {
    intentionalCloseRef.current = true;
    clearReconnectTimer();
    wsRef.current?.close();
  }, []);

  const handleMessage = useCallback(
    (raw: string) => {
      let parsed: ServerFrame;
      try {
        parsed = JSON.parse(raw) as ServerFrame;
      } catch {
        onError?.("invalid gateway frame");
        return;
      }

      if (parsed.type === "event" && parsed.event === "connect.challenge") {
        wsRef.current?.send(JSON.stringify({
          type: "req",
          id: "connect",
          method: "connect",
          params: { minProtocol: 1, maxProtocol: 1, authToken },
        }));
        return;
      }

      if (parsed.type === "res" && parsed.id === "connect") {
        if (parsed.ok) {
          connectResolveRef.current?.();
        } else {
          const message = parsed.error?.message ?? "gateway connection rejected";
          const error = parsed.error?.code === "unauthorized"
            ? new AuthRequiredError(message)
            : new Error(message);
          if (error instanceof AuthRequiredError) onAuthRequired?.(message);
          // A rejected handshake is not retryable on its own; stop the
          // reconnect loop and let the user act (e.g. unlock).
          intentionalCloseRef.current = true;
          clearReconnectTimer();
          connectRejectRef.current?.(error);
          wsRef.current?.close();
        }
        connectPromiseRef.current = null;
        connectResolveRef.current = null;
        connectRejectRef.current = null;
        return;
      }

      if (parsed.type === "event" && parsed.event === "chat" && parsed.payload) {
        const event = parsed.payload as ChatEventPayload;
        setState((prev) => reduceChatEvent(prev, event));
        if (event.state === "done") {
          if (
            event.sessionKey &&
            event.sessionKey !== sessionKeyRef.current &&
            onSessionResolvedRef.current
          ) {
            onSessionResolvedRef.current(event.sessionKey);
          }
          invalidateQueries("sessions");
          invalidateQueries("history");
          onDone?.();
        }
        return;
      }

      if (parsed.type === "event" && parsed.event === "tool_start" && parsed.payload) {
        setState((prev) => reduceToolStart(prev, parsed.payload as ToolStartPayload));
        return;
      }

      if (parsed.type === "event" && parsed.event === "tool_result" && parsed.payload) {
        setState((prev) => reduceToolResult(prev, parsed.payload as ToolResultPayload));
        return;
      }
    },
    [authToken, onAuthRequired, onDone, onError, clearReconnectTimer],
  );

  const connect = useCallback(
    async (options?: { background?: boolean }) => {
      const background = options?.background ?? false;
      if (wsRef.current?.readyState === WebSocket.OPEN) return;
      // A user-driven connect supersedes any pending backoff attempt.
      clearReconnectTimer();
      if (connectPromiseRef.current) return connectPromiseRef.current;
      intentionalCloseRef.current = false;

      connectPromiseRef.current = new Promise<void>((resolve, reject) => {
        connectResolveRef.current = resolve;
        connectRejectRef.current = reject;

        setConnectionState("connecting");
        const ws = new WebSocket(wsUrl());
        wsRef.current = ws;

        ws.onopen = () => {
          reconnectAttemptRef.current = 0;
          wasOpenRef.current = true;
          setConnectionState("open");
        };
        ws.onclose = () => {
          if (wasOpenRef.current && !intentionalCloseRef.current) {
            onError?.("Connection lost. Retrying…");
          }
          wasOpenRef.current = false;
          setConnectionState("closed");
          wsRef.current = null;
          connectPromiseRef.current = null;
          connectResolveRef.current = null;
          connectRejectRef.current = null;
          scheduleReconnectRef.current();
        };
        ws.onerror = () => {
          setConnectionState("closed");
          reject(new Error("websocket error"));
          // Background retries stay quiet; only explicit connects surface
          // the failure so a downed gateway does not spam the UI.
          if (!background) onError?.("gateway connection failed");
        };
        ws.onmessage = (event) => {
          if (typeof event.data === "string") handleMessage(event.data);
        };
      });

      return connectPromiseRef.current;
    },
    [handleMessage, onError, clearReconnectTimer],
  );

  const scheduleReconnect = useCallback(() => {
    if (intentionalCloseRef.current) return;
    if (reconnectTimerRef.current !== null) return;
    const delay = Math.min(
      RECONNECT_BASE_DELAY_MS * 2 ** reconnectAttemptRef.current,
      RECONNECT_MAX_DELAY_MS,
    );
    reconnectTimerRef.current = setTimeout(() => {
      reconnectTimerRef.current = null;
      reconnectAttemptRef.current += 1;
      void connect({ background: true }).catch(() => {});
    }, delay);
  }, [connect]);
  scheduleReconnectRef.current = scheduleReconnect;

  const disconnect = useCallback(() => {
    intentionalCloseRef.current = true;
    clearReconnectTimer();
    wsRef.current?.close();
    wsRef.current = null;
    setConnectionState("closed");
  }, []);

  const sendMessage = useCallback(
    async (text: string): Promise<string | null> => {
      await connect();
      const ws = wsRef.current;
      if (!ws || ws.readyState !== WebSocket.OPEN) return null;

      const requestId = crypto.randomUUID();
      const msg = {
        type: "req",
        id: requestId,
        method: "chat.send",
        params: {
          sessionKey,
          message: text,
        },
      };
      ws.send(JSON.stringify(msg));
      return requestId;
    },
    [connect, sessionKey],
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
