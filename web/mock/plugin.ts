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
    label: "web:web-chat:agent:lyre",
    chat_id: 1,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-03T10:30:00.000Z",
    last_message_preview: "Can you help me refactor the sidebar styles?",
  },
  {
    session_key: "dev-discussion",
    label: "discord:dev-discussion:agent:lyre",
    chat_id: 2,
    channel: "discord",
    agent_id: "lyre",
    last_message_time: "2026-07-03T09:15:00.000Z",
    last_message_preview: "design discussion about the new card colors…",
  },
  {
    session_key: "morning-notes",
    label: "cli:morning-notes:agent:lyre",
    chat_id: 3,
    channel: "cli",
    agent_id: "lyre",
    last_message_time: "2026-07-02T22:00:00.000Z",
    last_message_preview: "昨日の振り返りをまとめた",
  },
  {
    session_key: "quick-test",
    label: "tui:quick-test:agent:lyre",
    chat_id: 4,
    channel: "tui",
    agent_id: "lyre",
    last_message_time: "2026-07-02T18:45:00.000Z",
    last_message_preview: "proto 動かない…",
  },
  {
    session_key: "lyre-scrollbar",
    label: "web:lyre-scrollbar:agent:lyre",
    chat_id: 50,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-04T09:48:00.000Z",
    last_message_preview: "custom webkit scrollbar with themed thumb…",
  },
  {
    session_key: "lyre-hover-gap",
    label: "web:lyre-hover-gap:agent:lyre",
    chat_id: 49,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-04T09:20:00.000Z",
    last_message_preview: "added padding so lifted cards are not clipped",
  },
  {
    session_key: "lyre-release-notes",
    label: "discord:lyre-release-notes:agent:lyre",
    chat_id: 48,
    channel: "discord",
    agent_id: "lyre",
    last_message_time: "2026-07-04T08:30:00.000Z",
    last_message_preview: "v0.2 のリリースノートを整理中",
  },
  {
    session_key: "lyre-bug-231",
    label: "cli:lyre-bug-231:agent:lyre",
    chat_id: 47,
    channel: "cli",
    agent_id: "lyre",
    last_message_time: "2026-07-04T02:10:00.000Z",
    last_message_preview: "session filter resets on agent switch",
  },
  {
    session_key: "lyre-migration",
    label: "web:lyre-migration:agent:lyre",
    chat_id: 46,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-03T23:55:00.000Z",
    last_message_preview: "add index on sleep_runs.started_at",
  },
  {
    session_key: "lyre-i18n",
    label: "tui:lyre-i18n:agent:lyre",
    chat_id: 45,
    channel: "tui",
    agent_id: "lyre",
    last_message_time: "2026-07-03T20:05:00.000Z",
    last_message_preview: "extract user-facing strings into locale file",
  },
  {
    session_key: "lyre-perf",
    label: "web:lyre-perf:agent:lyre",
    chat_id: 44,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-03T16:40:00.000Z",
    last_message_preview: "virtualize long message lists",
  },
  {
    session_key: "lyre-docs",
    label: "cli:lyre-docs:agent:lyre",
    chat_id: 43,
    channel: "cli",
    agent_id: "lyre",
    last_message_time: "2026-07-03T14:12:00.000Z",
    last_message_preview: "document web channel auth flow",
  },
  {
    session_key: "lyre-standup",
    label: "telegram:lyre-standup:agent:lyre",
    chat_id: 42,
    channel: "telegram",
    agent_id: "lyre",
    last_message_time: "2026-07-03T08:00:00.000Z",
    last_message_preview: "今日のタスクを共有",
  },
  {
    session_key: "vega-design",
    label: "web:vega-design:agent:vega",
    chat_id: 60,
    channel: "web",
    agent_id: "vega",
    last_message_time: "2026-07-04T07:15:00.000Z",
    last_message_preview: "card spacing and hover affordance",
  },
  {
    session_key: "orion-infra",
    label: "discord:orion-infra:agent:orion",
    chat_id: 70,
    channel: "discord",
    agent_id: "orion",
    last_message_time: "2026-07-03T19:30:00.000Z",
    last_message_preview: "remove unused CI workflows",
  },
  {
    session_key: "ace-research",
    label: "web:ace-research:agent:ace",
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
  {
    id: "m5",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "スクロールの追従も確認したい。メッセージが増えたときに下に固定され続けるか見てくれますか",
    timestamp: "2026-07-03T10:31:00.000Z",
    message_kind: "text",
  },
  {
    id: "m6",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "はい。タイムラインは ResizeObserver でメッセージ領域の高さ変化を監視し、ユーザーが最下部にいるときだけ自動スクロールします。上にスクロールして過去ログを読んでいるときは追従を止めて、Jump to latest ボタンで戻れるようにしてあります。ストリーミング中の 1 メッセージ成長にも追従します。",
    timestamp: "2026-07-03T10:32:00.000Z",
    message_kind: "text",
  },
  {
    id: "m-tool-3",
    sender_id: "lyre",
    sender_kind: "tool" as const,
    content: JSON.stringify({
      tool: "read",
      status: "success",
      result: "timeline auto-follow uses a ResizeObserver pinned to messagesRef",
      input: { path: "web/src/features/chat/Timeline.tsx" },
      duration_ms: 73,
    }),
    timestamp: "2026-07-03T10:32:30.000Z",
    message_kind: "tool_result",
  },
  {
    id: "m7",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "検索はどこから開けますか",
    timestamp: "2026-07-03T10:34:00.000Z",
    message_kind: "text",
  },
  {
    id: "m8",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "チャットヘッダの右端にある虫眼鏡ボタンから開けます。Enter / Shift+Enter で前後のマッチに移動し、Esc または ✕ で閉じます。マッチしたメッセージへ自動スクロールして一時的にハイライトします。",
    timestamp: "2026-07-03T10:35:00.000Z",
    message_kind: "text",
  },
  {
    id: "m9",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "モバイルだとタブが見切れるんですけど",
    timestamp: "2026-07-03T10:37:00.000Z",
    message_kind: "text",
  },
  {
    id: "m10",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "モバイル幅ではタブストリップをセレクトボックスに切り替えています。そのため Chat / Sleep が確実に選べます。disabled なタブは選択不可の option として表示されます。",
    timestamp: "2026-07-03T10:38:00.000Z",
    message_kind: "text",
  },
  {
    id: "m-tool-4",
    sender_id: "lyre",
    sender_kind: "tool" as const,
    content: JSON.stringify({
      tool: "edit",
      status: "success",
      result: "replaced mobile tab strip with a native select dropdown",
      input: { path: "web/src/app/shell/TopBar.tsx" },
      duration_ms: 118,
    }),
    timestamp: "2026-07-03T10:38:30.000Z",
    message_kind: "tool_result",
  },
  {
    id: "m11",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "フェッチエラー時の表示は?",
    timestamp: "2026-07-03T10:40:00.000Z",
    message_kind: "text",
  },
  {
    id: "m12",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "認証エラーはモーダル、それ以外の取得エラーは右上のトーストで通知します。自動で消えるか ✕ で閉じられます。",
    timestamp: "2026-07-03T10:41:00.000Z",
    message_kind: "text",
  },
  {
    id: "m13",
    sender_id: "user",
    sender_kind: "user" as const,
    content:
      "最後に、長文メッセージの折り返しを確認させてください。この文章は意図的に長く書いており、バブルの最大幅に達したときに適切に折り返され、はみ出さず、スクロールも妨げないことを確認したいです。",
    timestamp: "2026-07-03T10:43:00.000Z",
    message_kind: "text",
  },
  {
    id: "m14",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "確認ありがとうございます。以下の点をご確認ください:\n- バブルは上限幅で折り返される\n- 長文でもレイアウトが崩れない\n- 自動スクロールが妨げられない\n\n何かあればすぐ直します。",
    timestamp: "2026-07-03T10:44:00.000Z",
    message_kind: "text",
  },
  {
    id: "m15",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "完璧です。この会話でスクロールのテストもできそうです。",
    timestamp: "2026-07-03T10:45:00.000Z",
    message_kind: "text",
  },
  {
    id: "m16",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "はい、この会話自体がスクロール確認用のボリュームになっています。セッションを切り替えると、このモックではセッションごとに異なる履歴を生成して返すようにしてあります。",
    timestamp: "2026-07-03T10:46:00.000Z",
    message_kind: "text",
  },
];

const SAMPLE_TOPICS = [
  "the sidebar layout",
  "agent card styling",
  "session list overflow",
  "scroll follow behavior",
  "the command palette",
  "mock data volume",
  "dark theme contrast",
  "mobile responsive tabs",
];

function hashSeed(value: string): number {
  let hash = 7;
  for (let i = 0; i < value.length; i++) {
    hash = (hash * 31 + value.charCodeAt(i)) | 0;
  }
  return Math.abs(hash);
}

// Builds a deterministic, distinct conversation for a session so the mock can
// surface different content per session (the real backend serves real history).
function historyFor(sessionKey: string) {
  const seed = hashSeed(sessionKey);
  const count = 8 + (seed % 14);
  const start = Date.parse("2026-07-04T08:00:00.000Z");
  const messages = [];
  for (let i = 0; i < count; i++) {
    const isUser = i % 2 === 0;
    const topic = SAMPLE_TOPICS[(seed + i) % SAMPLE_TOPICS.length];
    const timestamp = new Date(start + (i + 1) * 90_000).toISOString();
    messages.push({
      id: `${sessionKey}-m${i}`,
      sender_id: isUser ? "user" : "lyre",
      sender_kind: isUser ? "user" : "assistant",
      content: isUser
        ? `${sessionKey} で ${topic} を確認したい。これは ${i + 1} 件目の発言で、スクロールや折り返しの挙動を見るために少し長めの文章にしてあります。`
        : `${topic} について (${sessionKey})。アシスタントからの返信 ${i + 1} 件目です。メッセージ幅が内容に追随するか、長文のときに上限で折り返されるかを確認できます。`,
      timestamp,
      message_kind: "text",
    });
  }
  return messages;
}

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

        const parsed = new URL(url, "http://localhost");
        const { pathname } = parsed;

        if (pathname === "/api/agents") {
          return sendJson(res, { ok: true, agents: AGENTS });
        }
        if (pathname === "/api/sessions") {
          return sendJson(res, { ok: true, sessions: SESSIONS });
        }
        if (pathname === "/api/history") {
          const sessionKey = parsed.searchParams.get("session_key") ?? "";
          const messages =
            sessionKey === "web-chat" || sessionKey === ""
              ? MESSAGES
              : historyFor(sessionKey);
          return sendJson(res, { ok: true, messages });
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
