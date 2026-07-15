# EgoPulse DB Schema

SQLite データベースのスキーマ・テーブル定義・マイグレーション機構。

## 目次

1. [全体構成と ER 図](#1-全体構成と-er-図)
2. [テーブル定義](#2-テーブル定義)
3. [Rust 構造体マッピング](#3-rust-構造体マッピング)
4. [設計上の注意点](#4-設計上の注意点)

---

## 1. 全体構成と ER 図

```
egopulse.db (SQLite / WAL mode) — ConversationScope::Normal のストレージ先
├── db_meta                  — スキーマバージョン管理（key-value）
├── schema_migrations        — マイグレーション適用履歴
├── chats                    — チャットメタデータ・チャンネルアイデンティティ
├── messages                 — メッセージ履歴
├── sessions                 — セッションスナップショット（シリアライズ済み会話）
├── tool_calls               — ツール呼び出し記録
├── llm_usage_logs           — LLM API 使用量ログ
├── sleep_runs               — スリープバッチ実行履歴（run集約log）
├── sleep_run_steps          — Sleep step実行log（run×step）
├── sleep_step_checkpoints   — Step処理checkpoint（agent×step×source state）
├── pulse_runs               — Pulse（注意活性化）実行履歴
├── episode_events           — エピソード記憶台帳（Event Extraction で蓄積）
├── episode_rollups          — エピソード記憶の週次/月次派生要約（Call2 で生成）
├── memory_snapshots         — スリープ実行中のメモリファイル更新履歴（run×file log）
└── turn_runs                — Turn実行状態機械（durable turn lifecycle）
```

| 項目 | 値 |
|------|----|
| テーブル数 | 15（データテーブル 13 + マイグレーション基盤テーブル 2） |
| インデックス数 | 24 |
| 外部キー制約 | 3（tool_calls.chat_id → chats.chat_id, sleep_run_steps.sleep_run_id → sleep_runs.id, memory_snapshots.run_id → sleep_runs.id） |
| スキーマバージョン管理 | バージョンベース（`SCHEMA_VERSION` 定数、現行 v13） |
| DBライブラリ | rusqlite 0.37（bundled） |
| DBファイル | `{data_dir}/egopulse.db` |
| 接続ラッパー | `Mutex<Connection>` |
| PRAGMA | `journal_mode=WAL`, `busy_timeout=5s` |

---

### ER 図

```
┌──────────────────┐       ┌──────────────────┐
│    chats         │1    * │    messages      │
│──────────────────│───────│──────────────────│
│ chat_id (PK)     │       │ (id, chat_id) PK │
│ chat_title       │       │ sender_id        │
│ chat_type        │       │ content          │
│ last_message_time│       │ sender_kind      │
│ channel          │       │ timestamp        │
│ external_chat_id │       └──────────────────┘
│ agent_id         │
│                  │
│                  │1    1 ┌──────────────────┐
│                  │───────│   sessions       │
│                  │       │──────────────────│
└──────────────────┘       │ chat_id (PK)     │
        │                  │ messages_json    │
        │                  │ updated_at       │
        │                  └──────────────────┘
        │
        │1    *
        │───────┌──────────────────┐
        │       │  tool_calls      │
        │       │──────────────────│
        └───────│ chat_id (FK)     │
                │ id               │
                │ message_id       │
                │ (id, chat_id,    │
                │  message_id) PK  │
                │ tool_name        │
                │ tool_input       │
                │ tool_output      │
                │ timestamp        │
                └──────────────────┘

        │1    *
        │───────┌──────────────────┐
        │       │  llm_usage_logs  │
        │       │──────────────────│
        └───────│ id (PK)          │
                │ chat_id          │
                │ caller_channel   │
                │ provider         │
                │ model            │
                │ input_tokens     │
                │ output_tokens    │
                │ total_tokens     │
                │ request_kind     │
                │ created_at       │
                └──────────────────┘

        ┌──────────────────┐       ┌──────────────────┐
        │   sleep_runs     │1    * │memory_snapshots  │
        │──────────────────│───────│──────────────────│
        │ id (PK)          │       │ id (PK)          │
        │ agent_id         │       │ run_id           │
        │ status           │       │ agent_id         │
        │ trigger_type     │       │ file             │
        │ started_at       │       │ content_before   │
        │ finished_at      │       │ content_after    │
        │ source_chats_json│       │ created_at       │
        │ source_digest_md │       └──────────────────┘
        │ input_tokens     │
        │ output_tokens    │
        │ total_tokens     │
        │ error_message    │
        └──────────────────┘

        ┌──────────────────┐
        │  episode_events  │
        │──────────────────│
        │ id (PK)          │
        │ agent_id         │
        │ kind             │
        │ sleep_run_id     │
        │ (agent_id,       │
        │  experienced_at) │
        └──────────────────┘

        ┌──────────────────┐
        │ episode_rollups  │
        │──────────────────│
        │ id (PK)          │
        │ agent_id         │
        │ granularity      │
        │ period_key       │
        │ summary_md       │
        │ (agent_id,       │
        │  granularity,    │
        │  period_key) UQ  │
        └──────────────────┘

        ┌──────────────────┐
        │   pulse_runs     │
        │──────────────────│
        │ id (PK)          │
        │ agent_id         │
        │ intention_id     │
        │ due_key          │
        │ chat_id          │
        │ message_id       │
        │ status           │
        │ started_at       │
        │ finished_at      │
        │ output_kind      │
        │ output_text      │
        │ error_message    │
        └──────────────────┘

┌──────────────────┐       ┌──────────────────┐
│──────────────────│       │──────────────────│
│ key (PK)         │       │ version (PK)     │
│ value            │       │ applied_at       │
└──────────────────┘       │ note             │
                           └──────────────────┘
```

---

## 2. テーブル定義

### chats

チャットメタデータとチャンネル横断のアイデンティティマッピング。

```sql
CREATE TABLE IF NOT EXISTS chats (
    chat_id INTEGER PRIMARY KEY,
    chat_title TEXT,
    chat_type TEXT NOT NULL DEFAULT 'private',
    last_message_time TEXT NOT NULL,
    channel TEXT,
    external_chat_id TEXT,
agent_id TEXT NOT NULL DEFAULT 'default'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_chats_channel_external_chat_id
    ON chats(channel, external_chat_id);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| chat_id | INTEGER | PK (auto) | 内部ID |
| chat_title | TEXT | nullable | 表示名 |
| chat_type | TEXT | NOT NULL DEFAULT 'private' | チャット種別 |
| last_message_time | TEXT | NOT NULL | 最終メッセージ時刻（RFC3339） |
| channel | TEXT | nullable | チャンネル識別子（`cli`, `web`, `discord`, `telegram`） |
| external_chat_id | TEXT | nullable | 外部プラットフォームのチャットID |
| agent_id | TEXT | NOT NULL DEFAULT 'default' | エージェント識別子。エージェント単位の記憶読み込みやチャネル紜付けに使用 |
| revision | INTEGER | NOT NULL DEFAULT 0 | 会話変更ごとに増加する整数CAS。timestamp を競合判定から排除 |
| next_message_seq | INTEGER | NOT NULL DEFAULT 1 | 次に発行する chat 内 message sequence |

**操作**:
- `resolve_chat_id(channel, external_chat_id)` — 既存チャットの検索
- `resolve_or_create_chat_id(channel, external_chat_id, chat_title, chat_type)` — Upsert（`ON CONFLICT DO UPDATE`）
- `get_chat_by_id(chat_id)` — chat_id からチャンネル情報を逆引き

#### Channel Log (Multi-Agent Room)

Multi-Agent Room では共有の Channel Log チャットが作成される。

- `external_chat_id`: `discord:{channel_id}:multi-room-log`
- `chat_type`: `channel_log`
- `agent_id`: `""`（空文字）
- `session` 行は持たない（`messages` テーブルのみ使用）
- `resolve_channel_log_chat_id(channel_id)` で作成・取得
- `get_channel_log_messages(chat_id, limit)` で直近 N 件を取得

---

### messages

全チャンネルのメッセージ履歴。

```sql
CREATE TABLE IF NOT EXISTS messages (
    id TEXT NOT NULL,
    chat_id INTEGER NOT NULL,
    sender_id TEXT NOT NULL,
    content TEXT NOT NULL,
    sender_kind TEXT NOT NULL DEFAULT 'user',
    timestamp TEXT NOT NULL,
    message_kind TEXT NOT NULL DEFAULT 'message',
    recipient_agent_id TEXT,
    PRIMARY KEY (id, chat_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp
    ON messages(chat_id, timestamp);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK（複合） | プラットフォーム固有のメッセージID |
| chat_id | INTEGER | PK（複合） | chats.chat_id への参照 |
| sender_id | TEXT | NOT NULL | 統一送信者識別子（例: `"lyre"`, `"user:discord:123"`, `"system"`, `"pulse"`） |
| content | TEXT | NOT NULL | メッセージ本文 |
| sender_kind | TEXT | NOT NULL DEFAULT 'user' | 送信者種別（`user`, `assistant`, `system`）。`tool` はエージェント間通信（`agent_send`）専用のレガシー値 |
| timestamp | TEXT | NOT NULL | RFC3339 タイムスタンプ |
| message_kind | TEXT | NOT NULL DEFAULT 'message' | メッセージ種別（`message`, `agent_send`, `system_event`） |
| recipient_agent_id | TEXT | nullable | 受信エージェント ID。Multi-Agent Room で使用 |
| seq | INTEGER | nullable | chat 内の単調増加 sequence。因果順序の根拠。未割当は NULL |
| turn_id | TEXT | nullable | 所属 Turn（`turn_runs`）。legacy 行は NULL |
| parent_message_id | TEXT | nullable | Tool Result 等が参照する親 message |

**制約**:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_chat_seq
    ON messages(chat_id, seq)
    WHERE seq IS NOT NULL;
```

NULL の `seq` は index 対象外（SQLite の `NULL ≠ NULL` 仕様）。未割当行が複数存在しても衝突しない。

**操作**:
- `store_message(msg)` — `INSERT OR REPLACE`
- `store_message_only(msg)` — セッションを更新せずメッセージのみ保存。Channel Log (agent_send, system_event) 向け
- `get_recent_messages(chat_id, limit)` — 最新N件（DESC→reverse）
- `get_all_messages(chat_id)` — 全件（ASC）

#### `message_kind` の種類

| 値 | 説明 |
|----|------|
| `message` | 通常のチャットメッセージ（デフォルト） |
| `agent_send` | エージェント間通信。`sender_kind` は `tool`、`recipient_agent_id` に宛先エージェント ID が設定される |
| `system_event` | システムイベント。停止条件によるターン拒否や LLM 失敗を記録。`sender_id` は `"system"`、`sender_kind` は `system`。`content` は JSON 形式（`{"reason": "ChainDepthExceeded"}` 等） |
| `tool_call` | ツール実行結果。`sender_kind` は `assistant`、`content` は JSON 文字列（`{tool, status, result, input}`） |

---

### sessions

セッションのスナップショット。LLM の会話コンテキスト全体（ツールブロック含む）を JSON として格納。

```sql
CREATE TABLE IF NOT EXISTS sessions (
    chat_id INTEGER PRIMARY KEY,
    messages_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| chat_id | INTEGER | PK | chats.chat_id と1:1 |
| messages_json | TEXT | NOT NULL | シリアライズされた会話全体 |
| updated_at | TEXT | NOT NULL | 監査時刻（RFC3339）。競合判定には使用せず整数 `chats.revision` を使用 |
| snapshot_through_seq | INTEGER | NOT NULL DEFAULT 0 | `messages_json` へ反映済みの最終 message seq。`base + seq>snapshot_through` でLLM contextを構築 |

**操作**:
- `save_session(chat_id, messages_json)` — Upsert（`ON CONFLICT DO UPDATE`）
- `load_session(chat_id)` — JSON と updated_at を取得
- `load_session_snapshot(chat_id, limit)` — JSON + 最新メッセージレコードをトランザクションで取得
- `store_message_with_session(msg, json, expected_updated_at)` — メッセージ保存 + セッション更新をトランザクションで楽観排他実行

**設計ポイント**:
- 楽観排他: `expected_updated_at` と実際の `updated_at` を比較し、競合時は `SessionSnapshotConflict` エラー
- `messages_json` にはツール呼び出しブロックも含まれるため、セッション再開時に完全なコンテキストを復元可能

---

### tool_calls

LLM ツール/ファンクション呼び出しの実行記録。

```sql
CREATE TABLE IF NOT EXISTS tool_calls (
    id TEXT NOT NULL,
    chat_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    tool_input TEXT NOT NULL,
    tool_output TEXT,
    timestamp TEXT NOT NULL,
    PRIMARY KEY (id, chat_id, message_id),
    FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
    ON tool_calls(chat_id);

CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
    ON tool_calls(chat_id, message_id);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | NOT NULL, composite PK | プロバイダ由来のツール呼び出しID |
| chat_id | INTEGER | NOT NULL, FK, composite PK | chats.chat_id |
| message_id | TEXT | NOT NULL, composite PK | 対象 assistant メッセージID |
| tool_name | TEXT | NOT NULL | ツール/ファンクション名 |
| tool_input | TEXT | NOT NULL | 入力パラメータ（JSON） |
| tool_output | TEXT | nullable | 出力結果（JSON）。成功時の実行結果保存に使用 |
| timestamp | TEXT | NOT NULL | RFC3339 タイムスタンプ |
| turn_id | TEXT | nullable | 所属 Turn（`turn_runs`）。legacy 行は NULL |
| state | TEXT | NOT NULL DEFAULT 'pending' | Tool実行状態。`pending`/`running`/`succeeded`/`failed`/`uncertain` |
| input_hash | TEXT | nullable | canonical input の hash。同一call IDでのinput整合確認 |
| idempotency_class | TEXT | nullable | 再実行可能性分類。`read_only`/`idempotent`/`non_idempotent` |
| idempotency_key | TEXT | nullable | idempotent Tool の重複排除 key |
| started_at | TEXT | nullable | 実行開始時刻 |
| finished_at | TEXT | nullable | 完了時刻 |
| error_kind | TEXT | nullable | 失敗分類 |
| error_message | TEXT | nullable | sanitized error 概要 |

**制約**:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_calls_turn_id
    ON tool_calls(turn_id, id)
    WHERE turn_id IS NOT NULL;
```

`turn_id + tool_call_id` で Tool Call を一意にし、成功結果を再利用する。NULL の `turn_id`（legacy 行）は index 対象外。

**操作**:
- `store_tool_call(tool_call)` — INSERT
- `update_tool_call_output(id, output)` — 出力の事後更新
- `update_tool_call_output_for_message(chat_id, message_id, id, output)` — assistant メッセージ単位でスコープした出力更新
- `get_tool_calls_for_message(chat_id, message_id)` — メッセージ単位の呼び出し履歴
- `get_tool_calls_for_chat(chat_id)` — チャット単位の全呼び出し履歴

**設計ポイント**:
- `id` は OpenAI/Codex などのプロバイダが返す call id であり、永続化上のグローバルIDではない
- 同じプロバイダ call id が別 assistant メッセージで再利用されても履歴を保持できるよう、主キーは `(id, chat_id, message_id)`
- 新規台帳テーブルは作らず、本テーブルを Tool 実行台帳として拡張する（`Database`（tool.rs）が claim・状態遷移・結果再利用を担う）

---

### llm_usage_logs

LLM API の使用量ログ。トークン消費の追跡とコスト管理に使用。

```sql
CREATE TABLE IF NOT EXISTS llm_usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    caller_channel TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    total_tokens INTEGER NOT NULL,
    request_kind TEXT NOT NULL DEFAULT 'agent_loop',
    estimated_tokens INTEGER NOT NULL DEFAULT 0,
    has_tools INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created
    ON llm_usage_logs(chat_id, created_at);

CREATE INDEX IF NOT EXISTS idx_llm_usage_created
    ON llm_usage_logs(created_at);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | INTEGER | PK (auto) | 内部ID（AUTOINCREMENT） |
| chat_id | INTEGER | NOT NULL | chats.chat_id への参照 |
| caller_channel | TEXT | NOT NULL | 呼び出し元チャンネル（`cli`, `web`, `discord`, `telegram`） |
| provider | TEXT | NOT NULL | LLM プロバイダ名（`openai`, `openrouter`, `ollama` 等） |
| model | TEXT | NOT NULL | モデル名 |
| input_tokens | INTEGER | NOT NULL | 入力トークン数 |
| output_tokens | INTEGER | NOT NULL | 出力トークン数 |
| total_tokens | INTEGER | NOT NULL | 合計トークン数（input + output） |
| request_kind | TEXT | NOT NULL DEFAULT 'agent_loop' | リクエスト種別 |
| estimated_tokens | INTEGER | NOT NULL DEFAULT 0 | 生推定トークン数（chars/3）。補正係数の再構築に使用 |
| has_tools | INTEGER | NOT NULL DEFAULT 0 | tool 定義を含む payload か（0/1） |
| created_at | TEXT | NOT NULL | RFC3339 タイムスタンプ |

**操作**:
- `log_llm_usage(chat_id, caller_channel, provider, model, input_tokens, output_tokens, request_kind, estimated_tokens, has_tools)` — INSERT（total_tokens は自動計算）
- `load_calibration_observations(limit_per_key)` — 補正係数再構築用の観測を最近 N 件/キー取得（oldest-first）
- `get_llm_usage_summary(chat_id, since)` — 集計サマリ（requests, input/output/total tokens, last_request_at）
- `get_llm_usage_by_model(chat_id, since)` — モデル別集計（total_tokens 降順）

---

### sleep_runs

スリープバッチ（記憶整理処理）の実行履歴。

```sql
CREATE TABLE sleep_runs (
    id                  TEXT PRIMARY KEY,
    agent_id            TEXT NOT NULL,
    status              TEXT NOT NULL,
    trigger_type        TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    finished_at         TEXT,
    source_chats_json   TEXT NOT NULL DEFAULT '[]',
    source_digest_md    TEXT,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER NOT NULL DEFAULT 0,
    error_message       TEXT
);

CREATE INDEX idx_sleep_runs_agent_started
    ON sleep_runs(agent_id, started_at);

CREATE INDEX idx_sleep_runs_agent_status
    ON sleep_runs(agent_id, status);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | UUID v4 |
| agent_id | TEXT | NOT NULL | エージェント識別子 |
| status | TEXT | NOT NULL | 実行状態（running/success/failed/skipped） |
| trigger_type | TEXT | NOT NULL | 起動トリガー（manual/scheduled/backfill） |
| started_at | TEXT | NOT NULL | 開始時刻（RFC3339） |
| finished_at | TEXT | nullable | 終了時刻（RFC3339） |
| source_chats_json | TEXT | NOT NULL DEFAULT '[]' | 対象チャットID一覧（JSON配列） |
| source_digest_md | TEXT | nullable | ソースダイジェスト（Markdown） |
| input_tokens | INTEGER | NOT NULL DEFAULT 0 | 入力トークン数 |
| output_tokens | INTEGER | NOT NULL DEFAULT 0 | 出力トークン数 |
| total_tokens | INTEGER | NOT NULL DEFAULT 0 | 合計トークン数 |
| error_message | TEXT | nullable | エラーメッセージ |

**操作**:
- `create_sleep_run(agent_id, trigger)` — INSERT（status=running, id/started_at 自動生成）
- `try_create_sleep_run(agent_id, trigger)` — 同一 agent の running run を transaction 内で確認し、存在しなければ INSERT
- `update_sleep_run_success(id, source_chats_json, source_digest_md, input_tokens, output_tokens)` — status=success 更新
- `update_sleep_run_failed(id, error_message)` — status=failed 更新
- `update_sleep_run_skipped(id)` — status=skipped 更新
- `get_sleep_run(id)` — id で取得
- `list_sleep_runs(agent_id, limit)` — agent_id 絞り込み + started_at 降順
- `count_agent_pending_sleep_messages(agent_id)` — `event_extraction` / `prospective_update` いずれかの checkpoint より新しい `messages` を pending とみなし、`COUNT(DISTINCT m.id)` で重複除外して計数（Sleep 候補判定の cursor として `finished_at` ではなく checkpoint を使用）
- `get_agent_sessions_with_pending_sleep_messages(agent_id, limit)` — 同じ pending 条件でセッションを「最古の pending message が古い順、同値なら `chat_id` 順」で取得（hot session による backlog 飢餖防止）

**設計ポイント**:
- `trigger` は SQLite 予約語のため `trigger_type` にリネーム
- `status` は step 結果から `finalize_sleep_run` で派生する集約値（running/success/partial_failure/failed/skipped）
- `input_tokens`/`output_tokens`/`total_tokens` は `sleep_run_steps` の SUM から `finalize_sleep_run` で更新

---

### sleep_run_steps

Sleep Batch の step 別実行 log。run 内の各処理工程（event_extraction, episodic_update, semantic_update, prospective_update）の status/token/error を保持。

```sql
CREATE TABLE IF NOT EXISTS sleep_run_steps (
    sleep_run_id    TEXT NOT NULL,
    step_name       TEXT NOT NULL CHECK (step_name IN ('event_extraction', 'episodic_update', 'semantic_update', 'prospective_update')),
    status          TEXT NOT NULL CHECK (status IN ('pending', 'running', 'success', 'failed', 'skipped')),
    started_at      TEXT,
    finished_at     TEXT,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    error_message   TEXT,
    metadata_json   TEXT,
    PRIMARY KEY (sleep_run_id, step_name),
    FOREIGN KEY (sleep_run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sleep_run_steps_step_status
    ON sleep_run_steps(step_name, status, started_at);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| sleep_run_id | TEXT | PK（複合）, FK CASCADE | sleep_runs.id への参照 |
| step_name | TEXT | PK（複合）, CHECK | 処理工程名（4種） |
| status | TEXT | NOT NULL, CHECK | 状態（pending/running/success/failed/skipped） |
| started_at | TEXT | nullable | 開始時刻（RFC3339） |
| finished_at | TEXT | nullable | 終了時刻（RFC3339） |
| input_tokens | INTEGER | NOT NULL DEFAULT 0 | 入力トークン数（retry合算） |
| output_tokens | INTEGER | NOT NULL DEFAULT 0 | 出力トークン数（retry合算） |
| error_message | TEXT | nullable | エラーメッセージ |
| metadata_json | TEXT | nullable | step固有の監査情報（JSON） |

**操作**:
- `start_sleep_step(sleep_run_id, step_name)` — pending→running へ遷移
- `finish_sleep_step(sleep_run_id, step_name, result)` — running→success/failed/skipped へ遷移
- `list_sleep_run_steps(sleep_run_id)` — run の全 step を取得
- `get_sleep_run_step(sleep_run_id, step_name)` — 単一 step を取得
- `finalize_sleep_run(sleep_run_id)` — step 結果から run status/token を集約

---

### sleep_step_checkpoints

agent×step×source 単位の処理 checkpoint。各 step が次回どこから入力を再開するかを保持する state テーブル。

```sql
CREATE TABLE IF NOT EXISTS sleep_step_checkpoints (
    agent_id     TEXT NOT NULL,
    step_name    TEXT NOT NULL,
    source_kind  TEXT NOT NULL,
    source_id    TEXT NOT NULL,
    cursor_at    TEXT NOT NULL,
    cursor_id    TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (agent_id, step_name, source_kind, source_id),
    CHECK (step_name IN ('event_extraction', 'semantic_update', 'prospective_update')),
    CHECK (source_kind IN ('messages', 'episode_events')),
    CHECK (
        (step_name IN ('event_extraction', 'prospective_update') AND source_kind = 'messages')
        OR (step_name = 'semantic_update' AND source_kind = 'episode_events')
    )
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| agent_id | TEXT | PK（複合） | エージェント識別子 |
| step_name | TEXT | PK（複合）, CHECK | checkpoint を所有する step（3種、episodic_update は持たない） |
| source_kind | TEXT | PK（複合）, CHECK | 入力種別（messages/episode_events） |
| source_id | TEXT | PK（複合） | messages: chat_id, episode_events: agent_id |
| cursor_at | TEXT | NOT NULL | 最後に成功処理した行の時刻 |
| cursor_id | TEXT | NOT NULL | 同一時刻内の順序を確定する ID（tie-breaker） |
| updated_at | TEXT | NOT NULL | checkpoint の最終更新時刻 |

**操作**:
- `get_sleep_checkpoint(agent_id, step_name, source_kind, source_id)` — 単一 checkpoint を取得
- `upsert_sleep_checkpoint(checkpoint)` — checkpoint を挿入または更新
- `list_sleep_checkpoints(agent_id, step_name, source_kind)` — step/source 種の全 checkpoint を取得

**設計ポイント**:
- `episodic_update` は派生データから再計算するため checkpoint を持たない
- `(cursor_at, cursor_id)` の複合 cursor で同一時刻の取りこぼしを防ぐ
- CHECK 制約で step と source_kind の正当な組合せを DB レベルで保証

---

### pulse_runs

Pulse（注意活性化）の実行履歴。due 判定・重複防止・通知紐づけに使用。

```sql
CREATE TABLE IF NOT EXISTS pulse_runs (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    intention_id  TEXT NOT NULL,
    due_key       TEXT NOT NULL,
    chat_id       INTEGER,
    message_id    TEXT,
    status        TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    output_kind   TEXT,
    output_text   TEXT,
    error_message TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_pulse_runs_due
    ON pulse_runs(agent_id, intention_id, due_key);

CREATE INDEX IF NOT EXISTS idx_pulse_runs_agent_started
    ON pulse_runs(agent_id, started_at);

CREATE INDEX IF NOT EXISTS idx_pulse_runs_chat_id
    ON pulse_runs(chat_id);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | UUID v4 |
| agent_id | TEXT | NOT NULL | エージェント識別子 |
| intention_id | TEXT | NOT NULL | due になった intention の ID |
| due_key | TEXT | NOT NULL | 重複実行防止キー |
| chat_id | INTEGER | nullable | 通知先の通常 chat_id。silent の場合 null |
| message_id | TEXT | nullable | 保存した assistant message ID。silent の場合 null |
| status | TEXT | NOT NULL | 実行状態（running/success/failed/skipped） |
| started_at | TEXT | NOT NULL | 開始時刻（RFC3339） |
| finished_at | TEXT | nullable | 終了時刻（RFC3339） |
| output_kind | TEXT | nullable | 出力種別（silent/notify） |
| output_text | TEXT | nullable | LLM 出力テキスト |
| error_message | TEXT | nullable | エラーメッセージ |

**操作**:
- `try_create_pulse_run(agent_id, intention_id, due_key)` — INSERT（status=running, id/started_at 自動生成）
- `has_pulse_due_run(agent_id, intention_id, due_key)` — 重複チェック
- `get_pulse_run(id)` — id で取得
- `update_pulse_run_success(id, output_kind, output_text, chat_id, message_id)` — status=success 更新
- `update_pulse_run_failed(id, error_message)` — status=failed 更新
- `update_pulse_run_skipped(id, reason)` — status=skipped 更新
- `reap_orphaned_pulse_runs()` — 全 `status='running'` 行を `failed` 化（起動時の孤立行回収）。戻り値は更新行数

---

### memory_snapshots

スリープ実行中のメモリファイル更新履歴。各 run についてファイル単位の aggregate snapshot（before/after）を記録する。

```sql
CREATE TABLE memory_snapshots (
    id              TEXT PRIMARY KEY,
    run_id          TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    file            TEXT NOT NULL,
    content_before  TEXT NOT NULL,
    content_after   TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE (run_id, file),
    FOREIGN KEY (run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE,
    CHECK (file IN ('episodic', 'semantic', 'prospective'))
);

CREATE INDEX idx_memory_snapshots_run_id
    ON memory_snapshots(run_id);

CREATE INDEX idx_memory_snapshots_agent_created
    ON memory_snapshots(agent_id, created_at);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | UUID v4 |
| run_id | TEXT | NOT NULL, UNIQUE*, FK CASCADE | sleep_runs.id への参照 |
| agent_id | TEXT | NOT NULL | エージェント識別子 |
| file | TEXT | NOT NULL, CHECK | 対象ファイル（episodic/semantic/prospective） |
| content_before | TEXT | NOT NULL | 更新前のファイル内容 |
| content_after | TEXT | NOT NULL | 更新後のファイル内容 |
| created_at | TEXT | NOT NULL | 作成時刻（RFC3339） |

*UNIQUE 制約は `(run_id, file)` の複合。

**操作**:
- `create_memory_snapshot(run_id, agent_id, file, content_before, content_after)` — INSERT
- `get_snapshots_for_run(run_id)` — run_id 絞り込み + created_at 昇順
- `get_snapshots_for_agent(agent_id, limit)` — agent_id 絞り込み + created_at 降順
- `get_latest_snapshot_for_file(agent_id, file)` — agent+file の最新1件
- `ensure_memory_snapshots_complete(run_id, agent_id, base)` — 欠落 file に `content_before == content_after == base` の snapshot を補完する
- `list_running_sleep_runs()` — `status = 'running'` の Run を開始順に取得（スタートアップリカバリ用）

**設計ポイント**:
- **Aggregate snapshot 方針**: 1回 LLM 呼び出し前提のため、phase ごとではなく run 単位で1ファイルにつき1件の snapshot を保存する
- **完全な3ファイルセット**: finalize 前に3種類の snapshot が揃うことを publication bundle の整合性条件とする。各 Step は成功時に担当 file の snapshot を commit し、未実行・未変更の file は `ensure_memory_snapshots_complete()` が `before == after == base` で補完する。`content_before` は常に Run 開始時の bundle（base）を使用する
- **Publication / Recovery**: `publish_bundle()` は `content_before` を precondition とし、`content_after` を原子的に公開する。スタートアップリカバリは `content_after` から再公開し、現状が `before` / `after` のいずれにも一致しない場合は startup を停止する

---

### episode_events

エピソード記憶の台帳（正本）。Sleep Batch の Event Extraction で append-only に蓄積される。

```sql
CREATE TABLE IF NOT EXISTS episode_events (
    id               TEXT PRIMARY KEY,
    agent_id         TEXT NOT NULL,
    experienced_at   TEXT NOT NULL,
    encoded_at       TEXT NOT NULL,
    kind             TEXT NOT NULL,
    title            TEXT NOT NULL,
    body_md          TEXT NOT NULL,
    ripple_strength  INTEGER NOT NULL DEFAULT 3,
    certainty        TEXT NOT NULL DEFAULT 'stated',
    sleep_run_id     TEXT NOT NULL,
    source_refs_json TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    CHECK (kind IN (
        'self', 'relationship', 'world', 'feat',
        'anomaly', 'decision', 'insight', 'rhythm'
    )),
    CHECK (ripple_strength BETWEEN 1 AND 5),
    CHECK (certainty IN ('stated', 'derived', 'tentative'))
);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced
    ON episode_events(agent_id, experienced_at);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced
    ON episode_events(agent_id, kind, experienced_at);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced
    ON episode_events(agent_id, ripple_strength, experienced_at);

CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run
    ON episode_events(sleep_run_id);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | Event の安定ID（UUID v4） |
| agent_id | TEXT | NOT NULL | エージェントごとの記憶分離 |
| experienced_at | TEXT | NOT NULL | 出来事の発生時刻（RFC3339） |
| encoded_at | TEXT | NOT NULL | 記憶として符号化された時刻（RFC3339） |
| kind | TEXT | NOT NULL | 記憶上の役割分類（8種） |
| title | TEXT | NOT NULL | 短い見出し |
| body_md | TEXT | NOT NULL | Markdown 本文 |
| ripple_strength | INTEGER | NOT NULL DEFAULT 3 | 再活性化強さ（1〜5） |
| certainty | TEXT | NOT NULL DEFAULT 'stated' | 断定度 |
| sleep_run_id | TEXT | NOT NULL | 抽出元 Sleep Run ID |
| source_refs_json | TEXT | nullable | 根拠メッセージ参照（JSON 配列） |
| created_at | TEXT | NOT NULL | DB管理用作成時刻（RFC3339） |
| updated_at | TEXT | NOT NULL | DB管理用更新時刻（RFC3339） |

**CHECK 制約**:
| 対象 | ルール |
|------|--------|
| kind | `'self'`, `'relationship'`, `'world'`, `'feat'`, `'anomaly'`, `'decision'`, `'insight'`, `'rhythm'` のいずれか |
| ripple_strength | 1 以上 5 以下 |
| certainty | `'stated'`, `'derived'`, `'tentative'` のいずれか |

**操作**:
- `insert_episode_events(run_id, events)` — batch INSERT（同一 run_id の既存レコードを先に DELETE）
- `list_episode_events(agent_id, kind, min_ripple, limit)` — kind / ripple_strength でフィルタリングし experienced_at 降順
- `count_episode_events(agent_id)` — agent の総 event 数
- `list_episode_events_by_run(run_id)` — run 単位の全 event を experienced_at 昇順

**設計ポイント**:
- **append-only 台帳**: 同一 run_id での再抽出時のみ全削除→再挿入（冪等性担保）。それ以外は追記専用で不変性を維持
- **4 つの複合インデックス**: agent_id 起点 + kind/ripple_strength のフィルタリングと experienced_at ソートをカバー
- **source_refs_json**: 根拠となったメッセージの `(chat_id, message_id)` ペアを JSON 配列として保持。イベントのトレーサビリティを確保

---

### episode_rollups

Call2 (Episodic View Materialization) で生成される週次・月次の派生要約。
`episode_events` から再生成可能な派生キャッシュ。正本は `episode_events`。

```sql
CREATE TABLE IF NOT EXISTS episode_rollups (
    id                   TEXT PRIMARY KEY,
    agent_id             TEXT NOT NULL,
    granularity          TEXT NOT NULL,
    period_key           TEXT NOT NULL,
    period_start         TEXT NOT NULL,
    period_end_exclusive TEXT NOT NULL,
    summary_md           TEXT NOT NULL,
    max_ripple           INTEGER NOT NULL DEFAULT 3,
    event_count          INTEGER NOT NULL DEFAULT 0,
    generated_run_id     TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    CHECK (granularity IN ('week', 'month')),
    CHECK (max_ripple BETWEEN 1 AND 5),
    UNIQUE(agent_id, granularity, period_key)
);

CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_period
    ON episode_rollups(agent_id, granularity, period_start);

CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_ripple
    ON episode_rollups(agent_id, granularity, max_ripple, period_start);
```

| カラム | 型 | 制約 | 説明 |
|--------|-----|------|------|
| id | TEXT | PK | UUID v4 |
| agent_id | TEXT | NOT NULL | 対象エージェント |
| granularity | TEXT | NOT NULL, CHECK | `week` / `month` |
| period_key | TEXT | NOT NULL, UNIQUE* | `2026-W22` / `2026-04` |
| period_start | TEXT | NOT NULL | 期間開始（RFC3339 または日付文字列） |
| period_end_exclusive | TEXT | NOT NULL | 期間終了の排他的境界 |
| summary_md | TEXT | NOT NULL | 要約本文 Markdown |
| max_ripple | INTEGER | NOT NULL, CHECK(1-5) | 期間内 Event の最大 ripple |
| event_count | INTEGER | NOT NULL | 要約対象 Event 数 |
| generated_run_id | TEXT | NOT NULL | 生成した Sleep Run ID |
| created_at | TEXT | NOT NULL | 作成時刻 |
| updated_at | TEXT | NOT NULL | 更新時刻 |

*UNIQUE 制約は `(agent_id, granularity, period_key)` の複合。

**Rust 構造体**: `EpisodeRollup`, `RollupGranularity`

**クエリ関数**:
- `upsert_episode_rollup(rollup)` — INSERT ... ON CONFLICT DO UPDATE
- `list_episode_rollups(agent_id, granularity, limit)` — granularity フィルタ + period_start DESC
- `get_episode_rollup(agent_id, granularity, period_key)` — 複合キーで1件取得
- `list_episode_rollups_in_range(agent_id, granularity, start, end_exclusive)` — 期間範囲
- `list_background_episode_rollups(agent_id, min_ripple, before_period_start)` — Background Months 用
- `list_episode_events_in_range(agent_id, start, end_exclusive)` — Event 期間範囲取得

---

### turn_runs

Turn 実行の状態機械。受付・入力保存・model iteration・Tool 実行・完了・失敗・uncertain のライフサイクルを永続化し、重複受付防止と安全な再試行・復旧判断を可能にする。詳細は [session-lifecycle.md §10](./session-lifecycle.md#10-durable-turn-state) を参照。

```sql
CREATE TABLE IF NOT EXISTS turn_runs (
    turn_id TEXT PRIMARY KEY,
    chat_id INTEGER NOT NULL,
    request_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'accepted','input_committed','model_pending','model_completed',
        'tools_pending','tools_completed','completed','failed','cancelled','uncertain'
    )),
    current_iteration INTEGER NOT NULL DEFAULT 0,
    input_message_id TEXT,
    final_message_id TEXT,
    config_revision INTEGER NOT NULL DEFAULT 0,
    config_fingerprint TEXT,
    model_request_hash TEXT,
    model_attempt INTEGER NOT NULL DEFAULT 0,
    output_published INTEGER NOT NULL DEFAULT 0,
    error_kind TEXT,
    error_message TEXT,
    accepted_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT,
    request_payload_hash TEXT,
    scheduled_request_json TEXT,
    origin_id TEXT,
    origin_stop_reason TEXT,
    UNIQUE(chat_id, request_key)
);

CREATE INDEX IF NOT EXISTS idx_turn_runs_chat ON turn_runs(chat_id);
CREATE INDEX IF NOT EXISTS idx_turn_runs_state ON turn_runs(state);
CREATE INDEX IF NOT EXISTS idx_turn_runs_dispatch
    ON turn_runs(state, accepted_at, turn_id)
    WHERE scheduled_request_json IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_turn_runs_origin
    ON turn_runs(origin_id, accepted_at)
    WHERE origin_id IS NOT NULL;
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| turn_id | TEXT | PK | Turn 一意ID（UUID v4） |
| chat_id | INTEGER | NOT NULL | 対象 conversation |
| request_key | TEXT | NOT NULL, UNIQUE* | 同一受付の重複防止 key。`chat_id + request_key` で一意 |
| state | TEXT | NOT NULL, CHECK | Turn 状態（10 種）。自由文字列不可 |
| current_iteration | INTEGER | NOT NULL DEFAULT 0 | 現在の model loop 位置 |
| input_message_id | TEXT | nullable | user input message 参照 |
| final_message_id | TEXT | nullable | 最終 assistant message 参照 |
| config_revision | INTEGER | NOT NULL DEFAULT 0 | Turn 開始時の Config revision |
| config_fingerprint | TEXT | nullable | Config 内容の識別 hash |
| model_request_hash | TEXT | nullable | 現在 iteration の固定 request hash（retry 同一性確認） |
| model_attempt | INTEGER | NOT NULL DEFAULT 0 | 現在 iteration の試行回数 |
| output_published | INTEGER | NOT NULL DEFAULT 0 | delta/narration/Tool Call 等を外部公開済みか（0/1）。公開済みなら retry 不可 |
| error_kind | TEXT | nullable | 最終失敗分類 |
| error_message | TEXT | nullable | sanitized error 概要 |
| accepted_at | TEXT | NOT NULL | 受付時刻 |
| updated_at | TEXT | NOT NULL | 最終更新時刻 |
| finished_at | TEXT | nullable | 完了・停止時刻 |
| request_payload_hash | TEXT | nullable | 受付時の user input 本文 hash。再受付で同一 `request_key` に異なる本文が渡された場合に拒否する |
| scheduled_request_json | TEXT | nullable | accepted Turn の実行要求（`PersistedScheduledTurn` の versioned JSON）。再起動後に Dispatcher がこれから再実行する |
| origin_id | TEXT | nullable | Agent Send chain の identity。root Turn は自身の `turn_id`、子 Turn は親の `origin_id` を継承する |
| origin_stop_reason | TEXT | nullable | chain を停止させた理由（LLM 失敗・深さ超過など）。terminal した chain の再開抑止に用いる |

*UNIQUE 制約は `(chat_id, request_key)` の複合。同じ受付を再受付した場合は新規 Turn を作らず既存 Turn を返す。

**Turn 状態**:

| 状態 | 意味 |
|---|---|
| `accepted` | 受付済み。input 未保存なら保存 |
| `input_committed` | user input 保存済み。model iteration 開始可能 |
| `model_pending` | model 呼出し中。外部出力・Tool 実行がなく request hash 一致なら retry 可能 |
| `model_completed` | model 応答受信済み。保存済み response/Tool Call から続行 |
| `tools_pending` | Tool 実行中。`tool_calls` 状態を確認 |
| `tools_completed` | Tool 実行完了。次 iteration または finalize へ |
| `completed` | 完了。保存済み結果を返し新規実行しない |
| `failed` | 失敗。明示的再実行がない限り自動再開しない |
| `cancelled` | キャンセル。自動再開しない |
| `uncertain` | 再開可否不明（crash 後の running Tool 等）。自動再開しない |

**設計ポイント**:
- 状態遷移は Rust enum と中央定義した transition rule で管理し、許可されていない遷移は DB 更新前に拒否する（`Database`（turn.rs））
- `output_published` が真の Turn は partial output を外部公開済みのため自動 retry しない
- `UNIQUE(chat_id, request_key)` により同一受付の重複を防止する。再受付時は既存 Turn を返し、`completed` なら保存済み結果を再利用する
- crash recovery は起動時に `recover_interrupted()` が未端末 Turn を処理する。詳細は [session-lifecycle.md §10](./session-lifecycle.md#10-durable-turn-state)

---

### db_meta

スキーマバージョンの key-value ストア。現在は `schema_version` のみ格納。

```sql
CREATE TABLE IF NOT EXISTS db_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| key | TEXT | PK | 設定キー（`schema_version`） |
| value | TEXT | NOT NULL | 設定値（バージョン番号の文字列表現） |

**操作**:
- `schema_version(conn)` — `db_meta.schema_version` を読み取り。未設定時は `0` を返す
- `set_schema_version(conn, version, note)` — Upsert（`ON CONFLICT DO UPDATE`）+ `schema_migrations` への履歴記録

---

### schema_migrations

マイグレーションの適用履歴。各バージョンの適用日時と注記を保持。

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL,
    note TEXT
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| version | INTEGER | PK | スキーマバージョン番号 |
| applied_at | TEXT | NOT NULL | 適用日時（RFC3339） |
| note | TEXT | nullable | マイグレーションの説明（例: `"initial schema: chats, messages, sessions, tool_calls"`） |

**設計ポイント**:
- `set_schema_version` で `db_meta` の更新と同時にレコードが INSERT される
- `INSERT OR REPLACE` により再適用時は上書き

---

## 3. Rust 構造体マッピング

| 構造体 | テーブル | フィールド |
|--------|----------|-----------|
| `StoredMessage` | messages | id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, recipient_agent_id, seq, turn_id, parent_message_id |
| `ChatInfo` | chats（一部） | chat_id, channel, external_chat_id, chat_type, agent_id |
| `SessionSummary` | chats + messages（JOIN） | chat_id, channel, surface_thread, chat_title, last_message_time, last_message_preview, agent_id |
| `SessionSnapshot` | sessions + messages | messages_json, updated_at, recent_messages: Vec\<StoredMessage\> |
| `AgentSessionInfo` | chats + sessions（JOIN） | chat_id, channel, external_chat_id, updated_at, message_count, estimated_tokens |
| `ToolCall` | tool_calls | id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp |
| `LlmUsageSummary` | llm_usage_logs（集計） | requests, input_tokens, output_tokens, total_tokens, last_request_at |
| `LlmModelUsageSummary` | llm_usage_logs（モデル別集計） | model, requests, input_tokens, output_tokens, total_tokens |
| `SleepRun` | sleep_runs | id, agent_id, status, trigger, started_at, finished_at, source_chats_json, source_digest_md, input_tokens, output_tokens, total_tokens, error_message |
| `SleepRunStep` | sleep_run_steps | sleep_run_id, step_name, status, started_at, finished_at, input_tokens, output_tokens, error_message, metadata_json |
| `SleepStepCheckpoint` | sleep_step_checkpoints | agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at |
| `SleepRunStatus` | — | 5種の enum: `Running`, `Success`, `PartialFailure`, `Failed`, `Skipped`（SQL: `running`, `success`, `partial_failure`, `failed`, `skipped`） |
| `SleepStepName` | — | 4種の enum: `EventExtraction`, `EpisodicUpdate`, `SemanticUpdate`, `ProspectiveUpdate`（SQL: `event_extraction`, `episodic_update`, `semantic_update`, `prospective_update`） |
| `SleepStepStatus` | — | 5種の enum: `Pending`, `Running`, `Success`, `Failed`, `Skipped`（SQL: `pending`, `running`, `success`, `failed`, `skipped`） |
| `CheckpointSourceKind` | — | 2種の enum: `Messages`, `EpisodeEvents`（SQL: `messages`, `episode_events`） |
| `PulseRun` | pulse_runs | id, agent_id, intention_id, due_key, chat_id, message_id, status, started_at, finished_at, output_kind, output_text, error_message |
| `MemorySnapshot` | memory_snapshots | id, run_id, agent_id, file, content_before, content_after, created_at |
| `EpisodeEvent` | episode_events | id, agent_id, experienced_at, encoded_at, kind, title, body_md, ripple_strength, certainty, sleep_run_id, source_refs_json, created_at, updated_at |
| `EpisodeEventKind` | — | 8種の enum: `Self_`, `Relationship`, `World`, `Feat`, `Anomaly`, `Decision`, `Insight`, `Rhythm`（SQL: `self`, `relationship`, `world`, `feat`, `anomaly`, `decision`, `insight`, `rhythm`） |
| `EpisodeEventCertainty` | — | 3種の enum: `Stated`, `Derived`, `Tentative`（SQL: `stated`, `derived`, `tentative`） |
| `EpisodeRollup` | episode_rollups | id, agent_id, granularity, period_key, period_start, period_end_exclusive, summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at |
| Turn 状態（`turn_runs.state`） | — | 10種: `accepted`, `input_committed`, `model_pending`, `model_completed`, `tools_pending`, `tools_completed`, `completed`, `failed`, `cancelled`, `uncertain` |
| Tool 実行状態（`tool_calls.state`） | — | 5種: `pending`, `running`, `succeeded`, `failed`, `uncertain` |

---

## 4. 設計上の注意点

### マイグレーション機構

バージョンベースのインクリメンタルマイグレーションを採用。

**仕組み**:
1. `Database::new()` → `run_migrations(conn)` を呼び出し
2. `schema_version(conn)` で `db_meta` テーブルから現在のバージョンを取得（未設定時は `0`）
3. `if version < N` ブロックで未適用のマイグレーションを逐次実行
4. 各マイグレーション適用後に `set_schema_version(conn, N, "note")` でバージョンを更新し `schema_migrations` に履歴を記録
5. `SCHEMA_VERSION` 定数（現行 `12`）に到達したら完了。`debug_assert_eq!` で検証

起動時には、DDL/DMLの前に既存の `schema_version` を読み取る。検出した版が対応する `SCHEMA_VERSION` より新しい場合は、DB種別・検出版・対応版を含む `storage_unsupported_schema_version` で起動を拒否し、既存データや版番号を書き換えない。

**新規マイグレーションの追加手順**:
1. `SCHEMA_VERSION` 定数をインクリメント（例: `5` → `6`）
2. `run_migrations()` に `if version < 6 { ... }` ブロックを追加
3. ブロック内で `conn.execute_batch("ALTER TABLE ...")` 等の DDL を実行
4. 破壊的 DDL や複数ステートメントを伴う場合は transaction 内で実行する
5. `set_schema_version(conn, 6, "description")` または transaction 用 helper を呼び出し

```rust
// 現行の v2 -> v3 例（storage.rs 内）
// if version < 3 {
//     let tx = conn.unchecked_transaction()?;
//     tx.execute_batch(
//         "DROP INDEX IF EXISTS idx_tool_calls_chat_id;
//         DROP INDEX IF EXISTS idx_tool_calls_chat_message_id;
//
//         CREATE TABLE IF NOT EXISTS tool_calls_v3 (
//             id TEXT NOT NULL,
//             chat_id INTEGER NOT NULL,
//             message_id TEXT NOT NULL,
//             tool_name TEXT NOT NULL,
//             tool_input TEXT NOT NULL,
//             tool_output TEXT,
//             timestamp TEXT NOT NULL,
//             PRIMARY KEY (id, chat_id, message_id),
//             FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
//         );
//
//         INSERT OR IGNORE INTO tool_calls_v3
//             (id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp)
//         SELECT
//             id,
//             COALESCE(chat_id, 0),
//             COALESCE(message_id, ''),
//             COALESCE(tool_name, ''),
//             COALESCE(tool_input, ''),
//             tool_output,
//             COALESCE(timestamp, '')
//         FROM tool_calls;
//
//         DROP TABLE tool_calls;
//         ALTER TABLE tool_calls_v3 RENAME TO tool_calls;
//
//         CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
//             ON tool_calls(chat_id);
//
//         CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
//             ON tool_calls(chat_id, message_id);",
//     )?;
//     set_schema_version_in_tx(&tx, 3, "scope tool call uniqueness to chat and assistant message")?;
//     tx.commit()?;
//     version = 3;
// }
//
// // v3 -> v4: agent_id カラム追加
// if version < 4 {
//     conn.execute_batch(
//         "ALTER TABLE chats ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'default';",
//     )?;
//     set_schema_version_in_tx(&tx, 4, "add NOT NULL agent_id to chats (default: default)")?;
//     version = 4;
// }
```

> **Note**: v8/v9 は旧系統のマイグレーション履歴（参照用）。現行のコードラインでは v1 から v13 までを順に適用する。

#### v8: remove bot_id from Discord session external_chat_id (旧系統)

Discord Multi-Agent Room の二層セッションアーキテクチャ導入に伴い、`chats.external_chat_id` から `:bot:<bot_id>` セグメントを除去する。

**変換例**: `discord:123:bot:main:agent:lyre` → `discord:123:agent:lyre`

**対象**: `channel = 'discord'` かつ `external_chat_id LIKE '%:bot:%:agent:%'` のレコード

**処理**:
1. `pragma_table_info` で `external_chat_id` カラムの存在を確認（v1 DB からの段階的マイグレーション対応）
2. 対象レコードをフェッチし、Rust 側で `:bot:<bot_id>` セグメントを除去
3. `UPDATE chats SET external_chat_id = ? WHERE rowid = ?`

**非対象**: `channel != 'discord'` のレコード、既に新形式のレコードは変更なし

**特徴**:
- 外部ファイル（SQL マイグレーションファイル）なし。DDL は Rust コードに直接埋め込み
- 外部クレート（refinery, sqlx 等）への依存なし
- 再起動時は適用済みバージョンまでスキップされる（冪等）

#### v9: add pulse_runs table (旧系統)

Pulse Phase 1 (Temporal Activation) の実行履歴テーブルを追加。

#### v3: add episode_events table + 4 indexes

Event Extraction の保存先として `episode_events` テーブルを新規追加。CHECK 制約と 4 つの複合インデックスを含む。

```sql
CREATE TABLE IF NOT EXISTS episode_events (
    id               TEXT PRIMARY KEY,
    agent_id         TEXT NOT NULL,
    experienced_at   TEXT NOT NULL,
    encoded_at       TEXT NOT NULL,
    kind             TEXT NOT NULL,
    title            TEXT NOT NULL,
    body_md          TEXT NOT NULL,
    ripple_strength  INTEGER NOT NULL DEFAULT 3,
    certainty        TEXT NOT NULL DEFAULT 'stated',
    sleep_run_id     TEXT NOT NULL,
    source_refs_json TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    CHECK (kind IN ('self', 'relationship', 'world', 'feat',
                    'anomaly', 'decision', 'insight', 'rhythm')),
    CHECK (ripple_strength BETWEEN 1 AND 5),
    CHECK (certainty IN ('stated', 'derived', 'tentative'))
);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced
    ON episode_events(agent_id, experienced_at);
CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced
    ON episode_events(agent_id, kind, experienced_at);
CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced
    ON episode_events(agent_id, ripple_strength, experienced_at);
CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run
    ON episode_events(sleep_run_id);
```

**特徴**:
- CHECK 制約で kind（8種）、ripple_strength（1-5）、certainty（3種）を DB レベルで検証
- 4 つの複合インデックスでクエリパターン（agent_id + kind/ripple + time ソート）をカバー
- append-only 設計だが、同一 sleep_run_id の再実行時は冪等に全削除→再挿入

#### v6: add episode_rollups table + 2 indexes

Call2 Episodic View Materialization 用の `episode_rollups` テーブルを新規追加。
CHECK 制約（granularity、max_ripple）と 2 つの複合インデックスを含む。

#### v12: durable turn lifecycle + integer seq/revision

Turn 永続化の導入。新規 `turn_runs` テーブル（CHECK 制約付き state、`UNIQUE(chat_id, request_key)`）、`chats`/`messages`/`sessions`/`tool_calls` への拡張カラム追加、legacy backfill を同一 transaction で行う。全面 Event Sourcing は導入せず、既存テーブルを正本として拡張する。

- `messages.seq`: chat ごとに `(timestamp ASC, id ASC)` 安定順で `ROW_NUMBER()` により連番 backfill。部分 UNIQUE index `idx_messages_chat_seq`（`WHERE seq IS NOT NULL`）を追加
- `chats.revision`/`chats.next_message_seq`: messages 件数 / 最大 seq+1 で初期化
- `sessions.snapshot_through_seq`: chat の最大 seq で初期化
- `tool_calls.state`: `tool_output` あり=`succeeded`、なし=`uncertain`（推測せず停止）
- `ALTER TABLE ADD COLUMN` は `pragma_table_info` 存在チェックで導出し、version rollback 再実行でも重複しない
- migration 前 backup を必須化（`new_with_backup` が backup 失敗時に `MigrationBackupFailed` で起動を拒否）

#### v13: turn_runs.request_payload_hash

`turn_runs` へ `request_payload_hash` カラムを追加。受付時に user input 本文の SHA-256 を保存し、同一 `request_key` の再受付で本文が異なる場合に受付を拒否できるようにする。既存行は NULL を許容し、NULL は未計測の legacy データとして再受付を許す。

#### v14: durable scheduled turn columns

`turn_runs` へ `scheduled_request_json` / `origin_id` を追加し、部分索引 `idx_turn_runs_dispatch`（`scheduled_request_json IS NOT NULL`）と `idx_turn_runs_origin`（`origin_id IS NOT NULL`）を追加する。accepted Turn の実行要求を永続化し、再起動後に `TurnDispatcher` が再実行できるようにする。`origin_id` は Agent Send chain の identity を永続化する。Normal / Secret 両 DB に適用する。

#### v15: turn_origins table

`turn_origins` 表を新設し、origin（人間入力の chain）ごとの実行 turn 数・terminal stop reason・更新日時を永続化する。chain が停止条件（LLM failure / chain depth / turn count / invalid agent）に到達した際、その理由を durably 記録し、再起動後に `TurnTracker` が rehydrate することで終了した chain の再実行を防ぐ。Normal / Secret 両 DB に適用する。

### 外部キー制約が最小限

明示的な FK は `tool_calls.chat_id`、`sleep_run_steps.sleep_run_id`、`memory_snapshots.run_id` の 3 つ。`messages.chat_id` や `sessions.chat_id` には FK がない。整合性はアプリケーション層で担保。

### CASCADE なし

`ON DELETE` が一切定義されていない。チャット削除時に messages / sessions / tool_calls を手動でクリーンアップする必要がある。

---

## 5. secret.db（秘匿会話用データベース）

`ConversationScope::Secret` スコープの会話（YAML 設定で `secret: true` が設定されたチャネル）を `ConversationScope::Normal` 用の `egopulse.db` から物理的に隔離するための別データベース。スコープの概要は [architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) を参照。

### 5.1 概要

| 項目 | 値 |
|------|----|
| ファイルパス | `{data_dir}/secret.db` |
| 初期化条件 | `channels.discord.channels.*` または `channels.telegram.telegram_channels.*` に `secret: true` エントリが1件以上ある場合 |
| テーブル数 | 8（`chats`, `messages`, `sessions`, `tool_calls`, `llm_usage_logs`, `turn_runs`, `db_meta`, `schema_migrations`） |
| スキーマバージョン管理 | `SECRET_SCHEMA_VERSION` 定数（現行 v4）。`egopulse.db` の `SCHEMA_VERSION` とは独立 |
| DBライブラリ | `egopulse.db` と同一（rusqlite 0.37 bundled） |
| PRAGMA | `journal_mode=WAL`, `busy_timeout=5s` |

### 5.2 テーブル構成

`secret.db` は `egopulse.db` の会話・Turn・Tool 台帳を担うサブセット。通常 DB に存在する `sleep_runs`, `sleep_run_steps`, `sleep_step_checkpoints`, `pulse_runs`, `episode_events`, `episode_rollups`, `memory_snapshots` は含まない。秘匿会話も同一の会話順序・競合制御・Turn lifecycle・Tool 実行台帳を持つため、`chats`/`messages`/`sessions` 拡張カラム、`turn_runs`、`tool_calls` を配置する。Secret スコープの Tool claim・結果保存はすべてこの DB で行われ、通常 DB へは書き込まれない。

| テーブル | schema | 備考 |
|---|---|---|
| `chats` | `egopulse.db.chats` と同 schema | `chat_type` に `secret` 等の特殊値は使わない。DB ファイル自体で隔離を表現 |
| `messages` | `egopulse.db.messages` と同 schema | |
| `sessions` | `egopulse.db.sessions` と同 schema | `messages_json` に tool call block も包含されるため LLM context 復元に影響なし |
| `tool_calls` | `egopulse.db.tool_calls` と同 schema | Secret スコープの Tool 実行台帳。claim・input hash・状態遷移・結果保存を担い、Secret Tool の入出力が通常 DB へ漏れない |
| `llm_usage_logs` | `egopulse.db.llm_usage_logs` と同 schema | この DB 内のレコードはすべて Secret スコープとして扱われる |
| `db_meta` | `egopulse.db.db_meta` と同 schema | `SECRET_SCHEMA_VERSION` を管理 |
| `schema_migrations` | `egopulse.db.schema_migrations` と同 schema | |

**`tool_calls` テーブルについて**: 秘密モードでも Tool 実行台帳を保持する。claim-before-execute・input hash 整合確認・成功結果の再利用・実行状態管理を通常 DB と同じく適用し、Secret Tool の入出力を通常 DB へ書き出さず Secret DB 内に閉じ込める。tool call block は `sessions.messages_json` にも包含されるため LLM context 復元には影響しない。

**Sleep / Pulse 関連テーブル不在の理由**: Sleep Batch・PULSE は `secret.db` にアクセスしない（構造的保証）。秘匿内容が長期記憶（`episodic.md` 等）へ昇格したり、公開チャネルで発言されたりするのを防ぐ。

### 5.3 マイグレーション

`run_migrations()` とは別に `run_secret_migrations()` を使用。`Database::new_secret()` 経由で起動時に呼ばれる。

```rust
pub(super) const SECRET_SCHEMA_VERSION: i64 = 4;
```

`egopulse.db` 側の `SCHEMA_VERSION` と衝突しないよう、別定数・別関数で管理する。

### 5.4 バックアップ

`secret.db` は `egopulse.db` と同一スケジュールでバックアップされる。

| 項目 | egopulse.db 系 | secret.db 系 |
|---|---|---|
| ファイル名 | `egopulse-YYYYMMDD-HHMMSS.db` | `secret-YYYYMMDD-HHMMSS.db` |
| 世代管理 | `max_generations` を独立に適用 | `max_generations` を独立に適用 |
| 起動時バックアップ | VACUUM INTO | VACUUM INTO |
| 定期バックアップ | interval_days ごと | interval_days ごと |

`secret.db` が存在しない環境ではバックアップも生成されない。

---

## 6. バックアップ・復元

### 6.1 バックアップ方式

EgoPulse は SQLite の `VACUUM INTO` コマンドで一貫性スナップショットを取得する。WAL モード稼働中でも schema lock を一瞬取得するだけであり、夜間実行なら書き込みブロックは無視できる。

### 6.2 バックアップのタイミング

- **起動時バックアップ**: マイグレーション前に1回だけ実行（既存 DB が存在する場合のみ）。最も危険な瞬間（スキーマ変更前）の保険。
- **定期バックアップ**: デフォルトで 7 日ごと 03:00（タイムゾーンは `Config.timezone`）。設定で間隔と時刻を変更可能。

詳細は [config.md §2.9](./config.md#29-db-バックアップ設定dbbackup) を参照。

### 6.3 保存先と命名規則

- **保存先**: `~/.egopulse/runtime/backups/`（本番 DB と同階層）
- **ファイル名**: `egopulse-YYYYMMDD-HHMMSS.db`（タイムゾーンは `Config.timezone`）
- **世代管理**: 直近 `max_generations` 件（デフォルト 12）を保持。超過分は古い順に削除。

### 6.4 復元手順

バックアップからの復元は手動で行う。稼働中のプロセスが DB を置き換えることは安全でないため、CLI は提供しない。

```bash
# 1. サービス停止
systemctl --user stop egopulse

# 2. バックアップから復元（WAL/SHM は削除して新規生成させる）
cp ~/.egopulse/runtime/backups/egopulse-YYYYMMDD-HHMMSS.db \
   ~/.egopulse/runtime/egopulse.db
rm -f ~/.egopulse/runtime/egopulse.db-wal \
      ~/.egopulse/runtime/egopulse.db-shm

# 3. サービス再開
systemctl --user start egopulse
```

### 6.5 ディスク故障対策

バックアップは本番 DB と同じディスクに保存される。ディスク故障に備えるには、別物理ディスクへ定期的に同期する運用をユーザー責任で行う。

```bash
# 例: rsync で別ディスクへ日次同期
rsync -av --delete ~/.egopulse/runtime/backups/ /mnt/external/egopulse-backups/

# または rclone でクラウドストレージへ
rclone sync ~/.egopulse/runtime/backups/ remote:egopulse-backups/
```

---
