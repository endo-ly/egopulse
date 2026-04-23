# Microclaw DB Schema — Auth テーブル群

> WebUI / API のアクセス制御。パスワード認証・セッション管理・APIキー（スコープ付き）。

## ER 図

```
┌──────────────────┐       ┌──────────────────┐
│ auth_passwords   │       │ auth_sessions    │
│──────────────────│       │──────────────────│
│ id (PK, CHECK=1) │       │ session_id (PK)  │
│ password_hash    │       │ label            │
│ created_at       │       │ created_at       │
│ updated_at       │       │ expires_at       │
└──────────────────┘       │ last_seen_at     │
                           │ revoked_at       │
                           └──────────────────┘

┌──────────────────┐       ┌──────────────────┐
│ api_keys         │1    * │ api_key_scopes   │
│──────────────────│───────│──────────────────│
│ id (PK)          │       │ (api_key_id,     │
│ label            │       │  scope) PK       │
│ key_hash (UNIQUE)│       └──────────────────┘
│ prefix           │
│ created_at       │
│ revoked_at       │
│ last_used_at     │
│ expires_at       │
│ rotated_from_    │
│  key_id          │
└──────────────────┘
```

---

## auth_passwords

WebUI オペレーターのパスワード。単一行テーブル。

```sql
CREATE TABLE IF NOT EXISTS auth_passwords (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| id | INTEGER | PK CHECK(id=1) | 常に1。単一行を強制 |
| password_hash | TEXT | NOT NULL | ハッシュ化されたパスワード |
| created_at / updated_at | TEXT | NOT NULL | 作成・更新日時 |

**設計ポイント**: `CHECK(id = 1)` で物理的に1行しか挿入できない。シングルトンテーブル。

---

## auth_sessions

WebUI のセッション管理。

```sql
CREATE TABLE IF NOT EXISTS auth_sessions (
    session_id TEXT PRIMARY KEY,
    label TEXT,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_auth_sessions_expires
    ON auth_sessions(expires_at);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| session_id | TEXT | PK | セッションID（ランダム文字列） |
| label | TEXT | nullable | セッションラベル（ブラウザ等） |
| created_at | TEXT | NOT NULL | セッション作成日時 |
| expires_at | TEXT | NOT NULL | 有効期限 |
| last_seen_at | TEXT | NOT NULL | 最終アクセス日時 |
| revoked_at | TEXT | nullable | 失効日時（NULL = 有効） |

**設計ポイント**:
- `expires_at` インデックスで期限切れセッションのクリーンアップを高速化
- `revoked_at` で明示的なログアウト（無効化）を管理

---

## api_keys

APIキー管理。ハッシュ保存・有効期限・キーローテーション対応。

```sql
CREATE TABLE IF NOT EXISTS api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    label TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    last_used_at TEXT,
    expires_at TEXT,                         -- v8
    rotated_from_key_id INTEGER              -- v8
);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| label | TEXT | NOT NULL | キーの表示名 |
| key_hash | TEXT | UNIQUE | キーのハッシュ値（平文は保存しない） |
| prefix | TEXT | NOT NULL | キーのプレフィックス（表示用） |
| revoked_at | TEXT | nullable | 失効日時 |
| last_used_at | TEXT | nullable | 最終使用日時 |
| expires_at | TEXT | nullable | 有効期限（v8） |
| rotated_from_key_id | INTEGER | nullable | ローテーション元のキーID（v8） |

**設計ポイント**:
- **ハッシュ保存**: 平文キーは保存せず、ハッシュのみ保持
- **プレフィックス**: UI で `abc_****xyz` のように表示するための先頭文字
- **ローテーション**: `rotated_from_key_id` で新旧キーの関係を追跡

---

## api_key_scopes

APIキーのスコープ（権限）。多対多リレーション。

```sql
CREATE TABLE IF NOT EXISTS api_key_scopes (
    api_key_id INTEGER NOT NULL,
    scope TEXT NOT NULL,
    PRIMARY KEY (api_key_id, scope)
);

CREATE INDEX IF NOT EXISTS idx_api_key_scopes_scope
    ON api_key_scopes(scope);
```

| カラム | 型 | 制約 | 説明 |
|--------|----|------|------|
| api_key_id | INTEGER | PK（複合） | api_keys.id |
| scope | TEXT | PK（複合） | スコープ名（例: `chat:read`, `chat:write`, `admin`） |

**設計ポイント**: スコープごとのインデックスで「特定スコープを持つ全キー」の検索を高速化。
