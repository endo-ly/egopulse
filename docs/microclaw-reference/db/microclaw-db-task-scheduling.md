# Microclaw DB Schema — Task Scheduling テーブル群

> Cron / 一回限りのバックグラウンドタスクとその実行履歴・DLQ。

## ER 図

```
┌──────────────────┐
│ scheduled_tasks  │
│──────────────────│
│ id (PK)          │───┐
│ chat_id          │   │    ┌───────────────────┐
│ prompt           │   │    │ task_run_logs     │
│ schedule_type    │   ├───│───────────────────│
│ schedule_value   │   │    │ id (PK)           │
│ timezone         │   │    │ task_id           │
│ next_run         │   │    │ started_at        │
│ last_run         │   │    │ finished_at       │
│ status           │   │    │ duration_ms       │
│ created_at       │   │    │ success           │
└──────────────────┘   │    │ result_summary    │
                       │    └───────────────────┘
                       │
                       │    ┌───────────────────────┐
                       └───│ scheduled_task_dlq    │
                            │───────────────────────│
                            │ id (PK)               │
                            │ task_id               │
                            │ failed_at             │
                            │ started_at            │
                            │ finished_at           │
                            │ duration_ms           │
                            │ error_summary         │
                            │ replayed_at           │
                            │ replay_note           │
                            └───────────────────────┘
```

---

## scheduled_tasks

Cron および一回限りのスケジュールタスク定義。

```sql
CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    prompt TEXT NOT NULL,
    schedule_type TEXT NOT NULL DEFAULT 'cron',
    schedule_value TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT '',              -- v12
    next_run TEXT NOT NULL,
    last_run TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_status_next
    ON scheduled_tasks(status, next_run);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | INTEGER | PK (auto) | タスクID |
| chat_id | INTEGER | NOT NULL | 実行コンテキストのチャット |
| prompt | TEXT | NOT NULL | 実行するプロンプト |
| schedule_type | TEXT | NOT NULL DEFAULT 'cron' | `cron` または `once` |
| schedule_value | TEXT | NOT NULL | cron式 または ISO タイムスタンプ |
| timezone | TEXT | NOT NULL DEFAULT '' | タイムゾーン（v12追加） |
| next_run | TEXT | NOT NULL | 次回実行予定時刻 |
| last_run | TEXT | nullable | 前回実行時刻 |
| status | TEXT | NOT NULL DEFAULT 'active' | `active` / `paused` / `completed` / `cancelled` / `running` |
| created_at | TEXT | NOT NULL | 作成日時 |

**ステートマシン**:

```
active ──(実行開始)──▶ running ──(成功)──▶ active（next_run 更新）
                         │
                         └─(失敗/DLQ)──▶ active（リトライ）or cancelled
```

---

## task_run_logs

スケジュールタスクの実行履歴。成功・失敗にかかわらず全実行を記録。

```sql
CREATE TABLE IF NOT EXISTS task_run_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL,
    chat_id INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    success INTEGER NOT NULL DEFAULT 1,
    result_summary TEXT
);

CREATE INDEX IF NOT EXISTS idx_task_run_logs_task_id
    ON task_run_logs(task_id);
```

| カラム | 型 | 説明 |
|--------|----|------|
| task_id | INTEGER | 紐づく scheduled_tasks.id |
| started_at / finished_at | TEXT | 実行時間帯 |
| duration_ms | INTEGER | 実行時間（ミリ秒） |
| success | INTEGER | 成功フラグ（0/1） |
| result_summary | TEXT | 実行結果の要約 |

---

## scheduled_task_dlq

失敗したタスク実行のデッドレターキュー。リプレイ機能付き。

```sql
CREATE TABLE IF NOT EXISTS scheduled_task_dlq (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL,
    chat_id INTEGER NOT NULL,
    failed_at TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    error_summary TEXT,
    replayed_at TEXT,
    replay_note TEXT
);

CREATE INDEX IF NOT EXISTS idx_scheduled_task_dlq_task_failed
    ON scheduled_task_dlq(task_id, failed_at DESC);
CREATE INDEX IF NOT EXISTS idx_scheduled_task_dlq_chat_failed
    ON scheduled_task_dlq(chat_id, failed_at DESC);
```

| カラム | 型 | 説明 |
|--------|----|------|
| error_summary | TEXT | エラー内容 |
| replayed_at | TEXT | リプレイ実行日時（NULL = 未リプレイ） |
| replay_note | TEXT | リプレイ時のメモ |

**設計ポイント**:
- 実行ログ（task_run_logs）と失敗詳細（scheduled_task_dlq）を分離
- DLQ にはリプレイ機能（`replayed_at`, `replay_note`）を組み込み
- 複合インデックスで「特定タスクの最新失敗」「特定チャットの最新失敗」を高速に検索
