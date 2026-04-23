# Microclaw DB Schema — Memory テーブル群

> LLM の長期記憶の抽出・保存・検索・注入・置換を管理する5テーブル。

## ER 図

```
┌──────────────────┐
│    memories      │
│──────────────────│
│ id (PK)          │───┐
│ chat_id          │   │
│ content          │   │    ┌────────────────────────┐
│ category         │   │    │ memory_supersede_edges │
│ confidence       │   └───│────────────────────────│
│ source           │        │ from_memory_id         │
│ last_seen_at     │        │ to_memory_id           │
│ is_archived      │        │ reason                 │
│ archived_at      │        └────────────────────────┘
│ chat_channel     │
│ external_chat_id │
│ embedding_model  │
└──────────────────┘

┌────────────────────────┐     ┌────────────────────────┐
│ memory_reflector_state │     │ memory_reflector_runs  │
│────────────────────────│     │────────────────────────│
│ chat_id (PK)           │     │ id (PK)                │
│ last_reflected_ts      │     │ chat_id                │
│ updated_at             │     │ started_at             │
└────────────────────────┘     │ finished_at           │
                               │ extracted/inserted/   │
┌────────────────────────┐     │  updated/skipped_count│
│ memory_injection_logs  │     │ dedup_method          │
│────────────────────────│     │ parse_ok              │
│ id (PK)                │     │ error_text            │
│ chat_id                │     └────────────────────────┘
│ retrieval_method       │
│ candidate/selected/    │
│  omitted_count         │
│ tokens_est             │
└────────────────────────┘
```

---

## memories

構造化された知識ストレージ。信頼度スコアリング・アーカイブによるライフサイクル管理付き。

```sql
CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER,                       -- NULL = グローバル
    content TEXT NOT NULL,
    category TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    embedding_model TEXT,                  -- v2
    confidence REAL NOT NULL DEFAULT 0.70, -- v2
    source TEXT NOT NULL DEFAULT 'legacy', -- v2
    last_seen_at TEXT NOT NULL,            -- v2
    is_archived INTEGER NOT NULL DEFAULT 0,-- v2
    archived_at TEXT,                      -- v2
    chat_channel TEXT,                     -- v2
    external_chat_id TEXT                  -- v2
);

CREATE INDEX IF NOT EXISTS idx_memories_chat ON memories(chat_id);
CREATE INDEX IF NOT EXISTS idx_memories_active_updated
    ON memories(is_archived, updated_at);
CREATE INDEX IF NOT EXISTS idx_memories_confidence ON memories(confidence);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | INTEGER | PK (auto) | メモリID |
| chat_id | INTEGER | nullable | チャット固有の記憶。NULL = グローバル記憶 |
| content | TEXT | NOT NULL | 記憶の本文 |
| category | TEXT | NOT NULL | カテゴリ分類 |
| created_at | TEXT | NOT NULL | 作成日時 |
| updated_at | TEXT | NOT NULL | 更新日時 |
| embedding_model | TEXT | nullable | 埋め込みモデル名 |
| confidence | REAL | NOT NULL DEFAULT 0.70 | 信頼度スコア（0.0〜1.0） |
| source | TEXT | NOT NULL DEFAULT 'legacy' | 生成元（legacy, reflector 等） |
| last_seen_at | TEXT | NOT NULL | 最後に参照された日時 |
| is_archived | INTEGER | NOT NULL DEFAULT 0 | アーカイブ済みフラグ（ソフトデリート） |
| archived_at | TEXT | nullable | アーカイブ日時 |
| chat_channel | TEXT | nullable | チャンネル識別子 |
| external_chat_id | TEXT | nullable | 外部チャットID |

**設計ポイント**:
- **スコープ**: `chat_id` が NULL の場合はグローバル記憶、それ以外はチャット固有の記憶
- **ソフトデリート**: `is_archived` で論理削除。物理削除しないことで監査証跡を残す
- **信頼度**: `confidence` スコアで、より確実な記憶を優先的にコンテキストに注入
- **ライフサイクル**: `last_seen_at` で参照の鮮度を追跡。長期間参照されていない記憶は信頼度を下げる等の運用が可能

---

## memory_reflector_state

バックグラウンドメモリ抽出（Reflector）の進捗管理。

```sql
CREATE TABLE IF NOT EXISTS memory_reflector_state (
    chat_id INTEGER PRIMARY KEY,
    last_reflected_ts TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| chat_id | INTEGER | PK | 対象チャット |
| last_reflected_ts | TEXT | NOT NULL | 前回抽出完了時刻 |
| updated_at | TEXT | NOT NULL | レコード更新日時 |

**目的**: 各チャットで最後にメモリ抽出を実行した時点を記録し、差分抽出（新規メッセージのみ処理）を実現。

---

## memory_reflector_runs

メモリ抽出の実行ログ。各 Reflector パスの成功・失敗を詳細に記録。

```sql
CREATE TABLE IF NOT EXISTS memory_reflector_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    extracted_count INTEGER NOT NULL DEFAULT 0,
    inserted_count INTEGER NOT NULL DEFAULT 0,
    updated_count INTEGER NOT NULL DEFAULT 0,
    skipped_count INTEGER NOT NULL DEFAULT 0,
    dedup_method TEXT NOT NULL,
    parse_ok INTEGER NOT NULL DEFAULT 1,
    error_text TEXT
);

CREATE INDEX IF NOT EXISTS idx_memory_reflector_runs_chat_started
    ON memory_reflector_runs(chat_id, started_at);
```

| カラム | 型 | 説明 |
|--------|----|------|
| id | INTEGER PK | 実行ID |
| chat_id | INTEGER | 対象チャット |
| started_at / finished_at | TEXT | 実行時間帯 |
| extracted_count | INTEGER | LLMが抽出した記憶候補数 |
| inserted_count | INTEGER | 新規挿入数 |
| updated_count | INTEGER | 既存記憶の更新数 |
| skipped_count | INTEGER | スキップ数（重複等） |
| dedup_method | TEXT | 重複排除手法 |
| parse_ok | INTEGER | LLM出力のパース成功フラグ |
| error_text | TEXT | エラー詳細 |

---

## memory_injection_logs

コンテキストへの記憶注入の追跡。どの手法で何件の記憶を注入したかを記録。

```sql
CREATE TABLE IF NOT EXISTS memory_injection_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    retrieval_method TEXT NOT NULL,
    candidate_count INTEGER NOT NULL DEFAULT 0,
    selected_count INTEGER NOT NULL DEFAULT 0,
    omitted_count INTEGER NOT NULL DEFAULT 0,
    tokens_est INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_memory_injection_logs_chat_created
    ON memory_injection_logs(chat_id, created_at);
```

| カラム | 型 | 説明 |
|--------|----|------|
| retrieval_method | TEXT | 検索手法（semantic, keyword, hybrid 等） |
| candidate_count | INTEGER | 候補として抽出された記憶数 |
| selected_count | INTEGER | 実際に注入された数 |
| omitted_count | INTEGER | トークン制限等で除外された数 |
| tokens_est | INTEGER | 推定消費トークン数 |

---

## memory_supersede_edges

記憶の置換関係グラフ。新しい記憶が古い記憶を置き換えたことを記録。

```sql
CREATE TABLE IF NOT EXISTS memory_supersede_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_memory_id INTEGER NOT NULL,
    to_memory_id INTEGER NOT NULL,
    reason TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_supersede_from
    ON memory_supersede_edges(from_memory_id, created_at);
CREATE INDEX IF NOT EXISTS idx_memory_supersede_to
    ON memory_supersede_edges(to_memory_id, created_at);
```

| カラム | 型 | 説明 |
|--------|----|------|
| from_memory_id | INTEGER | 置換元（新しい記憶） |
| to_memory_id | INTEGER | 置換先（古い記憶） |
| reason | TEXT | 置換理由 |

**設計ポイント**:
- 有向グラフとして管理。新しい記憶が古い記憶を指す
- インデックスで両方向からのトラバーサルを高速化
- 記憶の進化（訂正・更新）を追跡可能
