# EgoPulse DB Schema — 現状

> ソース: `egopulse/src/storage.rs`

## 全体構成

```
egopulse.db (SQLite / WAL mode)
├── db_meta             — スキーマバージョン管理（key-value）
├── schema_migrations   — マイグレーション適用履歴
├── chats               — チャットメタデータ・チャンネルアイデンティティ
├── messages            — メッセージ履歴
├── sessions            — セッションスナップショット（シリアライズ済み会話）
├── tool_calls          — ツール呼び出し記録
└── llm_usage_logs      — LLM API 使用量ログ
```

| 項目 | 値 |
|------|----|
| テーブル数 | 7（データテーブル 5 + マイグレーション基盤テーブル 2） |
| インデックス数 | 6 |
| 外部キー制約 | 1（tool_calls.chat_id → chats.chat_id） |
| スキーマバージョン管理 | バージョンベース（`SCHEMA_VERSION` 定数、現行 v2） |
| DBライブラリ | rusqlite 0.37（bundled） |
| DBファイル | `{data_dir}/egopulse.db` |
| 接続ラッパー | `Mutex<Connection>` |
| PRAGMA | `journal_mode=WAL`, `busy_timeout=5s` |

---

## ER 図

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
                │ id (PK)          │
                │ message_id       │
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
│    db_meta       │       │ schema_migrations│
│──────────────────│       │──────────────────│
│ key (PK)         │       │ version (PK)     │
│ value            │       │ applied_at       │
└──────────────────┘       │ note             │
                           └──────────────────┘
```

---

## テーブル定義

### chats

チャットメタデータとチャンネル横断のアイデンティティマッピング。

```sql
CREATE TABLE IF NOT EXISTS chats (
    chat_id INTEGER PRIMARY KEY,
    chat_title TEXT,
    chat_type TEXT NOT NULL DEFAULT 'private',
    last_message_time TEXT NOT NULL,
    channel TEXT,
    external_chat_id TEXT
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

**操作**:
- `resolve_chat_id(channel, external_chat_id)` — 既存チャットの検索
- `resolve_or_create_chat_id(channel, external_chat_id, chat_title, chat_type)` — Upsert（`ON CONFLICT DO UPDATE`）
- `get_chat_by_id(chat_id)` — chat_id からチャンネル情報を逆引き

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
    id TEXT PRIMARY KEY,
    chat_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    tool_input TEXT NOT NULL,
    tool_output TEXT,
    timestamp TEXT NOT NULL,
    FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
    ON tool_calls(chat_id);

CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
    ON tool_calls(chat_id, message_id);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | TEXT | PK | ツール呼び出しID |
| chat_id | INTEGER | NOT NULL, FK | chats.chat_id |
| message_id | TEXT | NOT NULL | 対象メッセージID |
| tool_name | TEXT | NOT NULL | ツール/ファンクション名 |
| tool_input | TEXT | NOT NULL | 入力パラメータ（JSON） |
| tool_output | TEXT | nullable | 出力結果（JSON） |
| timestamp | TEXT | NOT NULL | RFC3339 タイムスタンプ |

**操作**:
- `store_tool_call(tool_call)` — INSERT
- `update_tool_call_output(id, output)` — 出力の事後更新
- `get_tool_calls_for_message(chat_id, message_id)` — メッセージ単位の呼び出し履歴
- `get_tool_calls_for_chat(chat_id)` — チャット単位の全呼び出し履歴

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

## Rust 構造体マッピング

| 構造体 | テーブル | フィールド |
|--------|----------|-----------|
| `StoredMessage` | messages | id, chat_id, sender_name, content, is_from_bot, timestamp |
| `ChatInfo` | chats（一部） | chat_id, channel, external_chat_id, chat_type |
| `SessionSummary` | chats + messages（JOIN） | chat_id, channel, surface_thread, chat_title, last_message_time, last_message_preview |
| `SessionSnapshot` | sessions + messages | messages_json, updated_at, recent_messages: Vec\<StoredMessage\> |
| `ToolCall` | tool_calls | id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp |
| `LlmUsageSummary` | llm_usage_logs（集計） | requests, input_tokens, output_tokens, total_tokens, last_request_at |
| `LlmModelUsageSummary` | llm_usage_logs（モデル別集計） | model, requests, input_tokens, output_tokens, total_tokens |

---

## 設計上の注意点

### マイグレーション機構

バージョンベースのインクリメンタルマイグレーションを採用。

**仕組み**:
1. `Database::new()` → `run_migrations(conn)` を呼び出し
2. `schema_version(conn)` で `db_meta` テーブルから現在のバージョンを取得（未設定時は `0`）
3. `if version < N` ブロックで未適用のマイグレーションを逐次実行
4. 各マイグレーション適用後に `set_schema_version(conn, N, "note")` でバージョンを更新し `schema_migrations` に履歴を記録
5. `SCHEMA_VERSION` 定数（現行 `2`）に到達したら完了。`debug_assert_eq!` で検証

**新規マイグレーションの追加手順**:
1. `SCHEMA_VERSION` 定数をインクリメント（例: `1` → `2`）
2. `run_migrations()` に `if version < 2 { ... }` ブロックを追加
3. ブロック内で `conn.execute_batch("ALTER TABLE ...")` 等の DDL を実行
4. `set_schema_version(conn, 2, "description")` を呼び出し

```rust
// 既存のテンプレート（storage.rs 内）
// if version < 2 {
//     conn.execute_batch(
//         "CREATE TABLE IF NOT EXISTS llm_usage_logs (
//             id INTEGER PRIMARY KEY AUTOINCREMENT,
//             chat_id INTEGER NOT NULL,
//             caller_channel TEXT NOT NULL,
//             provider TEXT NOT NULL,
//             model TEXT NOT NULL,
//             input_tokens INTEGER NOT NULL,
//             output_tokens INTEGER NOT NULL,
//             total_tokens INTEGER NOT NULL,
//             request_kind TEXT NOT NULL DEFAULT 'agent_loop',
//             created_at TEXT NOT NULL
//         );
//
//         CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created
//             ON llm_usage_logs(chat_id, created_at);
//
//         CREATE INDEX IF NOT EXISTS idx_llm_usage_created
//             ON llm_usage_logs(created_at);",
//     )?;
//     set_schema_version(conn, 2, "add llm_usage_logs table for LLM usage tracking")?;
//     version = 2;
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

## Microclaw との差分サマリ

| 観点 | EgoPulse（現状） | Microclaw（v19） |
|------|------------------|------------------|
| テーブル数 | 7（データ5 + マイグレーション基盤2） | 24 |
| マイグレーション | バージョンベース（v1→v2） | バージョンベース（v1→v19） |
| セッション設定 | messages_json のみ | label, thinking_level, verbose_level, reasoning_level, skill_envs_json, fork |
| メモリ/知識管理 | なし | memories + reflector/injection/supersede（5テーブル） |
| タスクスケジューリング | なし | scheduled_tasks + run_logs + dlq（3テーブル） |
| 認証・認可 | なし（静的トークンのみ） | auth + api_keys + scopes（4テーブル） |
| オブザーバビリティ | llm_usage_logs（1テーブル） | audit_logs + metrics + llm_usage（3テーブル） |
| サブエージェント | なし | runs + announces + events + focus（4テーブル） |
| ツール呼び出し記録 | tool_calls（独立テーブル） | sessions.messages_json 内に埋め込み |
