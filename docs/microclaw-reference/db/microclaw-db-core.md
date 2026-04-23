# Microclaw DB Schema — Core テーブル

> 会話の永続化・再開に必要な基本3テーブル。

## ER 図

```
┌──────────────┐       ┌──────────────────┐
│    chats     │1    * │    messages      │
│──────────────│───────│──────────────────│
│ chat_id (PK) │       │ (id,chat_id) PK  │
│ channel      │       │ sender_name      │
│ external_    │       │ content          │
│  chat_id     │       │ is_from_bot      │
│ chat_title   │       │ timestamp        │
│ chat_type    │       └──────────────────┘
│ last_message │
│  _time       │       ┌──────────────────┐
│              │1    1 │    sessions      │
│              │───────│──────────────────│
└──────────────┘       │ chat_id (PK)     │
                       │ messages_json    │
                       │ updated_at       │
                       │ label            │
                       │ thinking_level   │
                       │ verbose_level    │
                       │ reasoning_level  │
                       │ skill_envs_json  │
                       │ parent_session   │
                       │  _key            │
                       │ fork_point       │
                       └──────────────────┘
```

---

## chats

チャットメタデータとチャンネル横断のアイデンティティマッピング。

```sql
CREATE TABLE IF NOT EXISTS chats (
    chat_id INTEGER PRIMARY KEY,
    chat_title TEXT,
    chat_type TEXT NOT NULL DEFAULT 'private',
    last_message_time TEXT NOT NULL,
    channel TEXT,                          -- v2 追加
    external_chat_id TEXT                  -- v2 追加
);

CREATE INDEX IF NOT EXISTS idx_chats_channel_external
    ON chats(channel, external_chat_id);
CREATE INDEX IF NOT EXISTS idx_chats_channel_title
    ON chats(channel, chat_title);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| chat_id | INTEGER | PK (auto) | 内部ID |
| chat_title | TEXT | nullable | 表示名 |
| chat_type | TEXT | NOT NULL DEFAULT 'private' | チャット種別 |
| last_message_time | TEXT | NOT NULL | 最終メッセージ時刻（RFC3339） |
| channel | TEXT | nullable | チャンネル識別子（`cli`, `web`, `discord`, `telegram`） |
| external_chat_id | TEXT | nullable | 外部プラットフォームのチャットID |

**設計ポイント**:
- 内部PK（chat_id）と外部アイデンティティ（channel + external_chat_id）の二層構造
- `last_message_time` でソート可能にするため非NULL
- Upsert パターン: `ON CONFLICT(channel, external_chat_id) DO UPDATE SET`

---

## messages

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

**設計ポイント**:
- 複合PK `(id, chat_id)` により、プラットフォーム間で id が重複しても安全
- `(chat_id, timestamp)` インデックスで時系列クエリを高速化

---

## sessions

再開可能なセッション状態。LLM の会話コンテキスト全体をシリアライズして格納。

```sql
CREATE TABLE IF NOT EXISTS sessions (
    chat_id INTEGER PRIMARY KEY,
    messages_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    label TEXT,                            -- v6
    thinking_level TEXT,                   -- v6
    verbose_level TEXT,                    -- v6
    reasoning_level TEXT,                  -- v6
    skill_envs_json TEXT,                  -- v11
    parent_session_key TEXT,               -- v6
    fork_point INTEGER                     -- v6
);

CREATE INDEX IF NOT EXISTS idx_sessions_parent_session_key
    ON sessions(parent_session_key);
```

| カラム | 型 | 制約 | 追加バージョン | 説明 |
|--------|----|------|--------------|------|
| chat_id | INTEGER | PK | v1 | chats.chat_id と1:1 |
| messages_json | TEXT | NOT NULL | v1 | シリアライズされた `Vec<Message>` |
| updated_at | TEXT | NOT NULL | v1 | 楽観排他用タイムスタンプ |
| label | TEXT | nullable | v6 | セッション表示ラベル |
| thinking_level | TEXT | nullable | v6 | LLM thinking レベル設定 |
| verbose_level | TEXT | nullable | v6 | 出力の詳細度設定 |
| reasoning_level | TEXT | nullable | v6 | 推論レベル設定 |
| skill_envs_json | TEXT | nullable | v11 | スキル環境変数のJSON |
| parent_session_key | TEXT | nullable | v6 | フォーク元セッションのキー |
| fork_point | INTEGER | nullable | v6 | フォーク位置（メッセージインデックス） |

**設計ポイント**:
- **LLM設定の永続化**: セッションごとに thinking/reasoning/verbose レベルを保持。再開時に前回設定を復元
- **セッションフォーク**: `parent_session_key` + `fork_point` で、過去の特定時点から分岐したセッションを作成可能
- **楽観排他**: `updated_at` で同時書き込みを検出
- `messages_json` にはツール呼び出しブロック等も含まれるため、セッション再開時に完全なコンテキストを復元可能
