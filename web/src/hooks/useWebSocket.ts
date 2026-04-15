import { useEffect, useRef, useState } from "react";

import { AuthRequiredError, wsUrl } from "../api";
import type { UiStatus, WsEvent, WsReq, WsRes } from "../types";

type UseWebSocketArgs = {
  authTokenRef: React.MutableRefObject<string>;
  onAuthRequired: (message: string) => void;
  onStatusChange: (status: UiStatus) => void;
};

type UseWebSocketResult = {
  wsState: "connecting" | "open" | "closed";
  connect: () => Promise<void>;
};

export function useWebSocket({
  authTokenRef,
  onAuthRequired,
  onStatusChange,
}: UseWebSocketArgs): UseWebSocketResult {
  const [wsState, setWsState] = useState<"connecting" | "open" | "closed">(
    "connecting",
  );
  const socketRef = useRef<WebSocket | null>(null);
  const connectPromise = useRef<Promise<void> | null>(null);
  const connectResolve = useRef<(() => void) | null>(null);
  const connectReject = useRef<((error: Error) => void) | null>(null);

  useEffect(() => {
    return () => {
      socketRef.current?.close();
    };
  }, []);

  async function connect() {
    if (socketRef.current && socketRef.current.readyState === WebSocket.OPEN) {
      return;
    }
    if (connectPromise.current) {
      return connectPromise.current;
    }

    setWsState("connecting");
    connectPromise.current = new Promise<void>((resolve, reject) => {
      connectResolve.current = resolve;
      connectReject.current = reject;
      const socket = new WebSocket(wsUrl());
      socketRef.current = socket;

      socket.addEventListener("message", (event) => {
        let data: WsRes | WsEvent;
        try {
          data = JSON.parse(String(event.data)) as WsRes | WsEvent;
        } catch {
          connectReject.current?.(new Error("invalid JSON from server"));
          connectPromise.current = null;
          connectResolve.current = null;
          connectReject.current = null;
          setWsState("closed");
          onStatusChange({ tone: "error", text: "Gateway connection failed" });
          return;
        }

        if (data.type === "event" && data.event === "connect.challenge") {
          const connectReq: WsReq = {
            type: "req",
            id: "connect",
            method: "connect",
            params: {
              minProtocol: 1,
              maxProtocol: 1,
              authToken: authTokenRef.current,
            },
          };
          socket.send(JSON.stringify(connectReq));
          return;
        }

        if (data.type === "res" && data.id === "connect" && data.ok) {
          setWsState("open");
          onStatusChange({ tone: "ok", text: "Gateway connected" });
          connectPromise.current = null;
          connectResolve.current = null;
          connectReject.current = null;
          resolve();
          return;
        }

        if (data.type === "res" && data.id === "connect" && !data.ok) {
          if (data.error?.code === "unauthorized") {
            const message = data.error?.message || "Authentication required";
            onAuthRequired(message);
            connectReject.current?.(new AuthRequiredError(message));
          } else {
            connectReject.current?.(
              new Error(data.error?.message || "connect rejected"),
            );
          }
          connectPromise.current = null;
          connectResolve.current = null;
          connectReject.current = null;
          setWsState("closed");
          onStatusChange({ tone: "error", text: "Gateway connection failed" });
        }
      });

      socket.addEventListener("close", () => {
        setWsState("closed");
        connectPromise.current = null;
        connectResolve.current = null;
        connectReject.current = null;
        socketRef.current = null;
      });

      socket.addEventListener("error", () => {
        setWsState("closed");
        onStatusChange({ tone: "error", text: "Gateway connection failed" });
        connectPromise.current = null;
        connectResolve.current = null;
        connectReject.current = null;
        reject(new Error("websocket error"));
      });
    });

    return connectPromise.current;
  }

  return { wsState, connect };
}
