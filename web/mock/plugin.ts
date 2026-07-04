import type { ServerResponse } from "node:http";
import type { Plugin } from "vite";

const AGENTS = [
  { id: "lyre", label: "Lyre", is_default: true, active: false },
  { id: "ace", label: "Ace", is_default: false, active: true },
  { id: "vega", label: "Vega", is_default: false, active: false },
  { id: "orion", label: "Orion", is_default: false, active: true },
  { id: "nova", label: "Nova", is_default: false, active: false },
  { id: "draco", label: "Draco", is_default: false, active: true },
  { id: "pegasus", label: "Pegasus", is_default: false, active: false },
  { id: "andromeda", label: "Andromeda", is_default: false, active: false },
  { id: "phoenix", label: "Phoenix", is_default: false, active: true },
  { id: "hydra", label: "Hydra", is_default: false, active: false },
  { id: "cassiopeia", label: "Cassiopeia", is_default: false, active: false },
  { id: "perseus", label: "Perseus", is_default: false, active: false },
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
    session_key: "lyre-scrollbar",
    label: "Scrollbar polish",
    chat_id: 50,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-04T09:48:00.000Z",
    last_message_preview: "custom webkit scrollbar with themed thumb…",
  },
  {
    session_key: "lyre-hover-gap",
    label: "Hover clipping fix",
    chat_id: 49,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-04T09:20:00.000Z",
    last_message_preview: "added padding so lifted cards are not clipped",
  },
  {
    session_key: "lyre-release-notes",
    label: "Release notes draft",
    chat_id: 48,
    channel: "discord",
    agent_id: "lyre",
    last_message_time: "2026-07-04T08:30:00.000Z",
    last_message_preview: "v0.2 のリリースノートを整理中",
  },
  {
    session_key: "lyre-bug-231",
    label: "Bug #231",
    chat_id: 47,
    channel: "cli",
    agent_id: "lyre",
    last_message_time: "2026-07-04T02:10:00.000Z",
    last_message_preview: "session filter resets on agent switch",
  },
  {
    session_key: "lyre-migration",
    label: "DB migration plan",
    chat_id: 46,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-03T23:55:00.000Z",
    last_message_preview: "add index on sleep_runs.started_at",
  },
  {
    session_key: "lyre-i18n",
    label: "i18n strings",
    chat_id: 45,
    channel: "tui",
    agent_id: "lyre",
    last_message_time: "2026-07-03T20:05:00.000Z",
    last_message_preview: "extract user-facing strings into locale file",
  },
  {
    session_key: "lyre-perf",
    label: "Timeline perf",
    chat_id: 44,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-03T16:40:00.000Z",
    last_message_preview: "virtualize long message lists",
  },
  {
    session_key: "lyre-docs",
    label: "Docs: channels",
    chat_id: 43,
    channel: "cli",
    agent_id: "lyre",
    last_message_time: "2026-07-03T14:12:00.000Z",
    last_message_preview: "document web channel auth flow",
  },
  {
    session_key: "lyre-standup",
    label: "Standup notes",
    chat_id: 42,
    channel: "telegram",
    agent_id: "lyre",
    last_message_time: "2026-07-03T08:00:00.000Z",
    last_message_preview: "今日のタスクを共有",
  },
  {
    session_key: "vega-design",
    label: "Design review",
    chat_id: 60,
    channel: "web",
    agent_id: "vega",
    last_message_time: "2026-07-04T07:15:00.000Z",
    last_message_preview: "card spacing and hover affordance",
  },
  {
    session_key: "orion-infra",
    label: "Infra cleanup",
    chat_id: 70,
    channel: "discord",
    agent_id: "orion",
    last_message_time: "2026-07-03T19:30:00.000Z",
    last_message_preview: "remove unused CI workflows",
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
    id: "m-tool-1",
    sender_id: "lyre",
    sender_kind: "tool" as const,
    content: JSON.stringify({
      tool: "read",
      status: "success",
      result:
        "  .agent-row.active {\n    background: var(--color-accent-soft);\n    border-color: var(--color-border-strong);\n  }",
      input: { path: "web/src/styles/app.css" },
      duration_ms: 87,
    }),
    timestamp: "2026-07-03T10:27:30.000Z",
    message_kind: "tool_result",
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
    id: "m-tool-2",
    sender_id: "lyre",
    sender_kind: "tool" as const,
    content: JSON.stringify({
      tool: "edit",
      status: "success",
      result: "replaced 5 occurrences of accent-soft with panel-2 and accent-2 color-mix",
      input: { path: "web/src/styles/app.css", old: "accent-soft", new: "panel-2" },
      duration_ms: 142,
    }),
    timestamp: "2026-07-03T10:29:00.000Z",
    message_kind: "tool_result",
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
  if (process.env.VITE_MOCK !== "1") return null;

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
