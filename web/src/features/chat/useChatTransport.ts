import { useCallback, useEffect, useRef, useState } from "react";
import {
  initialChatState,
  reduceChatEvent,
  reduceDiscardOptimisticUserMessage,
  reduceOptimisticUserMessage,
  reduceUserInput,
  reduceToolResult,
  reduceToolStart,
  type ChatEventPayload,
  type ChatState,
  type ToolResultPayload,
  type ToolStartPayload,
  type UserInputPayload,
} from "./chatReducer";
import { AuthRequiredError } from "../../shared/api/auth";
import { wsUrl } from "../../shared/api/ws";
import { invalidateQueries } from "../../shared/hooks/useServerState";

export interface UseChatTransportOptions {
  sessionKey: string;
  agentId: string;
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
  payload?: unknown;
  error?: { code?: string; message?: string };
}

interface EventFrame {
  type: "event";
  event: string;
  payload?: unknown;
}

type ServerFrame = ResponseFrame | EventFrame;

interface ChatAckPayload {
  runId?: string;
  sessionKey?: string;
}

interface RetryableSend {
  requestId: string;
  sessionKey: string;
  text: string;
}

interface RoutedEventPayload {
  runId?: unknown;
  sessionKey?: unknown;
}

const RECONNECT_BASE_DELAY_MS = 1_000;
const RECONNECT_MAX_DELAY_MS = 30_000;
// The server acks chat.send on acceptance (not completion), so this only
// guards a lost frame, not a slow turn.
const SEND_ACK_TIMEOUT_MS = 15_000;

export function useChatTransport({
  sessionKey,
  agentId,
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
  // chat.send request ids awaiting their ack, with settle callbacks.
  const pendingSendsRef = useRef(
    new Map<
      string,
      {
        resolve: (requestId: string) => void;
        reject: (error: Error) => void;
        sessionKey: string;
        text: string;
        timer: ReturnType<typeof setTimeout>;
      }
    >(),
  );
  // A timeout means acceptance is unknown, not rejected. Keep the request id
  // attached to the unchanged draft so an explicit retry remains idempotent.
  const retryableSendsRef = useRef(new Map<string, RetryableSend>());
  const runRequestIdsRef = useRef(new Map<string, string>());
  // Maps a server run to the session key used by this transport when the
  // server canonicalizes a newly-created session to `chat:{id}`.
  const runSessionKeysRef = useRef(new Map<string, string>());
  // Set when a live socket drops unexpectedly so the next open resyncs.
  const disruptedRef = useRef(false);
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

  const settlePendingSend = useCallback(
    (requestId: string, error: Error | null) => {
      const pending = pendingSendsRef.current.get(requestId);
      if (!pending) return;
      pendingSendsRef.current.delete(requestId);
      clearTimeout(pending.timer);
      if (error) {
        setState((prev) => reduceDiscardOptimisticUserMessage(prev, requestId));
        pending.reject(error);
      } else {
        pending.resolve(requestId);
      }
    },
    [],
  );

  const findRetryableSend = useCallback((requestId: string) => {
    for (const [fingerprint, entry] of retryableSendsRef.current) {
      if (entry.requestId === requestId) return { fingerprint, entry };
    }
    return null;
  }, []);

  const rejectAllPendingSends = useCallback((error: Error, retryable: boolean) => {
    const pending = [...pendingSendsRef.current.entries()];
    pendingSendsRef.current.clear();
    for (const [requestId, entry] of pending) {
      clearTimeout(entry.timer);
      if (retryable) {
        retryableSendsRef.current.set(`${entry.sessionKey}\u0000${entry.text}`, {
          requestId,
          sessionKey: entry.sessionKey,
          text: entry.text,
        });
      }
      entry.reject(error);
    }
    if (pending.length > 0) {
      setState((prev) =>
        pending.reduce(
          (state, [requestId]) => reduceDiscardOptimisticUserMessage(state, requestId),
          prev,
        ),
      );
    }
  }, []);

  useEffect(() => {
    setState(initialChatState());
  }, [sessionKey]);

  useEffect(
    () => () => {
      intentionalCloseRef.current = true;
      clearReconnectTimer();
      rejectAllPendingSends(new Error("disconnected"), false);
      wsRef.current?.close();
    },
    [clearReconnectTimer, rejectAllPendingSends],
  );

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

      if (parsed.type === "res" && parsed.id !== "connect") {
        // The ack settles the sendMessage promise; failures surface there
        // so the composer can keep the text.
        if (parsed.ok && parsed.payload) {
          const ack = parsed.payload as ChatAckPayload;
          if (ack.runId && ack.sessionKey) {
            const pending = pendingSendsRef.current.get(parsed.id);
            const retryable = findRetryableSend(parsed.id);
            const request = pending ?? retryable?.entry;
            if (request) {
              runSessionKeysRef.current.set(ack.runId, request.sessionKey);
              runRequestIdsRef.current.set(ack.runId, parsed.id);
            }
          }
        } else if (!parsed.ok) {
          const retryable = findRetryableSend(parsed.id);
          if (retryable) retryableSendsRef.current.delete(retryable.fingerprint);
        }
        settlePendingSend(
          parsed.id,
          parsed.ok ? null : new Error(parsed.error?.message ?? "send failed"),
        );
        return;
      }

      if (parsed.type === "event" && parsed.event === "chat" && parsed.payload) {
        const event = parsed.payload as ChatEventPayload;
        if (!belongsToCurrentSession(event, sessionKeyRef.current, runSessionKeysRef.current)) {
          return;
        }
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
          const requestId = runRequestIdsRef.current.get(event.runId);
          if (requestId) {
            const retryable = findRetryableSend(requestId);
            if (retryable) retryableSendsRef.current.delete(retryable.fingerprint);
          }
          runRequestIdsRef.current.delete(event.runId);
          runSessionKeysRef.current.delete(event.runId);
        } else if (event.state === "error") {
          // A failed turn still persisted the user message; refetch it.
          invalidateQueries("sessions");
          invalidateQueries("history");
          if (event.terminal !== false) {
            const requestId = runRequestIdsRef.current.get(event.runId);
            if (requestId) {
              const retryable = findRetryableSend(requestId);
              if (retryable) retryableSendsRef.current.delete(retryable.fingerprint);
            }
            runRequestIdsRef.current.delete(event.runId);
            runSessionKeysRef.current.delete(event.runId);
          }
        }
        return;
      }

      if (parsed.type === "event" && parsed.event === "tool_start" && parsed.payload) {
        const payload = parsed.payload as RoutedEventPayload;
        if (!belongsToCurrentSession(payload, sessionKeyRef.current, runSessionKeysRef.current)) {
          return;
        }
        setState((prev) => reduceToolStart(prev, parsed.payload as ToolStartPayload));
        return;
      }

      if (parsed.type === "event" && parsed.event === "tool_result" && parsed.payload) {
        const payload = parsed.payload as RoutedEventPayload;
        if (!belongsToCurrentSession(payload, sessionKeyRef.current, runSessionKeysRef.current)) {
          return;
        }
        setState((prev) => reduceToolResult(prev, parsed.payload as ToolResultPayload));
        return;
      }

      if (parsed.type === "event" && parsed.event === "user_input" && parsed.payload) {
        const payload = parsed.payload as RoutedEventPayload;
        if (!belongsToCurrentSession(payload, sessionKeyRef.current, runSessionKeysRef.current)) {
          return;
        }
        setState((prev) => reduceUserInput(prev, parsed.payload as UserInputPayload));
      }
    },
    [authToken, onAuthRequired, onDone, onError, clearReconnectTimer, findRetryableSend, settlePendingSend],
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
          if (disruptedRef.current) {
            // The in-flight run lost its subscription; pull whatever
            // persisted while we were away.
            disruptedRef.current = false;
            invalidateQueries("sessions");
            invalidateQueries("history");
          }
        };
        ws.onclose = () => {
          if (wasOpenRef.current && !intentionalCloseRef.current) {
            onError?.("Connection lost. Retrying…");
            disruptedRef.current = true;
            rejectAllPendingSends(new Error("Connection lost. Retrying…"), true);
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
    [handleMessage, onError, clearReconnectTimer, rejectAllPendingSends],
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
    rejectAllPendingSends(new Error("disconnected"), false);
    wsRef.current?.close();
    wsRef.current = null;
    setConnectionState("closed");
  }, [clearReconnectTimer, rejectAllPendingSends]);

  const sendMessage = useCallback(
    async (text: string): Promise<string | null> => {
      await connect();
      const ws = wsRef.current;
      if (!ws || ws.readyState !== WebSocket.OPEN) return null;

      const fingerprint = `${sessionKey}\u0000${text}`;
      const retryable = retryableSendsRef.current.get(fingerprint);
      const requestId = retryable?.requestId ?? crypto.randomUUID();
      if (retryable) retryableSendsRef.current.delete(fingerprint);
      const msg = {
        type: "req",
        id: requestId,
        method: "chat.send",
        params: {
          sessionKey,
          agentId,
          message: text,
          requestId,
        },
      };
      setState((prev) => reduceOptimisticUserMessage(prev, { requestId, text }));
      // Resolve on the server ack; reject on refusal or timeout so the
      // caller can keep the text instead of losing it.
      return new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => {
          const pending = pendingSendsRef.current.get(requestId);
          if (!pending) return;
          pendingSendsRef.current.delete(requestId);
          retryableSendsRef.current.set(fingerprint, { requestId, sessionKey, text });
          setState((prev) => reduceDiscardOptimisticUserMessage(prev, requestId));
          reject(new Error("send acknowledgement timed out"));
        }, SEND_ACK_TIMEOUT_MS);
        pendingSendsRef.current.set(requestId, { resolve, reject, sessionKey, text, timer });
        try {
          ws.send(JSON.stringify(msg));
        } catch (error) {
          pendingSendsRef.current.delete(requestId);
          clearTimeout(timer);
          retryableSendsRef.current.set(fingerprint, { requestId, sessionKey, text });
          setState((prev) => reduceDiscardOptimisticUserMessage(prev, requestId));
          reject(error instanceof Error ? error : new Error("failed to send message"));
        }
      });
    },
    [agentId, connect, sessionKey],
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

function belongsToCurrentSession(
  payload: RoutedEventPayload,
  currentSessionKey: string,
  runSessionKeys: Map<string, string>,
): boolean {
  if (payload.sessionKey === currentSessionKey) return true;
  return typeof payload.runId === "string" && runSessionKeys.get(payload.runId) === currentSessionKey;
}
