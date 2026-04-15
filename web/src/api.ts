import type { StreamEvent, UiStatus } from "./types";

/** localStorage に auth token を保存する際のキー */
export const AUTH_TOKEN_STORAGE_KEY = "egopulse.webAuthToken";

/** 認証が必要な場合に投げられるエラー */
export class AuthRequiredError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AuthRequiredError";
  }
}

/** localStorage から auth token を読み出す */
export function loadAuthToken(): string {
  return window.localStorage.getItem(AUTH_TOKEN_STORAGE_KEY) || "";
}

/** auth token を localStorage に保存または削除する */
export function persistAuthToken(token: string): void {
  const trimmed = token.trim();
  if (trimmed) {
    window.localStorage.setItem(AUTH_TOKEN_STORAGE_KEY, trimmed);
  } else {
    window.localStorage.removeItem(AUTH_TOKEN_STORAGE_KEY);
  }
}

/**
 * 認証ヘッダー付きの汎用 fetch ラッパー。
 * 401 → AuthRequiredError, ネットワークエラー → Error, HTTP エラー → Error
 */
export function api<T>(
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

/** 現在のホストに基づいて WebSocket URL を生成する */
export function wsUrl(): string {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/ws`;
}

/** 現在時刻を ISO 文字列で返す */
export function nowIso(): string {
  return new Date().toISOString();
}

/** 現在時刻から "session-YYYYMMDDHHmmss" 形式のキーを生成する */
export function sessionKeyNow(): string {
  const d = new Date();
  const pad = (v: number) => String(v).padStart(2, "0");
  return `session-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
}

/** runId から下書きメッセージ ID を構築する */
export function buildDraftId(runId: string): string {
  return `draft:${runId}`;
}

let nextLocalId = 0;

/** ユニークなローカル ID を生成する */
export function makeId(prefix: string): string {
  nextLocalId += 1;
  return `${prefix}:${Date.now().toString(36)}:${nextLocalId.toString(36)}`;
}

/** SSE ストリームをパースして StreamEvent を順次 yield する非同期ジェネレーター */
export async function* parseSseFrames(
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

/** Web チャネル向けに session key を正規化する */
export function webSessionKey(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "main";
  if (trimmed.startsWith("web:")) return trimmed.slice(4).trim() || "main";
  return trimmed;
}

/** Web セッション用の外部 chat ID を構築する */
export function webExternalChatId(sessionKey: string): string {
  return `web:${sessionKey}`;
}

/** デフォルト UI ステータス */
export const defaultStatus: UiStatus = { tone: "idle", text: "Ready" };
