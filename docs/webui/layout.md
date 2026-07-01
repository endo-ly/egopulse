# EgoPulse WebUI — Layout

WebUI の全体レイアウト、Sidebar / Top Bar の構造、レスポンシブ挙動を定義する。

## 1. 全体構造

```
┌─ Sidebar (260px) ──┬─ Top Bar (h:56px) ──────────────────────┐
│                    │                                          │
│ (logo + version)   │ [⌘K] [Chat][Sleep][Pulse][Metrics][⚙]   │
│                    │                              [Health]    │
│ AGENTS             ├─ Main ──────────────────────────────────┤
│ (agent list)       │                                          │
│                    │   選択タブ + 選択 agent のコンテンツ      │
│ SESSIONS           │                                          │
│ (filter + list)    │                                          │
│ + New Session      │                                          │
│                    │                                          │
│ Runtime Status     │                                          │
└────────────────────┴──────────────────────────────────────────┘
```

- Sidebar は全高・固定幅（desktop）
- Top Bar は右側全幅・固定高（56px）
- Main は残り全幅・全高、内部スクロール

---

## 2. Sidebar

### 2.1 構成

```
┌─ Sidebar ──────────────┐
│ ◆ EgoPulse  v0.1.0     │  ← Brand header
│ ──────────────────     │
│ AGENTS                 │  ← Section title
│ ● lyre (default)       │  ← Agent item (selected)
│ ○ ace                  │  ← Agent item
│ ○ vega                 │
│ ──────────────────     │
│ SESSIONS  [All ▼]      │  ← Section + filter
│ ▸ Web Chat    [web] ●  │
│ ▸ Dev         [dis…]   │
│ ▸ Notes       [cli]    │
│ + New Session          │
│ ──────────────────     │
│ ◴ ok    ●2 turns live  │  ← Runtime status footer
└────────────────────────┘
```

### 2.2 Brand Header

- 高さ 56px（Top Bar と整合）
- 左にロゴ画像（40×40）、右に product name と version

### 2.3 AGENTS Section

Sidebar の第1セクション。必ず表示する。

- Section title（小テキスト・uppercase・muted）
- agent 一覧：各 agent を1行に並べる。左端に StatusDot、続けて agent name、必要に応じてタグ（`default` 等）
- 選択中 agent は強調表示（アクセント2色の枠線 + 内側 ring）
- StatusDot の色：
  - `live`（`active === true`、accent 色 + pulse アニメーション）：active turn 実行中
  - `idle`（`active === false`、muted-2 色）：待機中

#### Agent 一覧のデータソース

設定済みの全 agent を返す API を `/api/agents` で提供する。既存の `/api/agents` は Sleep run のある agent のみを返す仕様だったが、これを **設定済み agent 全てを返すように拡張** する。Sleep / Pulse の各一覧 API は引き続き「実行履歴のある agent」を個別に返してよい。

レスポンス例：

```json
{
  "ok": true,
  "agents": [
    { "id": "lyre", "label": "Lyre", "is_default": true, "active": false },
    { "id": "ace", "label": "Ace", "is_default": false, "active": true }
  ]
}
```

`active` フィールドは内部で `ActiveTurnTracker::is_active(agent_id)` を呼んで判定する（既存の `src/runtime/turn_scheduler.rs` に tracker が存在する）。

**polling 戦略**：`/api/agents` を5秒間隔でポーリングし、`active` フィールドを更新する。これにより最大5秒の遅延で StatusDot が active 状態に切り替わる。

### 2.4 SESSIONS Section

Sidebar の第2セクション。

```
SESSIONS  [All ▼]
  ▸ Web Chat          [web]  ●
  ▸ Dev Discussion    [discord]
  ▸ Yesterday notes   [cli]
  ▸ Quick test        [tui]
+ New Session
```

#### Session Item

- panel 背景・大 radius・card 相当の shadow
- 1行目：label（強調本文）
- 2行目：channel badge
- 3行目：preview（最終メッセージの先頭1行、`text-xs` `muted`、ellipsis 付き）
- 選択中：強調表示

#### Channel Filter

SESSIONS ヘッダーに単一選択のドロップダウンを置く：

- `All` / `Web` / `Discord` / `Telegram` / `CLI` / `TUI` / `Voice`
- 選択 agent と AND 条件でフィルタ
- 選択中セッションがフィルタで除外される場合、フィルタ切替と同時に最初のマッチするセッションへ選択を移す

#### List Order

- `last_message_time` 降順（最新が上）
- 新規セッション作成直後は楽観的にリスト先頭へ挿入し、サーバー応答後に実際の位置へ差し替え

#### Empty States

| 状態 | 表示 |
|---|---|
| セッション0件 | EmptyState: "No sessions yet. Start a new conversation." |
| フィルタ結果0件 | EmptyState: "No {channel} sessions for this agent." |
| ロード中 | Spinner（中）を中央 |
| ロード失敗 | EmptyState: error message + Retry button |

### 2.5 New Session

Sidebar 最下部に「+ New Session」ボタンを固定表示。

- 選択中 agent を親とする新規 web セッションを作成
- クリック → 楽観的に `session-{timestamp}` キーを生成し Sidebar 先頭へ挿入 → Chat タブへ遷移 → Composer へフォーカス
- サーバー側へは最初のメッセージ送信時に chat レコードが作成される（事前作成しない）
- 未送信の新規セッションはブラウザリロードで消失する（ドラフト扱い、永続化は提供しない）
- agent 未選択時は `default_agent` を使用

### 2.6 Runtime Status Footer

Sidebar 最下部、`+ New Session` の下。

- Health status + active turn 数
- 小テキスト・muted
- StatusDot で状態を視覚的に示す
- hover で Metrics タブへのリンクを表示
- `/health` を定期的にポーリングして更新（間隔は [metrics.md](./metrics.md) に準拠）

#### Health status の定義

| status | 条件 |
|---|---|
| `ok` | すべての有効チャネルが running、DB 正常、（MCP がある場合）全 MCP 接続 |
| `degraded` | 一部チャネルが failed / stopped、または一部 MCP 接続失敗。ただし Web チャネルは running のまま |
| `down` | Web チャネルが running でない、または DB 異常。WebUI 自体が動かないため表示されないはずだが、判定としては定義する |

---

## 3. Top Bar

```
┌─ Top Bar ────────────────────────────────────────────┐
│ [🔍 ⌘K Search…]  [Chat][Sleep][Pulse][Metrics][⚙]   │
│                                       [● 3/3 ch]     │
└──────────────────────────────────────────────────────┘
```

### 3.1 Command Palette Trigger

- 左端に検索アイコン + プレースホルダー "Search or jump…"
- クリックまたは `Cmd+K` / `Ctrl+K` で palette 開く（[command-palette.md](./command-palette.md)）
- 見た目は secondary button 相だが、右端にキーボードショートカット表示を伴う

### 3.2 Tabs

- 5つのタブ（Chat / Sleep / Pulse / Metrics / Config）を常時表示
- 現在位置は `aria-current="page"` で示す
- アクティブタブは下線アクセント色、非アクティブは muted
- Tab click で URL 遷移

#### URL 構造

| タブ | URL | agent スコープ |
|---|---|---|
| Chat | `/agents/:agentId/chat` （セッション選択時は `/agents/:agentId/chat/s/:sessionKey`） | agent scoped |
| Sleep | `/agents/:agentId/sleep` （run 詳細は `/agents/:agentId/sleep/r/:runId`） | agent scoped |
| Pulse | `/agents/:agentId/pulse` （run 詳細は `/agents/:agentId/pulse/r/:runId`） | agent scoped |
| Metrics | `/metrics` | global（agent フィルタは query で表現） |
| Config | `/config` | global |

Chat / Sleep / Pulse は Sidebar の agent 選択に従属する（agent scoped）。Metrics / Config はグローバルで、Sidebar の agent 選択の影響を受けない。

### 3.3 Health Badge

- 通常時：success トーン、"3/3 channels · 2 MCP" のような簡易表示
- 異常時：warning / danger トーン、`recent_errors_count > 0` なら数値表示
- click → Metrics タブへ遷移

---

## 4. レスポンシブ

### 4.1 ブレークポイント

| 名前 | 幅 | 想定デバイス |
|---|---|---|
| `sm` | < 640px | mobile 縦 |
| `md` | 640-1023px | tablet / mobile 横 |
| `lg` | ≥ 1024px | desktop |

### 4.2 Desktop (`lg`)

- Sidebar：常時表示、260px 固定
- Top Bar：全タブ + palette + health badge を1行に表示
- Chat：timeline / tool cards / composer すべて標準レイアウト

### 4.3 Tablet (`md`)

- Sidebar：240px に縮小、可能なら常時表示
- Top Bar：tab label を短縮（アイコンのみまたは略称）、label は tooltip で補完
- Sleep / Pulse diff：unified をデフォルトに（split は選択可能）

### 4.4 Mobile (`sm`)

- Sidebar：非表示、hamburger ボタンで overlay 表示
  - overlay 時：固定配置、左からスライドイン（slow motion）
  - backdrop：暗い半透明、タップで閉じる
  - 開閉状態は ephemeral state（URL には乗せない）
- Top Bar：
  - hamburger ボタンを左端に表示
  - tabs は Chat / Sleep / Pulse の3つのみ表示。Metrics / Config は右端の `⋯`（オーバーフロー）メニューへ格納
  - palette trigger はアイコンのみ
  - health badge は StatusDot のみ（詳細数字省略）
- Chat：
  - message bubble の最大幅を 90% に拡大
  - composer：toolbar 上部、textarea は2行表示（展開で4行）
  - tool card：常に collapsed、tap で展開
- Sleep / Pulse diff：常に unified
- Metrics：数値カードを2列 → 1列へ

### 4.5 Sidebar 開閉の状態機械

| 画面サイズ | デフォルト状態 | 開閉トリガ |
|---|---|---|
| desktop (`lg`) | 常時 open | 閉じる手段なし |
| tablet / mobile | closed | hamburger tap で open、backdrop tap / item tap / ESC / route 変更 で close |

---

## 5. フォーカス制御

- アプリ起動直後：agent が1つでもあれば最初の agent を選択、Chat タブを表示。Composer へはフォーカスを当てない（認証モーダル等が優先されうるため）
- New Session ボタン押下：Composer へフォーカス
- Tab 切替：タブ内容の最初のインタラクティブ要素へフォーカス
- Modal 開閉：focus trap を実装。開いたとき最初のインタラクティブ要素へ、閉じたとき呼び出し元へ復帰
- Command Palette 開閉：同上

---

## 6. ローディング・エラー表示

### 6.1 初期ロード

アプリ起動時、必要な初期データ（agents / sessions / config / health）を並列取得する。

- 取得完了まで Top Bar と Sidebar は spinner 付きでスケルトン表示
- 取得失敗時：該当セクションを EmptyState で表示し Retry button を提供
- 認証未設定時：AuthModal を overlay として表示し、他操作をブロック

### 6.2 個別データロード

各タブ・パネルごとのデータロードでは、以下を使い分ける：

| 状態 | UI |
|---|---|
| 初回ロード | 対象領域全体を大 spinner 中央表示 |
| 再取得（refetch） | 既存内容を表示したまま、右上に小 spinner |
| 取得失敗 | 対象領域を EmptyState で差し替え、Retry button 表示 |
| 取得済み・データ空 | EmptyState で説明と次アクションを提示 |

---

## 7. グローバルキーボードショートカット

| キー | 動作 |
|---|---|
| `Cmd/Ctrl + K` | Command Palette 開く |
| `Cmd/Ctrl + N` | New Session（選択 agent） |
| `Cmd/Ctrl + [` | 前のタブ |
| `Cmd/Ctrl + ]` | 次のタブ |
| `Esc` | Modal / Palette / Sidebar overlay を閉じる |

### 制約

- ブラウザが予約しているショートカットと衝突する組み合わせは避ける：
  - `Cmd+1..9`：ブラウザタブ切り替え（タブ直接選択には使わない）
  - `Cmd+,`：macOS でブラウザ設定（Config タブ起動には使わない）
  - `Cmd+W` / `Cmd+T` / `Cmd+Shift+N` 等：ブラウザ基本操作
- `Cmd+,` での Config タブ起動は諦め、Config タブへの遷移は Tab クリックまたは Command Palette 経由とする
- 入力フィールド・textarea フォーカス中は、`Cmd/Ctrl` 付きでないショートカットは無効化する
