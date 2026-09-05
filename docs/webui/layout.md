# EgoPulse WebUI — Layout

WebUI の全体レイアウト、Sidebar の構造、レスポンシブ挙動を定義する。

## 1. 全体構造

```
┌─ Sidebar (216px) ──┬─ Main ──────────────────────────────────┐
│                    │                                          │
│ ◆ EgoPulse      [🔍][<]│  選択タブ + 選択 agent のコンテンツ      │
│ [💬][🌙][◌][◔]     │                                          │
│ ────────────────── │   チャットは Composer 以外の              │
│ AGENTS             │   縦領域をすべて使う                      │
│ SESSIONS  [All ▼] +│                                          │
│ ────────────────── │                                          │
│ ● ok [⚙]           │                                          │
└────────────────────┴──────────────────────────────────────────┘
```

- Desktop では 2 カラムグリッド（Sidebar + Main）。Top Bar は存在しない
- Sidebar は全高・固定幅（desktop）
- Main は残り全幅・全高、内部スクロール。Chat タブでは Timeline + Composer のみで構成され、ヘッダー行を持たない

---

## 2. Sidebar

### 2.1 構成

```
┌─ Sidebar ──────────────┐
│ ◆ EgoPulse       [🔍][<]│  ← Brand row（検索は右端）
│ [💬][🌙][◌][◔]         │  ← Nav（運用ビュー4タブのミニタブバー）
│ ──────────────────     │
│ AGENTS                 │
│ ● lyre (default)       │
│ ○ ace                  │
│ ──────────────────     │
│ SESSIONS  [All ▼]  (+) │  ← New Session はヘッダー右端
│ ▸ [web]   preview…  ●  │
│ ▸ [dis…]  preview…     │
│ ──────────────────     │
│ ● ok [⚙]                 │  ← Footer（WS接続状態 + Config）
└────────────────────────┘
```

Sidebar は折りたたみ可能。Desktop でも [<] ボタンで icon-only の細いバー（48px）に縮小できる。

#### 畳み込み状態

| 状態 | 幅 | 表示内容 |
|---|---|---|
| expanded（デフォルト） | 216px (desktop / tablet) | 全要素表示 |
| collapsed | 48px | ブランドマーク「E」・nav（縦並びアイコンのみ）・Config 歯車・Runtime Status StatusDot。ラベル・セッション一覧・Search・New Session は非表示 |

- 畳み込み状態は URL query (`?sidebar=collapsed`) で永続化し、リロード後も維持
- collapsed 状態でアイコン click すると expanded に戻る
- collapsed の nav 行は `title` 属性でアクセシブルな名称を保持する
- Search ボタンは collapsed では非表示。`Cmd+K` で常に palette を開ける
- Mobile では折りたたみ機能を提供しない（Mobile は hamburger overlay のみ）

### 2.2 Brand Row

- 高さ 40px のコンパクトな1行。左に product name、右端に検索アイコン＋ collapse ボタン
- 検索アイコン（24px・ghost）：クリックで Command Palette を開く（[command-palette.md](./command-palette.md)）。hover で tooltip（"Search or jump… (⌘K)"）。collapsed では非表示（`Cmd+K` で常に開ける）

### 2.3 Nav（タブ）

**運用ビューの4タブ（Chat / Sleep / Pulse / Metrics）** を **1行のミニタブバー** に収める。サイドバー幅を4等分したセルに、アイコン + 微小ラベル（9px）を縦積みで並べる。これらは agent の運用を操作・観測する同種のビューであり、セットとして1か所に置く。

- 現在位置は `aria-current="page"` で示す。選択中セルは背景チントで強調表示
- 有効タブ（Chat / Sleep）：セル click で URL 遷移する（§3.1 の URL 構造）
- 無効タブ（Pulse / Metrics）：`disabled` でミュート表示。hover で "coming soon" の tooltip を表示し、有効化の際は同じセルのまま有効化する
- collapsed 時は縦並びのアイコン列になる（ラベルは CSS で非表示、`title` 属性でアクセシブルな名称を保持）
- Config（§2.7）はシステム設定というメタレベルの機能のためバーに含めず、Footer に置く

### 2.4 AGENTS Section

Sidebar の第1セクション。必ず表示する。

- Section title（小テキスト・uppercase・muted）
- agent 一覧：各 agent を1行に並べる。左端に StatusDot、続けて agent name、必要に応じてタグ（`default` 等）
- 行は枠線・背景を持たない。hover で背景ハイライト、選択中 agent はアクセント2色の背景チントで強調表示
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

`active` フィールドは内部で `ActiveTurnTracker::is_active(agent_id)` を呼んで判定する（既存の `src/runtime/turn/scheduler.rs` に tracker が存在する）。

**polling 戦略**：`/api/agents` を5秒間隔でポーリングし、`active` フィールドを更新する。これにより最大5秒の遅延で StatusDot が active 状態に切り替わる。

### 2.5 SESSIONS Section

Sidebar の第2セクション。

```
SESSIONS  [All ▼]
  ▸ [web]      preview…
  ▸ [discord]  preview…
  ▸ [cli]      preview…
  ▸ [tui]      preview…
```

#### Session Item

- 枠線・背景を持たない 1 行構成・細身（縦余白は極小）
- channel badge（コンパクト表示）+ preview（最終メッセージの先頭1行、小サイズ・muted・ellipsis 付き）
- hover で背景ハイライト、選択中はアクセント2色の背景チントで強調表示
- label（session key 等の ID 的表記）は表示しない

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

### 2.6 New Session

SESSIONS ヘッダー右端の「+」アイコンボタン（22px・ghost）。新規セッションの作成口がセッション一覧の文脈上にあることで、操作対象が直感的に分かる。

- 選択中 agent を親とする新規 web セッションを作成
- クリック → 楽観的に `session-{timestamp}` キーを生成し Sidebar 先頭へ挿入 → Chat タブへ遷移 → Composer へフォーカス
- サーバー側へは最初のメッセージ送信時に chat レコードが作成される（事前作成しない）
- 未送信の新規セッションはブラウザリロードで消失する（ドラフト扱い、永続化は提供しない）
- agent 未選択時は `default_agent` を使用

### 2.7 Runtime Status Footer

Sidebar 最下部。左に WS 接続状態、右に Config 歯車を置く。

- Health status（`ok` / `degraded`）：WebSocket の接続状態のみを写す。`closed` で `degraded`、それ以外で `ok`
- 小テキスト・muted。長い場合は ellipsis
- StatusDot で状態を視覚的に示す

#### Config Utility

Footer 右端の歯車アイコン（24px・ghost）。Config はシステム設定というメタレベルの機能のため Nav バーには含めず、定番の「フッターの歯車」位置に置く。無効化中は `disabled` でミュート表示 + "coming soon" tooltip。collapsed 時は StatusDot の下に縦並びで残る。

#### Health status の定義

| status | 条件 |
|---|---|
| `ok` | すべての有効チャネルが running、DB 正常、（MCP がある場合）全 MCP 接続 |
| `degraded` | 一部チャネルが failed / stopped、または一部 MCP 接続失敗。ただし Web チャネルは running のまま |
| `down` | Web チャネルが running でない、または DB 異常。WebUI 自体が動かないため表示されないはずだが、判定としては定義する |

---

## 3. Mobile Top Bar

Mobile（< 640px）のみ表示されるスリムなバー（高さ 44px）。

```
┌─ Top Bar ────────────────────────────────┐
│ [☰]  [Chat ▼]            ●  [🔍]         │
└──────────────────────────────────────────┘
```

| 要素 | 動作 |
|---|---|
| hamburger `[☰]` | Sidebar overlay の開閉。`aria-expanded` で状態を持つ |
| tab select | ドロップダウンで5タブへ遷移。無効タブは選択不可 |
| StatusDot | Runtime health の簡易表示 |
| palette `[🔍]` | Command Palette を開く |

Desktop では Top Bar はレンダリングされない。

### 3.1 タブと URL 構造

| タブ | URL | agent スコープ |
|---|---|---|
| Chat | `/agents/:agentId/chat` （セッション選択時は `/agents/:agentId/chat/s/:sessionKey`） | agent scoped |
| Sleep | `/agents/:agentId/sleep` （run 詳細は `/agents/:agentId/sleep/r/:runId`） | agent scoped |
| Pulse | `/agents/:agentId/pulse` （run 詳細は `/agents/:agentId/pulse/r/:runId`） | agent scoped |
| Metrics | `/metrics` | global（agent フィルタは query で表現） |
| Config | `/config` | global |

Chat / Sleep / Pulse は Sidebar の agent 選択に従属する（agent scoped）。Metrics / Config はグローバルで、Sidebar の agent 選択の影響を受けない。

---

## 4. レスポンシブ

### 4.1 ブレークポイント

| 名前 | 幅 | 想定デバイス |
|---|---|---|
| `sm` | < 640px | mobile 縦 |
| `md` | 640-1023px | tablet / mobile 横 |
| `lg` | ≥ 1024px | desktop |

### 4.2 Desktop (`lg`)

- Sidebar：常時表示、216px 固定。Nav（運用4タブ）/ Search / New Session / Config utility を含む
- Top Bar：なし
- Chat：timeline / composer のみで構成され、チャットが縦領域をすべて使う

### 4.3 Tablet (`md`)

- Sidebar：216px、常時表示
- Sleep / Pulse diff：unified をデフォルトに（split は選択可能）

### 4.4 Mobile (`sm`)

- Sidebar：非表示、hamburger ボタンで overlay 表示
  - overlay 時：固定配置、左からスライドイン（slow motion）
  - backdrop：暗い半透明、タップで閉じる
  - 開閉状態は ephemeral state（URL には乗せない）
- Top Bar：高さ 44px のスリムバー（§3）
- Chat：
  - message bubble の最大幅を 90% に拡大
  - composer：toolbar 上部、textarea は2行表示（展開で4行）
  - tool card：常に collapsed、tap で展開
- Sleep / Pulse diff：常に unified
- Metrics：数値カードを2列 → 1列へ

### 4.5 Sidebar 開閉の状態機械

| 画面サイズ | デフォルト状態 | 開閉トリガ |
|---|---|---|
| desktop (`lg`) | 常時 open | collapse ボタンで icon-only 化 |
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
