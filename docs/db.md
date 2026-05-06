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
egopulse.db (SQLite / WAL mode)
├── db_meta             — スキーマバージョン管理（key-value）
├── schema_migrations   — マイグレーション適用履歴
├── chats               — チャットメタデータ・チャンネルアイデンティティ
├── messages            — メッセージ履歴
├── sessions            — セッションスナップショット（シリアライズ済み会話）
├── tool_calls          — ツール呼び出し記録
├── llm_usage_logs      — LLM API 使用量ログ
├── sleep_runs          — スリープバッチ実行履歴
└── memory_snapshots    — スリープ実行中のメモリファイル更新履歴
```

| 項目 | 値 |
|------|----|
| テーブル数 | 9（データテーブル 7 + マイグレーション基盤テーブル 2） |
| インデックス数 | 10 |
| 外部キー制約 | 1（tool_calls.chat_id → chats.chat_id） |
| スキーマバージョン管理 | バージョンベース（`SCHEMA_VERSION` 定数、現行 v5） |
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
│ chat_title       │       │ sender_name      │
│ chat_type        │       │ content          │
│ last_message_time│       │ is_from_bot      │
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
        │ trigger_type     │       │ phase            │
        │ started_at       │       │ file             │
        │ finished_at      │       │ content_before   │
        │ source_chats_json│       │ content_after    │
        │ source_digest_md │       │ created_at       │
        │ phases_json      │       └──────────────────┘
        │ summary_md       │
        │ input_tokens     │
        │ output_tokens    │
        │ total_tokens     │
        │ error_message    │
        └──────────────────┘

┌──────────────────┐       ┌──────────────────┐
│    db_meta       │       │ schema_migrations│
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
agent_id TEXT NOT NULL DEFAULT 'lyre'
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
| agent_id | TEXT | NOT NULL DEFAULT 'lyre' | エージェント識別子。エージェント単位の記憶読み込みやチャネル紐付けに使用 |

**操作**:
- `resolve_chat_id(channel, external_chat_id)` — 既存チャットの検索
- `resolve_or_create_chat_id(channel, external_chat_id, chat_title, chat_type)` — Upsert（`ON CONFLICT DO UPDATE`）
- `get_chat_by_id(chat_id)` — chat_id からチャンネル情報を逆引き
- `count_agent_messages_since(agent_id, since: Option<&str>)` — agent の新規メッセージ数をカウント（JOIN messages + chats）
- `get_agent_sessions_since(agent_id, since: Option<&str>, limit)` — agent のセッション一覧を updated_at 降順で取得（JOIN chats + sessions）。message_count と estimated_tokens（chars/3 近似）を含む

---

### messages

全チャンネルのメッセージ履歴。

```sql
CREATE TABLE IF NOT EXISTS messages (
    id TEXT NOT NULL,
    chat_id INTEGER NOT NULL,
    sender_name TEXT NOT NULL,
    content TEXT NOT NULL,
    is_from_bot INTEGER NOT NULL DEFAULT 0,
    timestamp TEXT NOT NULL,
    PRIMARY KEY (id, chat_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp
    ON messages(chat_id, timestamp);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK（複合） | プラットフォーム固有のメッセージID |
| chat_id | INTEGER | PK（複合） | chats.chat_id への参照 |
| sender_name | TEXT | NOT NULL | 送信者表示名 |
| content | TEXT | NOT NULL | メッセージ本文 |
| is_from_bot | INTEGER | NOT NULL DEFAULT 0 | ボット発言フラグ（0/1） |
| timestamp | TEXT | NOT NULL | RFC3339 タイムスタンプ |

**操作**:
- `store_message(msg)` — `INSERT OR REPLACE`
- `get_recent_messages(chat_id, limit)` — 最新N件（DESC→reverse）
- `get_all_messages(chat_id)` — 全件（ASC）

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
| updated_at | TEXT | NOT NULL | 楽観排他用タイムスタンプ |

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
| tool_output | TEXT | nullable | 出力結果（JSON） |
| timestamp | TEXT | NOT NULL | RFC3339 タイムスタンプ |

**操作**:
- `store_tool_call(tool_call)` — INSERT
- `update_tool_call_output(id, output)` — 出力の事後更新
- `update_tool_call_output_for_message(chat_id, message_id, id, output)` — assistant メッセージ単位でスコープした出力更新
- `get_tool_calls_for_message(chat_id, message_id)` — メッセージ単位の呼び出し履歴
- `get_tool_calls_for_chat(chat_id)` — チャット単位の全呼び出し履歴

**設計ポイント**:
- `id` は OpenAI/Codex などのプロバイダが返す call id であり、永続化上のグローバルIDではない
- 同じプロバイダ call id が別 assistant メッセージで再利用されても履歴を保持できるよう、主キーは `(id, chat_id, message_id)`

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
| created_at | TEXT | NOT NULL | RFC3339 タイムスタンプ |

**操作**:
- `log_llm_usage(chat_id, caller_channel, provider, model, input_tokens, output_tokens, request_kind)` — INSERT（total_tokens は自動計算）
- `get_llm_usage_summary(chat_id, since)` — 集計サマリ（requests, input/output/total tokens, last_request_at）
- `get_llm_usage_by_model(chat_id, since)` — モデル別集計（total_tokens 降順）

---

### sleep_runs

スリープバッチ（記憶整理処理）の実行履歴。

```sql
CREATE TABLE IF NOT EXISTS sleep_runs (
    id                  TEXT PRIMARY KEY,
    agent_id            TEXT NOT NULL,
    status              TEXT NOT NULL,
    trigger_type        TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    finished_at         TEXT,
    source_chats_json   TEXT NOT NULL DEFAULT '[]',
    source_digest_md    TEXT,
    phases_json         TEXT NOT NULL DEFAULT '[]',
    summary_md          TEXT,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER NOT NULL DEFAULT 0,
    error_message       TEXT
);

CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_started
    ON sleep_runs(agent_id, started_at);

CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_status
    ON sleep_runs(agent_id, status);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | UUID v4 |
| agent_id | TEXT | NOT NULL | エージェント識別子 |
| status | TEXT | NOT NULL | 実行状態（running/success/failed/skipped） |
| trigger_type | TEXT | NOT NULL | 起動トリガー（manual/scheduled） |
| started_at | TEXT | NOT NULL | 開始時刻（RFC3339） |
| finished_at | TEXT | nullable | 終了時刻（RFC3339） |
| source_chats_json | TEXT | NOT NULL DEFAULT '[]' | 対象チャットID一覧（JSON配列） |
| source_digest_md | TEXT | nullable | ソースダイジェスト（Markdown） |
| phases_json | TEXT | NOT NULL DEFAULT '[]' | 実行フェーズ一覧（JSON配列） |
| summary_md | TEXT | nullable | 実行サマリー（Markdown） |
| input_tokens | INTEGER | NOT NULL DEFAULT 0 | 入力トークン数 |
| output_tokens | INTEGER | NOT NULL DEFAULT 0 | 出力トークン数 |
| total_tokens | INTEGER | NOT NULL DEFAULT 0 | 合計トークン数 |
| error_message | TEXT | nullable | エラーメッセージ |

**操作**:
- `create_sleep_run(agent_id, trigger)` — INSERT（status=running, id/started_at 自動生成）
- `update_sleep_run_success(id, ...)` — status=success 更新
- `update_sleep_run_failed(id, error_message)` — status=failed 更新
- `update_sleep_run_skipped(id)` — status=skipped 更新
- `get_sleep_run(id)` — id で取得
- `list_sleep_runs(agent_id, limit)` — agent_id 絞り込み + started_at 降順
- `get_latest_successful_run(agent_id)` — success の最新1件。スリープ入力収集（Phase 3）のカットオフタイムスタンプ決定にも使用

**設計ポイント**:
- `trigger` は SQLite 予約語のため `trigger_type` にリネーム
- 外部キー制約なし（アプリケーション層で整合性担保）

---

### memory_snapshots

スリープ実行中のメモリファイル更新履歴。各フェーズにおけるファイルの before/after を記録。

```sql
CREATE TABLE IF NOT EXISTS memory_snapshots (
    id              TEXT PRIMARY KEY,
    run_id          TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    phase           TEXT NOT NULL,
    file            TEXT NOT NULL,
    content_before  TEXT NOT NULL,
    content_after   TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id
    ON memory_snapshots(run_id);

CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created
    ON memory_snapshots(agent_id, created_at);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | UUID v4 |
| run_id | TEXT | NOT NULL | sleep_runs.id への参照 |
| agent_id | TEXT | NOT NULL | エージェント識別子 |
| phase | TEXT | NOT NULL | 実行フェーズ（pruning/consolidation/compression） |
| file | TEXT | NOT NULL | 対象ファイル（episodic/semantic/prospective） |
| content_before | TEXT | NOT NULL | 更新前のファイル内容 |
| content_after | TEXT | NOT NULL | 更新後のファイル内容 |
| created_at | TEXT | NOT NULL | 作成時刻（RFC3339） |

**操作**:
- `create_memory_snapshot(run_id, agent_id, phase, file, content_before, content_after)` — INSERT
- `get_snapshots_for_run(run_id)` — run_id 絞り込み + created_at 昇順
- `get_snapshots_for_agent(agent_id, limit)` — agent_id 絞り込み + created_at 降順
- `get_latest_snapshot_for_file(agent_id, file)` — agent+file の最新1件

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
| `StoredMessage` | messages | id, chat_id, sender_name, content, is_from_bot, timestamp |
| `ChatInfo` | chats（一部） | chat_id, channel, external_chat_id, chat_type, agent_id |
| `SessionSummary` | chats + messages（JOIN） | chat_id, channel, surface_thread, chat_title, last_message_time, last_message_preview, agent_id |
| `SessionSnapshot` | sessions + messages | messages_json, updated_at, recent_messages: Vec\<StoredMessage\> |
| `AgentSessionInfo` | chats + sessions（JOIN） | chat_id, channel, external_chat_id, updated_at, message_count, estimated_tokens |
| `ToolCall` | tool_calls | id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp |
| `LlmUsageSummary` | llm_usage_logs（集計） | requests, input_tokens, output_tokens, total_tokens, last_request_at |
| `LlmModelUsageSummary` | llm_usage_logs（モデル別集計） | model, requests, input_tokens, output_tokens, total_tokens |
| `SleepRun` | sleep_runs | id, agent_id, status, trigger, started_at, finished_at, source_chats_json, source_digest_md, phases_json, summary_md, input_tokens, output_tokens, total_tokens, error_message |
| `MemorySnapshot` | memory_snapshots | id, run_id, agent_id, phase, file, content_before, content_after, created_at |

---

## 4. 設計上の注意点

### マイグレーション機構

バージョンベースのインクリメンタルマイグレーションを採用。

**仕組み**:
1. `Database::new()` → `run_migrations(conn)` を呼び出し
2. `schema_version(conn)` で `db_meta` テーブルから現在のバージョンを取得（未設定時は `0`）
3. `if version < N` ブロックで未適用のマイグレーションを逐次実行
4. 各マイグレーション適用後に `set_schema_version(conn, N, "note")` でバージョンを更新し `schema_migrations` に履歴を記録
5. `SCHEMA_VERSION` 定数（現行 `5`）に到達したら完了。`debug_assert_eq!` で検証

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
//         "ALTER TABLE chats ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'lyre';",
//     )?;
//     set_schema_version_in_tx(&tx, 4, "add NOT NULL agent_id to chats (default: lyre)")?;
//     version = 4;
// }
```

**特徴**:
- 外部ファイル（SQL マイグレーションファイル）なし。DDL は Rust コードに直接埋め込み
- 外部クレート（refinery, sqlx 等）への依存なし
- 再起動時は適用済みバージョンまでスキップされる（冪等）

### 外部キー制約が最小限

明示的な FK は `tool_calls.chat_id` のみ。`messages.chat_id` や `sessions.chat_id` には FK がない。整合性はアプリケーション層で担保。

### CASCADE なし

`ON DELETE` が一切定義されていない。チャット削除時に messages / sessions / tool_calls を手動でクリーンアップする必要がある。

---
