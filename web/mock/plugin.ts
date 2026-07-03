import type { ServerResponse } from "node:http";
import type { Plugin } from "vite";

const AGENTS = [
  { id: "lyre", label: "Lyre", is_default: true, active: false },
  { id: "ace", label: "Ace", is_default: false, active: true },
  { id: "vega", label: "Vega", is_default: false, active: false },
];

const SESSIONS = [
  {
    session_key: "web-chat",
    label: "Web Chat",
    chat_id: 1,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-03T10:30:00.000Z",
    last_message_preview: "Can you help me refactor the sidebar styles?",
  },
  {
    session_key: "dev-discussion",
    label: "Dev #general",
    chat_id: 2,
    channel: "discord",
    agent_id: "lyre",
    last_message_time: "2026-07-03T09:15:00.000Z",
    last_message_preview: "design discussion about the new card colors…",
  },
  {
    session_key: "morning-notes",
    label: "Morning notes",
    chat_id: 3,
    channel: "cli",
    agent_id: "lyre",
    last_message_time: "2026-07-02T22:00:00.000Z",
    last_message_preview: "昨日の振り返りをまとめた",
  },
  {
    session_key: "quick-test",
    label: "Quick test",
    chat_id: 4,
    channel: "tui",
    agent_id: "lyre",
    last_message_time: "2026-07-02T18:45:00.000Z",
    last_message_preview: "proto 動かない…",
  },
  {
    session_key: "ace-research",
    label: "Research",
    chat_id: 5,
    channel: "web",
    agent_id: "ace",
    last_message_time: "2026-07-03T11:00:00.000Z",
    last_message_preview: "Tailwind v4 color-mix patterns investigated",
  },
];

const MESSAGES = [
  {
    id: "m1",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "サイドバーの色が深緑っぽくなっているのを直したい",
    timestamp: "2026-07-03T10:25:00.000Z",
    message_kind: "text",
  },
  {
    id: "m2",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "mockup を確認すると、選択状態は紫 (accent-2) を使うべきところがシアン (accent) になっていますね。",
    timestamp: "2026-07-03T10:27:00.000Z",
    message_kind: "text",
  },
  {
    id: "m3",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "お願いします",
    timestamp: "2026-07-03T10:28:00.000Z",
    message_kind: "text",
  },
  {
    id: "m4",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content: "accent-soft を panel-2 と紫 tint に置換しました。深緑は消滅します。",
    timestamp: "2026-07-03T10:30:00.000Z",
    message_kind: "text",
  },
];

export function mockApiPlugin(): Plugin | null {
  if (process.env.EGOPULSE_MOCK !== "1") return null;

  return {
    name: "egopulse-mock-api",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url ?? "";
        if (!url.startsWith("/api")) {
          next();
          return;
        }

        const { pathname } = new URL(url, "http://localhost");

        if (pathname === "/api/agents") {
          return sendJson(res, { ok: true, agents: AGENTS });
        }
        if (pathname === "/api/sessions") {
          return sendJson(res, { ok: true, sessions: SESSIONS });
        }
        if (pathname === "/api/history") {
          return sendJson(res, { ok: true, messages: MESSAGES });
        }
        if (pathname === "/api/sleep/runs") {
          return sendJson(res, { ok: true, runs: [] });
        }

        next();
      });
    },
  };
}

function sendJson(res: ServerResponse, data: unknown): void {
  res.setHeader("Content-Type", "application/json");
  res.end(JSON.stringify(data));
}
