# EgoPulse WebUI — Layout

## 1. 全体構造

Desktop は Sidebar と Main の2領域、Mobile は Top Bar と Main に Sidebar overlay を重ねる構成とする。

```
┌ Sidebar ─────────────────────┐
│ EgoPulse              🔍  ‹  │  Brand Row
│ [💬Chat][👤Agent][📅Schedule] │  Nav（agent 画面）
│ AGENTS                       │  Agents
│  ● lyre   DEFAULT            │
│  ● musa                      │
│ ──────────────────────────── │
│ （画面ごとの内容）               │  Context Section
│ ──────────────────────────── │
│ ● ok          [📈] [⚙]       │  Footer（状態 + グローバル画面）
└──────────────────────────────┘
```

---

## 2. Sidebar

### 2.1 Brand Row

- 左に product name、右端に検索アイコン（Command Palette を開く）と collapse ボタン

### 2.2 Nav

- agent 画面（Chat / Agent / Schedule）を1行のミニタブバーに並べる。各セルはアイコン + 小ラベル
- 現在位置は `aria-current="page"` と背景チントで示す

### 2.3 Agents

- 設定済みの全 agent を並べる（`/api/agents`）。選択中 agent は背景チントで強調し、`default` タグを添える
- agent アイコン：ユーザーが画像をアップロード・トリミングして設定できる（[api.md §2.11](../api.md#211-agent-avatar)）。Chat の assistant アバターにも使う
- StatusDot は未読の返事を持つセッションがある agent を示す。未読判定はフロントのみで行い、既読時刻は localStorage に保存する
- グローバル画面表示中に agent を選ぶと、その agent の Agent 画面へ遷移する

### 2.4 Footer

- 左：WebSocket 接続状態（`ok` / `degraded`）
- 右：グローバル画面（Metrics / Config）。Nav と同じアイコン + ラベルで並べ、表示中の画面は強調する

### 2.5 Context Section

Agents と Footer の間の領域。表示中の画面に応じて内容を切り替える。

| 画面 | 内容 |
|---|---|
| Chat | SESSIONS（§2.6） |
| Agent | SECTIONS（Overview / Soul / Rules / Secret / Memory / Skills）。SESSIONS と同じ「アイコン + 名前 + 1 行の要約」の行で並べる |
| Schedule | SECTIONS（Activity / Pulse / Sleep）。各行は名前と 1 行の要約（[schedule.md §1](./schedule.md#1-構成)） |
| Metrics / Config | セクション一覧 |

### 2.6 SESSIONS

- 選択 agent のセッションを `PINNED` / `ACTIVE` / `ARCHIVED`（折りたたみ）に分けて並べる。順序はサーバーの返却順（ピン留め → 最終更新降順）
- 各行は channel ミニバッジ + 表示名 + 最終メッセージ先頭1行。未読はタイトル太字 + バッジ右下のドット
- 行末の `⋯` / 右クリックで Rename / Pin / Archive / Copy session ID
- ヘッダーに channel フィルタ（`All` / `Web` / `Discord` / `Telegram` / `CLI` / `TUI` / `Voice`）と New Session（`+`）
- New Session は名前（任意）を入力して draft セッションを作り Chat へ遷移する。chat レコードは初回送信時に作成され、名前もその ACK で保存する
- 一覧は WS の `session_updated` で再取得し、60 秒ポーリングをフォールバックとする

### 2.7 Collapse

- Desktop では collapse ボタンで icon-only（48px）に縮小できる。Nav・Footer アイコン・StatusDot のみ残し、`title` で名称を保持する
- 状態は URL query（`?sidebar=collapsed`）で保持する

---

## 3. Mobile Top Bar

Mobile（< 640px）のみ表示する高さ 44px のバー。

| 要素 | 動作 |
|---|---|
| hamburger | Sidebar overlay の開閉 |
| StatusDot | 接続状態 |
| 検索 | Command Palette を開く |

画面切替は Sidebar overlay の Nav / Footer で行う。Sidebar は左右スワイプでも開閉できる（水平移動が垂直移動を上回る場合のみ判定）。

---

## 4. URL 構造

| 画面 | URL | スコープ |
|---|---|---|
| Chat | `/agents/:agentId/chat`（セッション選択時 `/s/:sessionKey`） | agent |
| Agent | `/agents/:agentId`、`/agents/:agentId/:section` | agent |
| Schedule | `/agents/:agentId/schedule`（Activity）、`/schedule/runs/{pulse\|sleep}/:runId`、`/schedule/pulse/:intentionId`、`/schedule/sleep`（[schedule.md §7](./schedule.md#7-url-構造)） | agent |
| Metrics | `/metrics?range=` | global |
| Config | `/config/:section` | global |

`/` は既定 agent の Chat を表示する。各画面の詳細は個別ドキュメントを参照。

---

## 5. レスポンシブ

| 名前 | 幅 | Sidebar | 2ペイン画面（Schedule 等） |
|---|---|---|---|
| `sm` | < 640px | hamburger overlay | 1ペイン。選択で詳細を全画面表示（`←` で戻る） |
| `md` | 640–1023px | 常時表示 | 1ペイン。同上 |
| `lg` | ≥ 1024px | 常時表示（collapse 可） | 一覧 + 詳細の2ペイン |

- Mobile の overlay は backdrop / 項目選択 / ESC / route 変更 / 左スワイプで閉じる。開閉状態は URL に乗せない
- diff の既定表示は `lg` で split、それ以下で unified

---

## 6. フォーカス・ロード・エラー

- 起動時は既定 agent の Chat を表示する。認証未設定なら AuthModal を最優先で表示する
- 画面切替時はその画面の最初のインタラクティブ要素へフォーカスする。Modal / Palette は focus trap を持ち、閉じたら呼び出し元へ戻す
- データロードは、初回は領域中央の spinner、再取得は既存内容を残して小 spinner、失敗は EmptyState + Retry、空は EmptyState で次アクションを示す

---

## 7. キーボードショートカット

| キー | 動作 |
|---|---|
| `Cmd/Ctrl + K` | Command Palette |
| `Cmd/Ctrl + N` | New Session（選択 agent） |
| `Cmd/Ctrl + [` / `]` | 前 / 次の agent 画面 |
| `Esc` | Modal / Palette / Sidebar overlay を閉じる |

ブラウザ予約の組み合わせ（`Cmd+1..9`、`Cmd+,`、`Cmd+W` 等）は使わない。入力欄フォーカス中は修飾キーなしのショートカットを無効にする。
