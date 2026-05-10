# Sleep Batch Audit WebUI — Design Spec

Sleep batch（記憶整理処理）の実行履歴とメモリ差分を、WebUIで人間が監査できるようにする機能の仕様書。

## 目次

1. [目的](#1-目的)
2. [前提と制約](#2-前提と制約)
3. [ユーザー体験](#3-ユーザー体験)
4. [画面仕様](#4-画面仕様)
5. [Backend API](#5-backend-api)
6. [Frontend構成](#6-frontend構成)
7. [データフロー](#7-データフロー)
8. [Out of Scope](#8-out-of-scope)

---

## 1. 目的

Sleep batchが「何をどう書き換えたか」を人間が目視確認できるようにする。

**主用途**: 個別runのbefore/after diff確認（記憶がどう書き換わったかの監査）。

既存のSQLiteテーブル（`sleep_runs`, `memory_snapshots`）には十分な監査データが格納されているが、閲覧手段がない。WebUIに監査画面を追加してこのギャップを解消する。

---

## 2. 前提と制約

| 項目 | 内容 |
|---|---|
| 対象テーブル | `sleep_runs`, `memory_snapshots`（スキーマ変更なし） |
| Frontend stack | React 18 + Tailwind CSS 4 + Vite（既存と同じ） |
| 外部ライブラリ | 追加なし（diff算出も自前実装） |
| 認証 | 既存のBearer token認証を継続 |
| ルーター | 導入しない（stateベースのビュー切替） |

---

## 3. ユーザー体験

### アクセス方法

1. Sidebarの「Sleep Batch」ボタンを押す
2. メインエリアがChatPanelからSleepBatchPanelに切り替わる
3. Sessionsのセッション選択に戻るとChatPanelに戻る

### ナビゲーションフロー

```
Sidebar [Sleep Batch] → Run一覧画面（agent選択 + runカード一覧）
                           ↓ [Details →]
                        Run詳細画面（メタ情報 + diff表示）
                           ↓ [← Back]
                        Run一覧画面
```

### ビュー切替のstate

```typescript
type MainView =
  | { type: 'chat' }
  | { type: 'sleep-batch' };
```

SidebarのSleep Batchボタン → `setMainView({ type: 'sleep-batch' })`
Sidebarのセッション選択 → `setMainView({ type: 'chat' })`

---

## 4. 画面仕様

### 4.1 Sidebar拡張

「Runtime Config」ボタンの下に「Sleep Batch」ボタンを追加する。既存の `secondary-button` スタイルを適用。

```
┌─ Sidebar ──────────────────────┐
│ 🖼 EgoPulse  v0.1.0       [×]  │
│                                 │
│ [+ New Session]                 │
│ [⚙ Runtime Config]              │
│ [💤 Sleep Batch]               │  ← 追加
│                                 │
│ Sessions (3)                    │
│ ┌─────────────────────────────┐ │
│ │ Web Chat            [web]   │ │
│ │ Discord #general [discord]  │ │
│ │ Telegram - Lyre  [telegram] │ │
│ └─────────────────────────────┘ │
└─────────────────────────────────┘
```

agentの選択機能はSidebarには置かない。メインエリア側で行う。

### 4.2 Run一覧画面

Sleep Batchボタン押下後のメインエリア。

```
┌─ Main: Sleep Batch ────────────────────────────────────────┐
│                                                             │
│ Sleep Batch                              [agent: lyre ▼]   │
│                                                             │
│ ┌─────────────────────────────────────────────────────────┐ │
│ │ ✅ 2026-05-09 03:00     scheduled     1.2k / 420 tok   │ │
│ │    3 sessions processed                    [Details →]  │ │
│ ├─────────────────────────────────────────────────────────┤ │
│ │ ❌ 2026-05-08 15:22     manual        0 / 0 tok        │ │
│ │    Error: parse failed: invalid JSON       [Details →]  │ │
│ ├─────────────────────────────────────────────────────────┤ │
│ │ ⏭ 2026-05-08 03:00     scheduled     skipped           │ │
│ │    No new messages                         [Details →]  │ │
│ ├─────────────────────────────────────────────────────────┤ │
│ │ ✅ 2026-05-07 03:00     scheduled     980 / 310 tok    │ │
│ │    2 sessions processed                    [Details →]  │ │
│ └─────────────────────────────────────────────────────────┘ │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**構成要素:**

| 要素 | 詳細 |
|---|---|
| ヘッダー | タイトル「Sleep Batch」+ agent選択ドロップダウン |
| Agent選択 | `select` 要素。`GET /api/agents` から取得。未選択時は全agentのrunを表示 |
| Runカード | 各runをカード形式で表示（`started_at` 降順） |
| Status icon | `✅` success / `❌` failed / `⏭` skipped / `🔄` running |
| 時刻 | `started_at` をローカル時刻で `MM/DD HH:mm` 形式 |
| Trigger badge | `scheduled` / `manual` をバッジ表示 |
| Token表示 | `input / output` 形式（>999の場合はk表記、例: `1.2k`） |
| 2行目 | 成功→「N sessions processed」、失敗→error_message先頭60字、skip→理由 |
| [Details →] | ボタンで詳細画面に遷移 |

**空状態・読み込み状態:**

| 状態 | 表示内容 |
|---|---|
| 読み込み中 | 「Loading...」をパネル中央に表示 |
| run 0件 | 「No sleep batch runs yet」をパネル中央にグレー表示 |
| agent 0件（fresh DB） | agentドロップダウンを非表示、run一覧は空状態を表示 |
| API エラー | 既存のエラー表示パターンに従う（赤字のエラーメッセージ） |

### 4.3 Run詳細画面

一覧で `[Details →]` 押下後のメインエリア。

```
┌─ Main: Sleep Batch Detail ─────────────────────────────────────────┐
│                                                                     │
│  [← Back]   Run: a1b2c3d4   ✅ success                            │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │  Started    2026-05-09 03:00:12                                 ││
│  │  Finished   2026-05-09 03:00:45  (33s)                         ││
│  │  Trigger    scheduled                                           ││
│  │  Tokens     1,247 in / 420 out                                  ││
│  │  Sessions   3 chats processed                                   ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  Memory Changes                              [Split ▼]             │
│                                                                     │
│  ▼ episodic.md                                                      │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │  ┌─ Before ───────────────┐  ┌─ After ────────────────┐       ││
│  │  │ # Episodic Memory      │  │ # Episodic Memory      │       ││
│  │  │                        │  │                        │       ││
│  │  │ ## Recent Events       │  │ ## Recent Events       │       ││
│  │  │ - Had lunch with Taro  │  │ - Had lunch with Taro  │       ││
│  │  │                        │  │ - Discussed AI archite… │ ← add││
│  │  │ - Old project task     │  │                        │ ← del ││
│  │  │                        │  │                        │       ││
│  │  └────────────────────────┘  └────────────────────────┘       ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  ▼ semantic.md                                                      │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │  (same structure)                                                ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  ▼ prospective.md                                                   │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │  (same structure)                                                ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

**メタ情報カード:**

| 項目 | ソース | 表示 |
|---|---|---|
| Started | `started_at` | ローカル時刻 `YYYY-MM-DD HH:mm:ss` |
| Finished | `finished_at` | ローカル時刻 + 所要時間 `finished_at - started_at` |
| Trigger | `trigger_type` | `scheduled` / `manual` |
| Tokens | `input_tokens`, `output_tokens` | カンマ区切り + `in / out` |
| Sessions | `source_chats_json` の length | `N chats processed` |
| Error | `error_message` | 失敗runのみ赤字で全文表示 |

**Memory Changesセクション:**

- `[Split ▼]` トグルで side-by-side ↔ unified diff を切替
- デフォルトは side-by-side
- 3ファイル（episodic / semantic / prospective）をそれぞれ表示
- 各ファイル名をクリックで折りたたみ可能。デフォルトは全展開
- 変更がないファイル（`content_before == content_after`）はファイル名のみ表示し、下に「No changes」をグレーで表示
- `content_before == content_after` の場合はdiffの2カラム表示を省略し、「No changes」テキストのみ

**run status別の表示ルール:**

| Status | diff表示 | 備考 |
|---|---|---|
| success | snapshotsのbefore/afterをdiff表示 | 通常ケース |
| failed | snapshotsがあればdiff表示、なければ「No snapshots available」 | LLM呼出前に失敗した場合はsnapshotなし |
| skipped | diffセクション全体を非表示 | メタ情報のみ表示 |
| running | diffセクション全体を非表示。「Still running...」を表示 | snapshots未確定のため |

**Side-by-side diff（デフォルト）:**

- 2カラムgrid（`grid-template-columns: 1fr 1fr`）
- Before / After のヘッダー付き
- 差分行のハイライト:
  - 追加行: 背景 `rgba(0, 212, 255, 0.08)` + 行頭に `+`
  - 削除行: 背景 `rgba(248, 113, 113, 0.08)` + 行頭に `-`
  - 変更なし行: 背景なし
- 行レベルのdiff（文字単位のdiffはしない）
- 既存CSS変数 `--color-accent`, `--color-danger` を再利用

**Unified diff（切替後）:**

- 1カラム
- 追加行 = 緑背景 + `+` prefix
- 削除行 = 赤背景 + `-` prefix

---

## 5. Backend API

すべて既存のBearer token認証（`Authorization: Bearer <token>`）を要求する。

### 5.1 `GET /api/agents`

agent一覧を返す。ドロップダウンの選択肢用。

**Response (200):**

```json
{
  "ok": true,
  "agents": ["lyre", "ace"]
}
```

**実装**: `SELECT DISTINCT agent_id FROM sleep_runs ORDER BY agent_id`

**設計意図**: sleep batch履歴のあるagentのみを返す。将来的にconfig内のagent定義一覧に拡張可能なURL設計。

### 5.2 `GET /api/sleep/runs`

run一覧を返す。

**Query params:**

| Param | Required | Default | Description |
|---|---|---|---|
| `agent_id` | No | all agents | agent絞り込み |
| `limit` | No | 20 | 取得件数（max 100） |

**Response (200):**

```json
{
  "ok": true,
  "runs": [
    {
      "id": "a1b2c3d4-...",
      "agent_id": "lyre",
      "status": "success",
      "trigger_type": "scheduled",
      "started_at": "2026-05-09T03:00:12Z",
      "finished_at": "2026-05-09T03:00:45Z",
      "input_tokens": 1247,
      "output_tokens": 420,
      "total_tokens": 1667,
      "error_message": null,
      "session_count": 3
    }
  ]
}
```

`session_count` は `source_chats_json`（JSON配列）の `length` から算出。

**実装**: 既存の `list_sleep_runs(agent_id, limit)` を使用。`session_count` のみ追加計算。`agent_id` 未指定時は全agentのrunを取得するクエリを追加（WHERE句なし）。

### 5.3 `GET /api/sleep/runs/:run_id`

単一runの詳細 + memory snapshots。

**Response (200):**

```json
{
  "ok": true,
  "run": {
    "id": "a1b2c3d4-...",
    "agent_id": "lyre",
    "status": "success",
    "trigger_type": "scheduled",
    "started_at": "2026-05-09T03:00:12Z",
    "finished_at": "2026-05-09T03:00:45Z",
    "input_tokens": 1247,
    "output_tokens": 420,
    "total_tokens": 1667,
    "error_message": null,
    "source_chats_json": "[1, 5, 12]"
  },
  "snapshots": [
    {
      "file": "episodic",
      "content_before": "# Episodic Memory\n\n...",
      "content_after": "# Episodic Memory\n\n..."
    },
    {
      "file": "semantic",
      "content_before": "...",
      "content_after": "..."
    },
    {
      "file": "prospective",
      "content_before": "...",
      "content_after": "..."
    }
  ]
}
```

**実装**: 既存の `get_sleep_run(id)` + `get_snapshots_for_run(id)` を使用。

**エラーレスポンス:**

| Case | HTTP Status | Body |
|---|---|---|
| run_id不存在 | 404 | `{ ok: false, error: "not_found", message: "Run not found" }` |

### 5.4 Backend実装方針

- ルーター: `src/channels/web/` の既存ルーターにルートを追加
- 認証: 既存の `web.auth_token` と同じBearer認証ミドルウェア
- エラーレスポンス: 既存フォーマット（`{ ok: false, error: "...", message: "..." }`）
- 新規追加クエリ: `list_distinct_agent_ids()` のみ

---

## 6. Frontend構成

### 新規ファイル

| ファイル | 責務 |
|---|---|
| `web/src/components/SleepBatchPanel.tsx` | 一覧・詳細の切り替えを管理するメインパネル |
| `web/src/components/RunList.tsx` | run一覧のカード表示 + agentフィルタ |
| `web/src/components/RunDetail.tsx` | run詳細のメタ情報 + diff表示のオーケストレーション |
| `web/src/components/DiffViewer.tsx` | split/unified diffレンダラ（再利用可能） |
| `web/src/hooks/useSleepBatch.ts` | runs/snapshots/agentsのデータフェッチ + キャッシュ |
| `web/src/diff.ts` | 行レベルdiff算出ユーティリティ（LCSベース） |

### 変更ファイル

| ファイル | 変更内容 |
|---|---|
| `web/src/components/App.tsx` | MainView state追加、SleepBatchPanelのレンダリング |
| `web/src/components/Sidebar.tsx` | Sleep Batchボタン追加 |
| `web/src/types.ts` | SleepRun, MemorySnapshot, Agent型追加 |
| `web/src/api.ts` | sleep batch API呼び出し関数追加 |
| `web/src/app.css` | run-card, diff関連スタイル追加 |

### State管理

既存と同じくprops/hooksバケツリレー。外部stateライブラリは導入しない。

### Diff算出

外部ライブラリなしで行レベルのdiffを実装する:

1. `content_before` / `content_after` を行配列に分割（`\n` 区切り）
2. LCS（Longest Common Subsequence）アルゴリズムで追加/削除行を検出
3. 結果を `DiffLine[]` として返す:
   ```typescript
   type DiffLine =
     | { type: 'add'; content: string }
     | { type: 'remove'; content: string }
     | { type: 'unchanged'; before: string; after: string };
   ```
   `unchanged` は before/after 両方を保持し、side-by-side描画で使用する。
4. 文字単位のdiffは実装しない（行単位で十分）

### CSS

既存の `app.css` に追記。新規CSSファイルは作らない。

追加するスタイル:
- `.run-card` — session-itemと似たカードスタイル
- `.diff-container` — diff表示の外枠
- `.diff-header` — Before / After のヘッダー
- `.diff-line-add` — 追加行のハイライト
- `.diff-line-remove` — 削除行のハイライト
- `.diff-line-unchanged` — 変更なし行

---

## 7. データフロー

```
User → Sidebar [Sleep Batch]
     → App.tsx: setMainView({ type: 'sleep-batch' })
     → SleepBatchPanel mounts
       → useSleepBatch.fetchAgents()
       → useSleepBatch.fetchRuns(agentId)
     → RunList renders

User → RunList [Details →]
     → SleepBatchPanel: setSelectedRunId(runId)
     → useSleepBatch.fetchRunDetail(runId)
     → RunDetail renders
       → DiffViewer renders (per file: episodic, semantic, prospective)
```

---

## 8. Out of Scope

以下は本仕様の対象外とする:

- **LLM入力プロンプトの表示** — system promptの中身は表示しない
- **Sleep batchの再実行ボタン** — WebUIからの実行トリガーは提供しない
- **統計・集計ダッシュボード** — 成功率・トークン推移等の集計は将来スコープ
- **リアルタイム通知** — batch完了時のブラウザ通知やチャネル通知
- **文字単位のdiff** — 行レベルのdiffのみ
- **外部diffライブラリの導入** — 自前実装で対応
- **URL routing** — stateベースのビュー切替のみ

---

## 9. レスポンシブ対応

モバイル・タブレットでの表示ルール。既存の `app.css` ブレークポイント（< 768px / 768–1023px / ≥ 1024px）に従う。

| 画面幅 | Run一覧 | Run詳細 |
|---|---|---|
| ≥ 1024px | カード一覧（全幅） | side-by-side diff（2カラム） |
| 768–1023px | カード一覧（全幅） | unified diff（1カラム）に自動切替 |
| < 768px | カード一覧（全幅） | unified diff（1カラム）に自動切替 |

- side-by-side / unified のトグルは常に表示可能だが、狭い画面ではデフォルトが unified
- diffのテキストが長い場合は水平スクロール（`overflow-x: auto`）
