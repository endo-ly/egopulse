# Microclaw DB Schema — Sub-Agent テーブル群

> 並列サブエージェントの実行管理・親子関係・完了通知・イベントタイムライン。

## ER 図

```
┌──────────────────────┐
│ subagent_runs        │
│──────────────────────│
│ run_id (PK)          │───┐
│ parent_run_id        │   │    ┌──────────────────────┐
│ depth                │   │    │ subagent_events      │
│ chat_id              │   ├───│──────────────────────│
│ caller_channel       │   │    │ id (PK)              │
│ task                 │   │    │ run_id               │
│ context              │   │    │ event_type           │
│ status               │   │    │ detail               │
│ created_at           │   │    │ created_at           │
│ started_at           │   │    └──────────────────────┘
│ finished_at          │   │
│ cancel_requested     │   │    ┌──────────────────────┐
│ error_text           │   ├───│ subagent_announces   │
│ result_text          │   │    │──────────────────────│
│ input/output/        │   │    │ id (PK)              │
│  total_tokens        │   │    │ run_id (UNIQUE)      │
│ provider / model     │   │    │ chat_id              │
│ token_budget         │   │    │ caller_channel       │
│ artifact_json        │   │    │ payload_text         │
└──────────────────────┘   │    │ status               │
                           │    │ attempts             │
┌──────────────────────┐   │    │ next_attempt_at      │
│ subagent_focus_      │   │    │ last_error           │
│  bindings            │   │    └──────────────────────┘
│──────────────────────│   │
│ chat_id (PK)         │   │
│ run_id               │   │
│ updated_at           │   │
└──────────────────────┘   │
                           │
         （parent_run_id による自己参照）
```

---

## subagent_runs

サブエージェントの実行レコード。親子関係・トークン予算・成果物を管理。

```sql
CREATE TABLE IF NOT EXISTS subagent_runs (
    run_id TEXT PRIMARY KEY,
    parent_run_id TEXT,                       -- v14
    depth INTEGER NOT NULL DEFAULT 1,         -- v14
    chat_id INTEGER NOT NULL,
    caller_channel TEXT NOT NULL,
    task TEXT NOT NULL,
    context TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    error_text TEXT,
    result_text TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    provider TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    token_budget INTEGER NOT NULL DEFAULT 0,  -- v18
    artifact_json TEXT                        -- v18
);

CREATE INDEX IF NOT EXISTS idx_subagent_runs_chat_created
    ON subagent_runs(chat_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_subagent_runs_chat_status
    ON subagent_runs(chat_id, status);
CREATE INDEX IF NOT EXISTS idx_subagent_runs_parent_status
    ON subagent_runs(parent_run_id, status);
```

| カラム | 型 | 追加 | 説明 |
|--------|----|------|------|
| run_id | TEXT PK | v13 | 実行ID |
| parent_run_id | TEXT | v14 | 親エージェントのrun_id。NULL = ルート |
| depth | INTEGER | v14 | 階層の深さ（1 = ルート直下） |
| chat_id | INTEGER | v13 | 紐づくチャット |
| caller_channel | TEXT | v13 | 呼び出し元チャンネル |
| task | TEXT | v13 | 実行タスクの説明 |
| context | TEXT | v13 | 追加コンテキスト |
| status | TEXT | v13 | 実行ステータス |
| cancel_requested | INTEGER | v13 | キャンセル要求フラグ |
| token_budget | INTEGER | v18 | トークン予算上限 |
| artifact_json | TEXT | v18 | 成果物（JSON） |

**ステータス遷移**:

```
pending → running → completed
                  → failed
                  → cancelled（cancel_requested = 1）
```

**設計ポイント**:
- **再帰的親子関係**: `parent_run_id` でエージェントのネストを表現。`depth` で階層の深さを制限可能
- **トークン予算**: `token_budget` でサブエージェントごとのコスト上限を設定
- **成果物**: `artifact_json` で実行結果の構造化データを保存

---

## subagent_announces

サブエージェント完了の通知キュー。親チャットへの結果通知をリトライ付きで管理。

```sql
CREATE TABLE IF NOT EXISTS subagent_announces (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL UNIQUE,
    chat_id INTEGER NOT NULL,
    caller_channel TEXT NOT NULL,
    payload_text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_subagent_announces_status_next
    ON subagent_announces(status, next_attempt_at);
```

| カラム | 型 | 説明 |
|--------|----|------|
| run_id | TEXT UNIQUE | 紐づく subagent_runs.run_id |
| payload_text | TEXT | 通知ペイロード（結果テキスト等） |
| status | TEXT | `pending` / `delivered` / `failed` |
| attempts | INTEGER | 配信試行回数 |
| next_attempt_at | TEXT | 次回リトライ予定時刻 |
| last_error | TEXT | 直近のエラー |

**設計ポイント**:
- At-least-once 配信パターン。`attempts` と `next_attempt_at` で指数バックオフリトライ
- `(status, next_attempt_at)` インデックスで「再送が必要な通知」を効率的にポーリング

---

## subagent_events

サブエージェント実行のイベントタイムライン。詳細なデバッグ・トレースに使用。

```sql
CREATE TABLE IF NOT EXISTS subagent_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    detail TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_subagent_events_run_created
    ON subagent_events(run_id, created_at ASC);
```

| カラム | 型 | 説明 |
|--------|----|------|
| run_id | TEXT | 紐づく subagent_runs.run_id |
| event_type | TEXT | イベント種別（例: `started`, `tool_call`, `tool_result`, `thinking`, `completed`） |
| detail | TEXT | イベント詳細（JSON等） |

**設計ポイント**: `(run_id, created_at ASC)` で時系列順のイベントストリームを高速に取得。

---

## subagent_focus_bindings

スレッド（チャット）とサブエージェントのバインディング。フォーカスモードで使用。

```sql
CREATE TABLE IF NOT EXISTS subagent_focus_bindings (
    chat_id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

| カラム | 型 | 説明 |
|--------|----|------|
| chat_id | INTEGER PK | チャットID（1:1バインディング） |
| run_id | TEXT | 紐づくサブエージェント |
| updated_at | TEXT | バインディング更新日時 |

**設計ポイント**: チャットが特定のサブエージェントに「フォーカス」している状態を管理。フォーカス中はそのエージェントの出力がチャットにルーティングされる。
