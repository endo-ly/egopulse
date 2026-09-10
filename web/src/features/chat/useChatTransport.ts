import { useCallback, useEffect, useRef, useState } from "react";
import {
  initialChatState,
  reduceChatEvent,
  reduceDiscardOptimisticUserMessage,
  reduceOptimisticUserMessage,
  reduceRunAccepted,
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
}

interface UncertainSend {
  draftId: string;
  durableRequestId: string;
  sessionKey: string;
  text: string;
}

interface PendingSend {
  resolve: (durableRequestId: string) => void;
  reject: (error: Error) => void;
  draftId: string;
  durableRequestId: string;
  // The session and agent at the time the send was issued. The user may
  // switch to another session before the ack arrives, so per-run state must
  // not depend on what the transport happens to show at that moment.
  sessionKey: string;
  agentId: string;
  text: string;
  timer: ReturnType<typeof setTimeout>;
}

interface RoutedEventPayload {
  runId?: unknown;
  sessionKey?: unknown;
}

/// Per-run tracking for runs accepted from this transport. Lives until the
/// run terminates; independent of which session is currently displayed.
interface RunTracking {
  // The (possibly pre-canonical) session label the send was issued with.
  sessionKey: string;
  agentId: string;
  // Last gateway seq applied; frames at or below it are replays the client
  // already applied, so they are dropped instead of appended twice.
  lastSeq: number;
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
  const agentIdRef = useRef(agentId);
  agentIdRef.current = agentId;
  const onSessionResolvedRef = useRef(onSessionResolved);
  onSessionResolvedRef.current = onSessionResolved;
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reconnectAttemptRef = useRef(0);
  // RPC ids identify one WebSocket attempt. Durable request ids identify the
  // logical send and remain stable only while its current draft is uncertain.
  const pendingSendsRef = useRef(new Map<string, PendingSend>());
  const uncertainSendRef = useRef<UncertainSend | null>(null);
  // Runs accepted by this transport, until they terminate. After every
  // reconnect they are resubscribed (run.subscribe) so streaming continues
  // where the dropped socket left it.
  const runsRef = useRef(new Map<string, RunTracking>());
  // RPC ids of pending run.subscribe requests, to interpret their responses.
  const resubscribeRpcsRef = useRef(new Map<string, string>());
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
    (rpcId: string, error: Error | null) => {
      const pending = pendingSendsRef.current.get(rpcId);
      if (!pending) return;
      pendingSendsRef.current.delete(rpcId);
      clearTimeout(pending.timer);
      if (error) {
        setState((prev) =>
          reduceDiscardOptimisticUserMessage(prev, pending.durableRequestId),
        );
        pending.reject(error);
      } else {
        pending.resolve(pending.durableRequestId);
      }
    },
    [],
  );

  const rejectAllPendingSends = useCallback((error: Error, retryable: boolean) => {
    const pending = [...pendingSendsRef.current.entries()];
    pendingSendsRef.current.clear();
    for (const [_rpcId, entry] of pending) {
      clearTimeout(entry.timer);
      if (retryable) {
        uncertainSendRef.current = {
          draftId: entry.draftId,
          durableRequestId: entry.durableRequestId,
          sessionKey: entry.sessionKey,
          text: entry.text,
        };
      }
      entry.reject(error);
    }
    if (!retryable) uncertainSendRef.current = null;
    if (pending.length > 0) {
      setState((prev) =>
        pending.reduce(
          (state, [_rpcId, entry]) =>
            reduceDiscardOptimisticUserMessage(state, entry.durableRequestId),
          prev,
        ),
      );
    }
  }, []);

  useEffect(() => {
    setState(initialChatState());
    if (uncertainSendRef.current?.sessionKey !== sessionKey) {
      uncertainSendRef.current = null;
    }
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

  // Re-attaches this socket to runs accepted before the connection dropped.
  // The server replays everything after the client's last applied seq, so
  // streaming resumes without a gap or duplicates.
  const resubscribeInFlightRuns = useCallback(() => {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    for (const [runId, tracking] of runsRef.current) {
      const rpcId = crypto.randomUUID();
      resubscribeRpcsRef.current.set(rpcId, runId);
      ws.send(
        JSON.stringify({
          type: "req",
          id: rpcId,
          method: "run.subscribe",
          params: {
            runId,
            lastSeq: tracking.lastSeq,
          },
        }),
      );
    }
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
          resubscribeInFlightRuns();
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
        const resubscribedRunId = resubscribeRpcsRef.current.get(parsed.id);
        if (resubscribedRunId !== undefined) {
          resubscribeRpcsRef.current.delete(parsed.id);
          if (!parsed.ok) {
            // The run is gone (TTL expiry, restart): its events will never
            // arrive, so drop the frozen live transcript and per-run seq
            // state, then reconcile from persisted history.
            runsRef.current.delete(resubscribedRunId);
            setState((prev) => ({
              ...prev,
              messages: prev.messages.filter(
                (message) =>
                  message.id !== `draft:${resubscribedRunId}` &&
                  !message.id.startsWith(`draft:${resubscribedRunId}:`),
              ),
            }));
            invalidateQueries("sessions");
            invalidateQueries("history");
          } else {
            const result = parsed.payload as {
              replayTruncated?: boolean;
            } | null;
            if (result?.replayTruncated) {
              // The replay buffer already evicted events the client applied,
              // so the resumed stream starts mid-run and would render a
              // half transcript. Drop the drafts and rebuild from history;
              // the live subscription stays attached and fills forward.
              setState((prev) => ({
                ...prev,
                messages: prev.messages.filter(
                  (message) =>
                    message.id !== `draft:${resubscribedRunId}` &&
                    !message.id.startsWith(`draft:${resubscribedRunId}:`),
                ),
              }));
              invalidateQueries("sessions");
              invalidateQueries("history");
            }
          }
          return;
        }
        // The ack settles the sendMessage promise; failures surface there
        // so the composer can keep the text.
        const pending = pendingSendsRef.current.get(parsed.id);
        if (parsed.ok && parsed.payload) {
          const { runId } = parsed.payload as ChatAckPayload;
          if (runId && pending) {
            runsRef.current.set(runId, {
              sessionKey: pending.sessionKey,
              agentId: pending.agentId,
              lastSeq: 0,
            });
            // Show the assistant's turn has started only in the session that
            // issued the send: the user may have switched away while the ack
            // was in flight, and that session must not inherit the draft.
            if (sessionKeyRef.current === pending.sessionKey) {
              setState((prev) =>
                reduceRunAccepted(prev, {
                  runId,
                  agentId: pending.agentId,
                }),
              );
            }
          }
        } else if (!parsed.ok && pending) {
          uncertainSendRef.current = null;
        }
        settlePendingSend(
          parsed.id,
          parsed.ok ? null : new Error(parsed.error?.message ?? "send failed"),
        );
        return;
      }

      if (parsed.type === "event" && parsed.event === "chat" && parsed.payload) {
        const event = parsed.payload as ChatEventPayload;
        if (!belongsToCurrentSession(event, sessionKeyRef.current, runsRef.current)) {
          return;
        }
        // Replayed frames at or below the last applied seq are already in the
        // transcript; applying them again would duplicate streamed text.
        const tracking = runsRef.current.get(event.runId);
        if (tracking) {
          if (event.seq <= tracking.lastSeq) {
            return;
          }
          tracking.lastSeq = event.seq;
        }
        setState((prev) => reduceChatEvent(prev, event, agentIdRef.current));
        if (event.state === "done") {
          // The server reports the run's canonical session; switch the UI
          // over only when the run still belongs to the session the user is
          // looking at (i.e. this transport sent it and they never left).
          if (
            event.sessionKey &&
            event.sessionKey !== sessionKeyRef.current &&
            tracking &&
            tracking.sessionKey === sessionKeyRef.current &&
            onSessionResolvedRef.current
          ) {
            onSessionResolvedRef.current(event.sessionKey);
          }
          invalidateQueries("sessions");
          invalidateQueries("history");
          if (event.terminal !== false) {
            onDone?.();
            runsRef.current.delete(event.runId);
          }
        } else if (event.state === "error") {
          // A failed turn still persisted the user message; refetch it.
          invalidateQueries("sessions");
          invalidateQueries("history");
          if (event.terminal !== false) {
            runsRef.current.delete(event.runId);
          }
        }
        return;
      }

      if (parsed.type === "event" && parsed.event === "tool_start" && parsed.payload) {
        const payload = parsed.payload as RoutedEventPayload;
        if (!belongsToCurrentSession(payload, sessionKeyRef.current, runsRef.current)) {
          return;
        }
        setState((prev) => reduceToolStart(prev, parsed.payload as ToolStartPayload));
        return;
      }

      if (parsed.type === "event" && parsed.event === "tool_result" && parsed.payload) {
        const payload = parsed.payload as RoutedEventPayload;
        if (!belongsToCurrentSession(payload, sessionKeyRef.current, runsRef.current)) {
          return;
        }
        setState((prev) => reduceToolResult(prev, parsed.payload as ToolResultPayload));
        return;
      }

      if (parsed.type === "event" && parsed.event === "user_input" && parsed.payload) {
        const payload = parsed.payload as RoutedEventPayload;
        if (!belongsToCurrentSession(payload, sessionKeyRef.current, runsRef.current)) {
          return;
        }
        setState((prev) => reduceUserInput(prev, parsed.payload as UserInputPayload));
      }
    },
    [authToken, onAuthRequired, onDone, onError, clearReconnectTimer, settlePendingSend, resubscribeInFlightRuns],
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

  // A backgrounded mobile browser kills the socket and the backoff timer may
  // not fire until long after the user returns. Regaining visibility
  // reconnects immediately (skipping the remaining backoff) so run events
  // are missed for seconds rather than tens of seconds.
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState !== "visible") return;
      if (wsRef.current || connectPromiseRef.current) return;
      if (intentionalCloseRef.current) return;
      clearReconnectTimer();
      reconnectAttemptRef.current = 0;
      void connect({ background: true }).catch(() => {});
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
  }, [connect, clearReconnectTimer]);

  const disconnect = useCallback(() => {
    intentionalCloseRef.current = true;
    clearReconnectTimer();
    rejectAllPendingSends(new Error("disconnected"), false);
    wsRef.current?.close();
    wsRef.current = null;
    setConnectionState("closed");
  }, [clearReconnectTimer, rejectAllPendingSends]);

  const sendMessage = useCallback(
    async (text: string, draftId: string): Promise<string | null> => {
      await connect();
      const ws = wsRef.current;
      if (!ws || ws.readyState !== WebSocket.OPEN) return null;

      const uncertain = uncertainSendRef.current;
      const durableRequestId =
        uncertain &&
        uncertain.draftId === draftId &&
        uncertain.sessionKey === sessionKey &&
        uncertain.text === text
          ? uncertain.durableRequestId
          : crypto.randomUUID();
      uncertainSendRef.current = null;
      const rpcId = crypto.randomUUID();
      const msg = {
        type: "req",
        id: rpcId,
        method: "chat.send",
        params: {
          sessionKey,
          agentId,
          message: text,
          requestId: durableRequestId,
        },
      };
      setState((prev) =>
        reduceOptimisticUserMessage(prev, { requestId: durableRequestId, text }),
      );
      // Resolve on the server ack; reject on refusal or timeout so the
      // caller can keep the text instead of losing it.
      return new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => {
          const pending = pendingSendsRef.current.get(rpcId);
          if (!pending) return;
          pendingSendsRef.current.delete(rpcId);
          uncertainSendRef.current = {
            draftId,
            durableRequestId,
            sessionKey,
            text,
          };
          setState((prev) =>
            reduceDiscardOptimisticUserMessage(prev, durableRequestId),
          );
          reject(new Error("send acknowledgement timed out"));
        }, SEND_ACK_TIMEOUT_MS);
        pendingSendsRef.current.set(rpcId, {
          resolve,
          reject,
          draftId,
          durableRequestId,
          sessionKey,
          agentId,
          text,
          timer,
        });
        try {
          ws.send(JSON.stringify(msg));
        } catch (error) {
          pendingSendsRef.current.delete(rpcId);
          clearTimeout(timer);
          uncertainSendRef.current = {
            draftId,
            durableRequestId,
            sessionKey,
            text,
          };
          setState((prev) =>
            reduceDiscardOptimisticUserMessage(prev, durableRequestId),
          );
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
  runs: Map<string, RunTracking>,
): boolean {
  if (payload.sessionKey === currentSessionKey) return true;
  const runId = typeof payload.runId === "string" ? payload.runId : null;
  return runId !== null && runs.get(runId)?.sessionKey === currentSessionKey;
}
