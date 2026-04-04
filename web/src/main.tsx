import React, { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

type SessionItem = {
  session_key: string;
  label: string;
  chat_id: number;
  channel: string;
  last_message_time?: string;
  last_message_preview?: string | null;
};

type MessageItem = {
  id: string;
  sender_name: string;
  content: string;
  is_from_bot: boolean;
  timestamp: string;
};

type ConfigPayload = {
  model: string;
  base_url: string;
  data_dir: string;
  web_enabled: boolean;
  web_host: string;
  web_port: number;
  web_auth_enabled: boolean;
  has_api_key: boolean;
  config_path: string;
  requires_restart: boolean;
};

type HealthPayload = {
  version?: string;
};

type StreamEvent = {
  event: string;
  payload: Record<string, unknown>;
};

type WsReq = {
  type: "req";
  id: string;
  method: string;
  params: Record<string, unknown>;
};

type WsRes = {
  type: "res";
  id: string;
  ok: boolean;
  payload?: Record<string, unknown>;
  error?: { code?: string; message?: string };
};

type WsEvent = {
  type: "event";
  event: string;
  payload?: Record<string, unknown>;
};

type UiStatus = {
  tone: "idle" | "ok" | "error";
  text: string;
};

const defaultStatus: UiStatus = { tone: "idle", text: "Ready" };
const AUTH_TOKEN_STORAGE_KEY = "egopulse.webAuthToken";

class AuthRequiredError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AuthRequiredError";
  }
}

function loadAuthToken(): string {
  return window.localStorage.getItem(AUTH_TOKEN_STORAGE_KEY) || "";
}

function persistAuthToken(token: string) {
  const trimmed = token.trim();
  if (trimmed) {
    window.localStorage.setItem(AUTH_TOKEN_STORAGE_KEY, trimmed);
  } else {
    window.localStorage.removeItem(AUTH_TOKEN_STORAGE_KEY);
  }
}

function api<T>(
  path: string,
  authToken: string,
  options?: RequestInit,
): Promise<T> {
  const trimmedToken = authToken.trim();
  return fetch(path, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      ...(trimmedToken ? { Authorization: `Bearer ${trimmedToken}` } : {}),
      ...(options?.headers || {}),
    },
  })
    .catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(`Network error: ${message}`);
    })
    .then(async (res) => {
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        const payload = data as { error?: string; message?: string };
        if (res.status === 401) {
          throw new AuthRequiredError(
            String(payload.message || payload.error || "Authentication required"),
          );
        }
        throw new Error(
          String(payload.error || payload.message || `HTTP ${res.status}`),
        );
      }
      return data as T;
    });
}

function wsUrl(): string {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/ws`;
}

function nowIso(): string {
  return new Date().toISOString();
}

function sessionKeyNow(): string {
  const d = new Date();
  const pad = (v: number) => String(v).padStart(2, "0");
  return `session-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
}

function buildDraftId(runId: string): string {
  return `draft:${runId}`;
}

let nextLocalId = 0;

function makeId(prefix: string): string {
  nextLocalId += 1;
  return `${prefix}:${Date.now().toString(36)}:${nextLocalId.toString(36)}`;
}

async function* parseSseFrames(
  response: Response,
  signal: AbortSignal,
): AsyncGenerator<StreamEvent, void> {
  if (!response.body) {
    throw new Error("empty stream body");
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  let eventName = "message";
  let dataLines: string[] = [];

  const flush = (): StreamEvent | null => {
    if (dataLines.length === 0) return null;
    const raw = dataLines.join("\n");
    dataLines = [];

    let payload: Record<string, unknown> = {};
    try {
      payload = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      payload = { raw };
    }

    const event: StreamEvent = { event: eventName, payload };
    eventName = "message";
    return event;
  };

  const handleLine = (line: string): StreamEvent | null => {
    if (line === "") return flush();
    if (line.startsWith(":")) return null;

    const sep = line.indexOf(":");
    const field = sep >= 0 ? line.slice(0, sep) : line;
    let value = sep >= 0 ? line.slice(sep + 1) : "";
    if (value.startsWith(" ")) value = value.slice(1);

    if (field === "event") eventName = value;
    if (field === "data") dataLines.push(value);

    return null;
  };

  while (true) {
    if (signal.aborted) return;

    const { done, value } = await reader.read();
    pending += decoder.decode(value || new Uint8Array(), { stream: !done });

    while (true) {
      const idx = pending.indexOf("\n");
      if (idx < 0) break;
      let line = pending.slice(0, idx);
      pending = pending.slice(idx + 1);
      if (line.endsWith("\r")) line = line.slice(0, -1);
      const event = handleLine(line);
      if (event) yield event;
    }

    if (done) {
      if (pending.length > 0) {
        let line = pending;
        if (line.endsWith("\r")) line = line.slice(0, -1);
        const event = handleLine(line);
        if (event) yield event;
      }
      const event = flush();
      if (event) yield event;
      return;
    }
  }
}

function App() {
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [selectedSession, setSelectedSession] = useState<string>("");
  const [messages, setMessages] = useState<MessageItem[]>([]);
  const [draft, setDraft] = useState("");
  const [config, setConfig] = useState<ConfigPayload | null>(null);
  const [configApiKey, setConfigApiKey] = useState("");
  const [authToken, setAuthToken] = useState(() => loadAuthToken());
  const [authDraft, setAuthDraft] = useState(() => loadAuthToken());
  const [showAuth, setShowAuth] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [health, setHealth] = useState<HealthPayload>({});
  const [status, setStatus] = useState<UiStatus>(defaultStatus);
  const [wsState, setWsState] = useState<"connecting" | "open" | "closed">(
    "connecting",
  );
  const socketRef = useRef<WebSocket | null>(null);
  const connectPromise = useRef<Promise<void> | null>(null);
  const connectResolve = useRef<(() => void) | null>(null);
  const connectReject = useRef<((error: Error) => void) | null>(null);
  const sendAbortRef = useRef<AbortController | null>(null);
  const messageEndRef = useRef<HTMLDivElement | null>(null);
  const selectedSessionRef = useRef("");
  const authTokenRef = useRef(authToken);

  const selectedLabel = useMemo(() => {
    return (
      sessions.find((item) => item.session_key === selectedSession)?.label ||
      selectedSession
    );
  }, [selectedSession, sessions]);

  useEffect(() => {
    messageEndRef.current?.scrollIntoView({ block: "end" });
  }, [messages]);

  useEffect(() => {
    selectedSessionRef.current = selectedSession;
  }, [selectedSession]);

  useEffect(() => {
    authTokenRef.current = authToken;
  }, [authToken]);

  useEffect(() => {
    void withAuthHandling(async () => {
      await refreshHealth();
      await refreshConfig();
      await refreshSessions();
      await connectGateway();
    });
    return () => {
      socketRef.current?.close();
      sendAbortRef.current?.abort();
    };
  }, []);

  async function withAuthHandling(action: () => Promise<void>) {
    try {
      await action();
    } catch (error) {
      if (error instanceof AuthRequiredError) {
        setShowAuth(true);
        setWsState("closed");
        setStatus({ tone: "error", text: error.message });
        return;
      }
      throw error;
    }
  }

  async function refreshHealth() {
    const payload = await api<{ ok: boolean; version: string }>(
      "/api/health",
      authTokenRef.current,
    );
    setHealth({ version: payload.version });
  }

  async function refreshConfig() {
    const payload = await api<{ ok: boolean; config: ConfigPayload }>(
      "/api/config",
      authTokenRef.current,
    );
    setConfig(payload.config);
    setConfigApiKey("");
  }

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

  async function connectGateway() {
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
          setStatus({ tone: "error", text: "Gateway connection failed" });
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
          setStatus((current) =>
            current.tone === "error"
              ? current
              : { tone: "ok", text: "Gateway connected" },
          );
          connectPromise.current = null;
          connectResolve.current = null;
          connectReject.current = null;
          resolve();
          return;
        }

        if (data.type === "res" && data.id === "connect" && !data.ok) {
          if (data.error?.code === "unauthorized") {
            setShowAuth(true);
            connectReject.current?.(
              new AuthRequiredError(
                data.error?.message || "Authentication required",
              ),
            );
          } else {
            connectReject.current?.(
              new Error(data.error?.message || "connect rejected"),
            );
          }
          connectPromise.current = null;
          connectResolve.current = null;
          connectReject.current = null;
          setWsState("closed");
          setStatus({ tone: "error", text: "Gateway connection failed" });
          return;
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
        setStatus((current) =>
          current.tone === "error"
            ? current
            : { tone: "error", text: "Gateway connection failed" },
        );
        connectPromise.current = null;
        connectResolve.current = null;
        connectReject.current = null;
        reject(new Error("websocket error"));
      });
    });

    return connectPromise.current;
  }

  async function handleNewSession() {
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
        body: JSON.stringify({
          session_key: sessionKey,
          message: text,
        }),
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

        if (streamEvent.event === "replay_meta") {
          continue;
        }

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
                item.id === draftId ? { ...item, id: `${draftId}:done` } : item,
              ),
            );
          }
          setStatus({ tone: "ok", text: "Response received" });
          break;
        }
      }
    } catch (error) {
      if (error instanceof AuthRequiredError) {
        setShowAuth(true);
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

  async function handleSaveAuthToken(event: FormEvent) {
    event.preventDefault();
    persistAuthToken(authDraft);
    setAuthToken(authDraft.trim());
    setShowAuth(false);
    socketRef.current?.close();
    await withAuthHandling(async () => {
      await refreshHealth();
      await refreshConfig();
      await refreshSessions();
      await connectGateway();
    });
  }

  async function handleSaveConfig(event: FormEvent) {
    event.preventDefault();
    if (!config) return;

    const payload = {
      model: config.model,
      base_url: config.base_url,
      data_dir: config.data_dir,
      web_enabled: config.web_enabled,
      web_host: config.web_host,
      web_port: config.web_port,
      api_key: configApiKey,
    };

    try {
      const response = await api<{
        ok: boolean;
        config: ConfigPayload;
        requires_restart: boolean;
      }>("/api/config", authTokenRef.current, {
        method: "PUT",
        body: JSON.stringify(payload),
      });
      setConfig(response.config);
      setConfigApiKey("");
      setStatus({
        tone: "ok",
        text: response.requires_restart
          ? "Config saved. Restart required for runtime changes."
          : "Config saved.",
      });
      setShowSettings(false);
    } catch (error) {
      if (error instanceof AuthRequiredError) {
        setShowAuth(true);
      }
      setStatus({
        tone: "error",
        text: error instanceof Error ? error.message : "Failed to save config",
      });
    }
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <img src="/icon.png" alt="EgoPulse" />
          <div>
            <h1>EgoPulse</h1>
            <p>{health.version ? `v${health.version}` : "Web"}</p>
          </div>
        </div>

        <button
          className="primary-button"
          onClick={() => void handleNewSession()}
        >
          New Session
        </button>
        <button
          className="secondary-button"
          onClick={() => setShowSettings(true)}
        >
          Runtime Config
        </button>

        <div className="sidebar-section">
          <div className="sidebar-title-row">
            <h2>Sessions</h2>
            <span>{sessions.length}</span>
          </div>
          <div className="session-list">
            {sessions.map((item) => (
              <button
                key={item.session_key}
                className={
                  item.session_key === selectedSession
                    ? "session-item active"
                    : "session-item"
                }
                onClick={() => {
                  selectedSessionRef.current = item.session_key;
                  setSelectedSession(item.session_key);
                  void loadHistory(item.session_key);
                }}
              >
                <strong>{item.label}</strong>
                <small>{item.last_message_preview || "No messages yet"}</small>
              </button>
            ))}
          </div>
        </div>
      </aside>

      <main className="main-panel">
        <header className="chat-header">
          <div>
            <h2>{selectedLabel || "Select a session"}</h2>
            <p>
              Gateway: {wsState}
              {config?.web_auth_enabled ? " / auth enabled" : ""}
            </p>
          </div>
          <div className={`status-badge ${status.tone}`}>{status.text}</div>
        </header>

        <section className="timeline">
          {messages.map((message) => (
            <article
              key={message.id}
              className={
                message.is_from_bot ? "bubble bubble-bot" : "bubble bubble-user"
              }
            >
              <div className="bubble-meta">
                <span>{message.sender_name}</span>
                <time>{new Date(message.timestamp).toLocaleTimeString()}</time>
              </div>
              <pre>{message.content}</pre>
            </article>
          ))}
          <div ref={messageEndRef} />
        </section>

        <form className="composer" onSubmit={handleSend}>
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (
                event.key === "Enter" &&
                (event.ctrlKey || event.metaKey)
              ) {
                event.preventDefault();
                handleSend(event as unknown as FormEvent);
              }
            }}
            placeholder="Type a message"
            rows={3}
          />
          <button className="primary-button" type="submit">
            Send
          </button>
        </form>
      </main>

      {showSettings && config ? (
        <div
          className="modal-backdrop"
          onClick={() => setShowSettings(false)}
          onKeyDown={(event) => {
            if (event.key === "Escape") setShowSettings(false);
          }}
          role="presentation"
        >
          <div
            className="modal-card"
            role="dialog"
            aria-modal="true"
            aria-labelledby="modal-title"
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => {
              if (event.key === "Escape") setShowSettings(false);
            }}
          >
            <div className="modal-header">
              <div>
                <h3 id="modal-title">Runtime Config</h3>
                <p>{config.config_path}</p>
              </div>
              <button
                className="icon-button"
                onClick={() => setShowSettings(false)}
                aria-label="Close modal"
              >
                ×
              </button>
            </div>

            <form className="config-form" onSubmit={handleSaveConfig}>
              <label>
                <span>Model</span>
                <input
                  value={config.model}
                  onChange={(event) =>
                    setConfig({ ...config, model: event.target.value })
                  }
                />
              </label>
              <label>
                <span>Base URL</span>
                <input
                  value={config.base_url}
                  onChange={(event) =>
                    setConfig({ ...config, base_url: event.target.value })
                  }
                />
              </label>
              <label>
                <span>API Key</span>
                <input
                  type="password"
                  value={configApiKey}
                  placeholder={
                    config.has_api_key
                      ? "Configured. Enter to replace."
                      : "Enter API key"
                  }
                  onChange={(event) => setConfigApiKey(event.target.value)}
                />
              </label>
              <label>
                <span>Data Dir</span>
                <input
                  value={config.data_dir}
                  onChange={(event) =>
                    setConfig({ ...config, data_dir: event.target.value })
                  }
                />
              </label>
              <div className="grid-two">
                <label>
                  <span>Web Host</span>
                  <input
                    value={config.web_host}
                    onChange={(event) =>
                      setConfig({ ...config, web_host: event.target.value })
                    }
                  />
                </label>
                <label>
                  <span>Web Port</span>
                  <input
                    type="number"
                    value={config.web_port}
                    onChange={(event) => {
                      const parsed = Number(event.target.value);
                      const clamped =
                        Number.isFinite(parsed) && parsed >= 1 && parsed <= 65535
                          ? Math.round(parsed)
                          : 0;
                      setConfig({
                        ...config,
                        web_port: clamped,
                      });
                    }}
                  />
                </label>
              </div>
              <label className="checkbox-row">
                <input
                  type="checkbox"
                  checked={config.web_enabled}
                  onChange={(event) =>
                    setConfig({ ...config, web_enabled: event.target.checked })
                  }
                />
                <span>Enable web channel</span>
              </label>
              <div className="config-footer">
                <span>
                  {config.requires_restart
                    ? "Changes are persisted to disk. Restart EgoPulse to apply runtime changes."
                    : ""}
                </span>
                <button className="primary-button" type="submit">
                  Save
                </button>
              </div>
            </form>
          </div>
        </div>
      ) : null}

      {showAuth ? (
        <div className="modal-backdrop" role="presentation">
          <div
            className="modal-card"
            role="dialog"
            aria-modal="true"
            aria-labelledby="auth-modal-title"
          >
            <div className="modal-header">
              <div>
                <h3 id="auth-modal-title">Web Access Token</h3>
                <p>Enter channels.web.auth_token to access EgoPulse APIs.</p>
              </div>
            </div>

            <form className="config-form" onSubmit={handleSaveAuthToken}>
              <label>
                <span>Auth Token</span>
                <input
                  type="password"
                  value={authDraft}
                  autoFocus
                  onChange={(event) => setAuthDraft(event.target.value)}
                />
              </label>
              <div className="config-footer">
                <span>Stored locally in this browser only.</span>
                <button className="primary-button" type="submit">
                  Unlock
                </button>
              </div>
            </form>
          </div>
        </div>
      ) : null}
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
