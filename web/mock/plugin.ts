import fs from "node:fs";
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

// Mirrors the agent_avatars table: in-memory uploads, with Lyre pre-seeded
// from the bundled app icon so both avatar states are visible in the UI.
const avatarStore = new Map<string, { body: Buffer; type: string }>();
const avatarVersions = new Map<string, number>();
let lyreDefaultAvatarActive = true;

let defaultIcon: Buffer | null = null;
function defaultAvatar(): Buffer {
  if (!defaultIcon) {
    defaultIcon = fs.readFileSync(new URL("../public/icon.png", import.meta.url));
  }
  return defaultIcon;
}

function agentsPayload() {
  return {
    ok: true,
    agents: AGENTS.map((agent) => {
      const uploaded = avatarStore.has(agent.id);
      const version = avatarVersions.get(agent.id);
      const avatar_url =
        uploaded || (agent.id === "lyre" && lyreDefaultAvatarActive)
          ? `/api/agents/${agent.id}/avatar?v=${uploaded ? version : "default"}`
          : null;
      return { ...agent, avatar_url };
    }),
  };
}

const SESSIONS = [
  {
    session_key: "web-chat",
    label: "web:web-chat:agent:lyre",
    chat_id: 1,
    channel: "web",
    agent_id: "lyre",
    last_message_time: "2026-07-04T10:00:00.000Z",
    last_message_preview: "マークダウン表示とツールカードのショーケース",
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

const MARKDOWN_SHOWCASE = [
  "# 見出しレベル 1",
  "",
  "マークダウンの **太字**、*イタリック*、~~取り消し線~~、`インラインコード` が確認できます。",
  "",
  "## 見出しレベル 2",
  "",
  "### 見出しレベル 3",
  "#### 見出しレベル 4",
  "",
  "## リスト",
  "",
  "番号付きリスト:",
  "1. 最初の項目",
  "2. 二番目の項目",
  "   - ネストした項目 A",
  "   - ネストした項目 B",
  "3. 三番目の項目",
  "",
  "タスクリスト (GFM):",
  "- [x] マークダウン描画",
  "- [x] コードハイライト",
  "- [ ] ツールカードの履歴マージ",
  "",
  "## 表 (GFM)",
  "",
  "| 機能 | 状態 | 備考 |",
  "|------|:----:|------|",
  "| 見出し h1〜h4 | ✅ | 余白を拡張 |",
  "| 表 | ✅ | GFM 対応 |",
  "| コードハイライト | ✅ | rehype-highlight |",
  "| 折りたたみ | ✅ | 20 行超で表示 |",
  "",
  "## 引用",
  "",
  "> これは引用です。複数行になるとブロックとして扱われます。",
  ">> ネストした引用も確認できます。",
  "",
  "## リンク",
  "",
  "[EgoPulse ドキュメント](https://example.com) のようにリンクも描画されます。",
  "",
  "---",
  "",
  "## コードブロック（シンタックスハイライト）",
  "",
  "```rust",
  "fn main() {",
  "    let greeting = \"Hello, EgoPulse\";",
  "    println!(\"{greeting}\");",
  "}",
  "```",
  "",
  "```typescript",
  "const sum = (a: number, b: number): number => a + b;",
  "console.log(`sum = ${sum(1, 2)}`);",
  "```",
  "",
  "```bash",
  "cargo build --release",
  "systemctl --user restart egopulse",
  "```",
  "",
  "## 長文コード（折りたたみ確認用: 20 行超）",
  "",
  "```typescript",
  "// 折りたたみの挙動確認用のダミー実装です。",
  "interface Token { kind: string; value: string }",
  "function tokenize(source: string): Token[] {",
  "  const tokens: Token[] = [];",
  "  let buffer = \"\";",
  "  for (const ch of source) {",
  "    if (ch === \" \") {",
  "      if (buffer) { tokens.push({ kind: \"word\", value: buffer }); buffer = \"\"; }",
  "      tokens.push({ kind: \"space\", value: ch });",
  "      continue;",
  "    }",
  "    buffer += ch;",
  "  }",
  "  if (buffer) tokens.push({ kind: \"word\", value: buffer });",
  "  return tokens;",
  "}",
  "const sample = tokenize(\"hello world from egopulse\");",
  "console.log(sample.length);",
  "console.log(sample[0]);",
  "console.log(sample[1]);",
  "console.log(sample[2]);",
  "console.log(sample[3]);",
  "console.log(\"done\");",
  "```",
].join(`
`);

const MESSAGES = [
  {
    id: "m1",
    sender_id: "user",
    sender_kind: "user" as const,
    content:
      "マークダウンの表示を一通り確認したい。見出し・表・番号付きリスト・コードハイライト・引用などを網羅した見本を出して。あわせてツールカードの見え方も確認したい。",
    timestamp: "2026-07-04T09:55:00.000Z",
    message_kind: "text",
  },
  {
    id: "m-tool-1",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content: JSON.stringify({
      tool: "read",
      status: "success",
      result:
        `  // markdown showcase scaffold
  - headings h1..h4
  - gfm tables
  - code blocks with language`,
      input: { path: "docs/markdown-showcase.md" },
      duration_ms: 64,
    }),
    timestamp: "2026-07-04T09:55:30.000Z",
    message_kind: "tool_call",
  },
  {
    id: "m2",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content: MARKDOWN_SHOWCASE,
    timestamp: "2026-07-04T09:56:00.000Z",
    message_kind: "text",
  },
  {
    id: "m3",
    sender_id: "user",
    sender_kind: "user" as const,
    content: "完璧。ツールがエラーのときはどう見える？",
    timestamp: "2026-07-04T09:58:00.000Z",
    message_kind: "text",
  },
  {
    id: "m-tool-2",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content: JSON.stringify({
      tool: "shell",
      status: "error",
      result: "command not found: rake",
      input: { command: "rake assets:precompile" },
      duration_ms: 312,
    }),
    timestamp: "2026-07-04T09:58:30.000Z",
    message_kind: "tool_call",
  },
  {
    id: "m4",
    sender_id: "lyre",
    sender_kind: "assistant" as const,
    content:
      "エラー状態のツールカードは自動的に展開され、赤い `error` バッジで表示されます（上の `shell` を参照）。成功時は所要時間バッジ付きで折りたたまれます。",
    timestamp: "2026-07-04T09:59:00.000Z",
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
  const messages: Array<Record<string, unknown>> = [];
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

// Mirrors src/channels/web/sleep.rs: field names (`trigger`, `session_count`)
// and value sets must stay in sync with the real backend.
const SLEEP_RUNS = [
  {
    id: "run-20260704-lyre-manual",
    agent_id: "lyre",
    status: "success",
    trigger: "manual",
    started_at: "2026-07-04T03:00:00.000Z",
    finished_at: "2026-07-04T03:04:12.000Z",
    source_chats_json: '["web:web-chat", "discord:dev-discussion"]',
    source_digest_md: "2 chats · 41 messages · 2026-07-03",
    input_tokens: 18240,
    output_tokens: 3120,
    total_tokens: 21360,
    error_message: null,
    session_count: 2,
  },
  {
    id: "run-20260703-lyre-scheduled",
    agent_id: "lyre",
    status: "partial_failure",
    trigger: "scheduled",
    started_at: "2026-07-03T03:00:00.000Z",
    finished_at: "2026-07-03T03:05:47.000Z",
    source_chats_json: '["web:web-chat"]',
    source_digest_md: "1 chat · 18 messages · 2026-07-02",
    input_tokens: 12480,
    output_tokens: 1960,
    total_tokens: 14440,
    error_message: "semantic update failed: rate limited, retry later",
    session_count: 1,
  },
  {
    id: "run-20260703-ace-scheduled",
    agent_id: "ace",
    status: "failed",
    trigger: "scheduled",
    started_at: "2026-07-03T03:00:00.000Z",
    finished_at: "2026-07-03T03:00:58.000Z",
    source_chats_json: '["cli:morning-notes"]',
    source_digest_md: "1 chat · 6 messages · 2026-07-02",
    input_tokens: 3200,
    output_tokens: 0,
    total_tokens: 3200,
    error_message: "event extraction failed: model timeout after 45s",
    session_count: 1,
  },
  {
    id: "run-20260704-lyre-running",
    agent_id: "lyre",
    status: "running",
    trigger: "scheduled",
    started_at: "2026-07-04T03:00:00.000Z",
    finished_at: null,
    source_chats_json: '["web:web-chat"]',
    source_digest_md: "1 chat · 9 messages · 2026-07-04",
    input_tokens: 5120,
    output_tokens: 480,
    total_tokens: 5600,
    error_message: null,
    session_count: 1,
  },
  {
    id: "run-20260702-ace-backfill",
    agent_id: "ace",
    status: "success",
    trigger: "backfill",
    started_at: "2026-07-02T12:00:00.000Z",
    finished_at: "2026-07-02T12:03:31.000Z",
    source_chats_json: '["cli:morning-notes", "web:notes"]',
    source_digest_md: "2 chats · 22 messages · 2026-07-01",
    input_tokens: 9840,
    output_tokens: 1740,
    total_tokens: 11580,
    error_message: null,
    session_count: 2,
  },
  {
    id: "run-20260702-lyre-skipped",
    agent_id: "lyre",
    status: "skipped",
    trigger: "scheduled",
    started_at: "2026-07-02T03:00:00.000Z",
    finished_at: "2026-07-02T03:00:04.000Z",
    source_chats_json: "[]",
    source_digest_md: "no new messages since last run",
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
    error_message: null,
    session_count: 0,
  },
];

const SLEEP_SNAPSHOTS: Record<string, Array<Record<string, unknown>>> = {
  "run-20260704-lyre-manual": [
    {
      id: "snap-001-episodic",
      run_id: "run-20260704-lyre-manual",
      agent_id: "lyre",
      file: "episodic",
      content_before: [
        "# Episodic memory",
        "",
        "## 2026-07-02",
        "- ace と run-cards の配色について議論。",
        "",
        "## 2026-07-03",
        "- 決定: チャットヘッダーを廃止し、全高を使う。",
      ].join("\n"),
      content_after: [
        "# Episodic memory",
        "",
        "## 2026-07-02",
        "- ace と run-cards の配色について議論。",
        "",
        "## 2026-07-03",
        "- 決定: チャットヘッダーを廃止し、全高を使う。",
        "- Markdown ショーケース確認: ツールカードの状態色を見直し。",
        "",
        "## 2026-07-04",
        "- 決定: サイドバーナビは4列ミニタブバー。",
      ].join("\n"),
      created_at: "2026-07-04T03:02:10.000Z",
    },
    {
      id: "snap-001-semantic",
      run_id: "run-20260704-lyre-manual",
      agent_id: "lyre",
      file: "semantic",
      content_before: [
        "# Semantic memory",
        "",
        "- コンパクトで装飾の少ないUIを好む。",
        "- プライマリカラー: シアン (#00d4ff)。",
      ].join("\n"),
      content_after: [
        "# Semantic memory",
        "",
        "- コンパクトで装飾の少ないUIを好む。",
        "- プライマリカラー: シアン (#00d4ff)。読みやすさ優先。",
        "- サーフェスは無彩色、アクセントはシアン/パープル。",
      ].join("\n"),
      created_at: "2026-07-04T03:03:02.000Z",
    },
    {
      id: "snap-001-prospective",
      run_id: "run-20260704-lyre-manual",
      agent_id: "lyre",
      file: "prospective",
      content_before: [
        "# Prospective memory",
        "",
        "- [ ] Sleep 画面を改修する (mock 不足)。",
      ].join("\n"),
      content_after: [
        "# Prospective memory",
        "",
        "- [x] Sleep 画面を改修する (mock 不足)。",
        "- [ ] Config タブのプレースホルダを置き換える。",
      ].join("\n"),
      created_at: "2026-07-04T03:03:44.000Z",
    },
  ],
  "run-20260703-lyre-scheduled": [
    {
      id: "snap-002-episodic",
      run_id: "run-20260703-lyre-scheduled",
      agent_id: "lyre",
      file: "episodic",
      content_before: [
        "# Episodic memory",
        "",
        "## 2026-07-02",
        "- ace と run-cards の配色について議論。",
      ].join("\n"),
      content_after: [
        "# Episodic memory",
        "",
        "## 2026-07-02",
        "- ace と run-cards の配色について議論。",
        "",
        "## 2026-07-03",
        "- 決定: チャットヘッダーを廃止し、全高を使う。",
      ].join("\n"),
      created_at: "2026-07-03T03:02:40.000Z",
    },
  ],
  "run-20260702-ace-backfill": [
    {
      id: "snap-005-semantic",
      run_id: "run-20260702-ace-backfill",
      agent_id: "ace",
      file: "semantic",
      content_before: [
        "# Semantic memory",
        "",
        "- 朝はCLIメモ、夜はレビュー。",
      ].join("\n"),
      content_after: [
        "# Semantic memory",
        "",
        "- 朝はCLIメモ、夜はレビュー。",
        "- Backfill は安定稼働。エラー率に異常なし。",
      ].join("\n"),
      created_at: "2026-07-02T12:02:15.000Z",
    },
  ],
};

// Mirrors src/channels/web/sleep.rs: step names, statuses, and response shapes
// must stay in sync with the real backend.
const SLEEP_STEPS: Record<string, Array<Record<string, unknown>>> = {
  "run-20260704-lyre-manual": [
    { step: "event_extraction", status: "success", started_at: "2026-07-04T03:00:05.000Z", finished_at: "2026-07-04T03:01:40.000Z", input_tokens: 8200, output_tokens: 640, error_message: null },
    { step: "episodic_update", status: "success", started_at: "2026-07-04T03:01:42.000Z", finished_at: "2026-07-04T03:02:30.000Z", input_tokens: 5400, output_tokens: 980, error_message: null },
    { step: "semantic_update", status: "success", started_at: "2026-07-04T03:02:32.000Z", finished_at: "2026-07-04T03:03:50.000Z", input_tokens: 4640, output_tokens: 1500, error_message: null },
  ],
  "run-20260703-lyre-scheduled": [
    { step: "event_extraction", status: "success", started_at: "2026-07-03T03:00:04.000Z", finished_at: "2026-07-03T03:01:10.000Z", input_tokens: 7100, output_tokens: 520, error_message: null },
    { step: "episodic_update", status: "success", started_at: "2026-07-03T03:01:12.000Z", finished_at: "2026-07-03T03:02:00.000Z", input_tokens: 4300, output_tokens: 860, error_message: null },
    { step: "semantic_update", status: "failed", started_at: "2026-07-03T03:02:02.000Z", finished_at: "2026-07-03T03:05:40.000Z", input_tokens: 1080, output_tokens: 580, error_message: "semantic update failed: rate limited, retry later" },
    { step: "prospective_update", status: "skipped", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
  ],
  "run-20260703-ace-scheduled": [
    { step: "event_extraction", status: "failed", started_at: "2026-07-03T03:00:06.000Z", finished_at: "2026-07-03T03:00:51.000Z", input_tokens: 3200, output_tokens: 0, error_message: "event extraction failed: model timeout after 45s" },
    { step: "episodic_update", status: "skipped", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
    { step: "semantic_update", status: "skipped", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
    { step: "prospective_update", status: "skipped", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
  ],
  "run-20260704-lyre-running": [
    { step: "event_extraction", status: "running", started_at: "2026-07-04T03:00:05.000Z", finished_at: null, input_tokens: 5120, output_tokens: 480, error_message: null },
    { step: "episodic_update", status: "pending", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
    { step: "semantic_update", status: "pending", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
    { step: "prospective_update", status: "pending", started_at: null, finished_at: null, input_tokens: 0, output_tokens: 0, error_message: null },
  ],
};

// Mirrors ~/.egopulse/agents/<id>/memory/*.md served by GET /api/agents/:id/memory.
const AGENT_MEMORY: Record<string, Record<string, string>> = {
  lyre: {
    episodic: [
      "# Episodic memory",
      "",
      "## 2026-07-04",
      "- 決定: サイドバーナビは4列ミニタブバー。",
      "- Markdown ショーケース確認: ツールカードの状態色を見直し。",
      "",
      "## 2026-07-03",
      "- 決定: チャットヘッダーを廃止し、全高を使う。",
    ].join("\n"),
    semantic: [
      "# Semantic memory",
      "",
      "- コンパクトで装飾の少ないUIを好む。",
      "- プライマリカラー: シアン (#00d4ff)。読みやすさ優先。",
      "- サーフェスは無彩色、アクセントはシアン/パープル。",
    ].join("\n"),
    prospective: [
      "# Prospective memory",
      "",
      "- [ ] Config タブのプレースホルダを置き換える。",
    ].join("\n"),
  },
  ace: {
    episodic: "",
    semantic: "# Semantic memory\n\n- 朝はCLIメモ、夜はレビュー。\n- Backfill は安定稼働。\n",
    prospective: "",
  },
};

export function mockApiPlugin(): Plugin | null {
  if (process.env.VITE_MOCK !== "1") return null;

  return {
    name: "egopulse-mock-api",
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = req.url ?? "";
        if (!url.startsWith("/api")) {
          next();
          return;
        }

        const parsed = new URL(url, "http://localhost");
        const { pathname } = parsed;

        if (pathname === "/api/agents") {
          return sendJson(res, agentsPayload());
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
          const agentId = parsed.searchParams.get("agent_id") ?? "";
          const runs = SLEEP_RUNS.filter(
            (run) => !agentId || run.agent_id === agentId,
          ).sort((a, b) => b.started_at.localeCompare(a.started_at));
          return sendJson(res, { ok: true, runs });
        }
        if (pathname.startsWith("/api/sleep/runs/")) {
          const runId = decodeURIComponent(pathname.slice("/api/sleep/runs/".length));
          const run = SLEEP_RUNS.find((candidate) => candidate.id === runId);
          if (!run) {
            res.statusCode = 404;
            return sendJson(res, { ok: false, error: "not_found" });
          }
          return sendJson(res, {
            ok: true,
            run,
            snapshots: SLEEP_SNAPSHOTS[runId] ?? [],
            steps: SLEEP_STEPS[runId] ?? [],
          });
        }
        const avatarMatch = pathname.match(/^\/api\/agents\/([^/]+)\/avatar$/);
        if (avatarMatch) {
          const agentId = decodeURIComponent(avatarMatch[1]);
          if (req.method === "PUT") {
            const chunks: Buffer[] = [];
            for await (const chunk of req) {
              chunks.push(chunk as Buffer);
            }
            const body = Buffer.concat(chunks);
            if (body.length === 0) {
              res.statusCode = 400;
              return sendJson(res, { ok: false, error: "empty_body" });
            }
            const type = req.headers["content-type"] ?? "image/png";
            avatarStore.set(agentId, { body, type });
            const version = Date.now();
            avatarVersions.set(agentId, version);
            return sendJson(res, {
              ok: true,
              avatar_url: `/api/agents/${agentId}/avatar?v=${version}`,
            });
          }
          if (req.method === "DELETE") {
            avatarStore.delete(agentId);
            avatarVersions.delete(agentId);
            if (agentId === "lyre") {
              lyreDefaultAvatarActive = false;
            }
            return sendJson(res, { ok: true });
          }
          const stored = avatarStore.get(agentId);
          if (!stored && agentId === "lyre" && lyreDefaultAvatarActive) {
            res.setHeader("Content-Type", "image/png");
            res.end(defaultAvatar());
            return;
          }
          if (!stored) {
            res.statusCode = 404;
            return sendJson(res, { ok: false, error: "not_found" });
          }
          res.setHeader("Content-Type", stored.type);
          res.end(stored.body);
          return;
        }

        const memoryMatch = pathname.match(/^\/api\/agents\/([^/]+)\/memory$/);
        if (memoryMatch) {
          const agentId = decodeURIComponent(memoryMatch[1]);
          return sendJson(res, {
            ok: true,
            memory: AGENT_MEMORY[agentId] ?? { episodic: "", semantic: "", prospective: "" },
          });
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
